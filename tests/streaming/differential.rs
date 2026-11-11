//! Seeded compatibility run through the same full-pipeline recording harness.
use super::*;
use rand::{rngs::StdRng, Rng, SeedableRng};

const PATTERNS: [&str; 30] = [
    "valid",
    "no_terminal",
    "partial",
    "unknown_name",
    "extension",
    "missing_type",
    "conflicting_types",
    "error_first",
    "rate_limit",
    "error_middle",
    "error_after_terminal",
    "usage_after_error",
    "usage_after_terminal",
    "non_json",
    "non_json_after_terminal",
    "malformed_json",
    "empty_data",
    "multiline",
    "transport",
    "terminal_transport",
    "crlf",
    "pending",
    "comments",
    "repeat_failure",
    "reset_field",
    "reset_data",
    "reset_line",
    "eof_lf",
    "eof_crlf",
    "eof_cr",
];

fn sources(kind: &str, pattern: &str) -> (String, bool) {
    let usage = json!({"prompt_tokens":2,"completion_tokens":3,"total_tokens":5,"provider_trace":"PRIVATE_UPSTREAM_DETAIL"});
    let (start, content, terminal, failure, rate, usage_event, unknown, missing, conflict) = match kind {
        "messages" => (
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"usage\":{\"input_tokens\":2}}}\n\n".into(),
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n".into(),
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".into(),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\n".into(),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Too many requests\"}}\n\n".into(),
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n".into(),
            "event: start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"usage\":{\"input_tokens\":2}}}\n\n".into(),
            "event: message_start\ndata: {\"message\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"usage\":{\"input_tokens\":2}}}\n\n".into(),
            "event: message_stop\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"usage\":{\"input_tokens\":2}}}\n\n".into(),
        ),
        "responses" => {
            let response = json!({"id":"upstream-id","model":"upstream-model","status":"completed","output":[],"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5,"provider_trace":"PRIVATE_UPSTREAM_DETAIL"},"provider_trace":"PRIVATE_UPSTREAM_DETAIL"});
            let event = |name: &str, payload: Value| format!("event: {name}\ndata: {payload}\n\n");
            let terminal = json!({"type":"response.completed","sequence_number":3,"response":response});
            (
                event("response.in_progress", json!({"type":"response.in_progress","sequence_number":0,"response":{"id":"upstream-id","model":"upstream-model","status":"in_progress","output":[],"provider_trace":"PRIVATE_UPSTREAM_DETAIL"}})),
                event("response.output_text.delta", json!({"type":"response.output_text.delta","sequence_number":1,"delta":"hi","provider_trace":"PRIVATE_UPSTREAM_DETAIL"})),
                event("response.completed", terminal.clone()),
                event("response.failed", json!({"type":"response.failed","sequence_number":3,"response":{"id":"upstream-id","model":"upstream-model","status":"failed","output":[],"error":{"code":"server_error","message":"PRIVATE_UPSTREAM_DETAIL"},"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5,"provider_trace":"PRIVATE_UPSTREAM_DETAIL"}}})),
                event("response.error", json!({"type":"response.error","sequence_number":2,"error":{"code":"rate_limit_exceeded","message":"Too many requests"}})),
                format!("data: {{\"usage\":{usage}}}\n\n"),
                event("lifecycle", json!({"type":"response.in_progress","response":response})),
                event("response.in_progress", json!({"response":response})),
                event("response.in_progress", terminal),
            )
        }
        "legacy" => (
            String::new(),
            "data: {\"completion\":\"hi\",\"model\":\"upstream-model\",\"log_id\":\"upstream-id\",\"stop_reason\":null}\n\n".into(),
            "data: {\"completion\":\"\",\"model\":\"upstream-model\",\"log_id\":\"upstream-id\",\"stop_reason\":\"stop_sequence\"}\n\ndata: [DONE]\n\n".into(),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\n".into(),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Too many requests\"}}\n\n".into(),
            format!("data: {{\"usage\":{usage}}}\n\n"),
            "event: extension\ndata: {\"completion\":\"hi\"}\n\n".into(),
            "data: {\"completion\":\"hi\"}\n\n".into(),
            "event: message_stop\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"private\"}}\n\n".into(),
        ),
        _ => {
            let content = json!({"id":"upstream-id","object":"chat.completion.chunk","created":1700000000,"model":"upstream-model","choices":[{"index":0,"delta":{"content":"hi"}}],"usage":usage,"provider_trace":"PRIVATE_UPSTREAM_DETAIL"});
            (
                String::new(), format!("data: {content}\n\n"), "data: [DONE]\n\n".into(),
                format!("data: {{\"error\":{{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"}},\"usage\":{usage}}}\n\n"),
                format!("data: {{\"error\":{{\"code\":429,\"type\":\"rate_limit_error\",\"message\":\"Too many requests\"}},\"usage\":{usage}}}\n\n"),
                format!("data: {{\"choices\":[],\"usage\":{usage}}}\n\n"),
                format!("event: extension\ndata: {content}\n\n"),
                format!("data: {content}\n\n"),
                "event: message_stop\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"private\"}}\n\n".into(),
            )
        }
    };
    let valid = format!("{start}{content}{terminal}");
    let source = match pattern {
        "valid" | "terminal_transport" => valid,
        "no_terminal" | "transport" => format!("{start}{content}"),
        "partial" | "reset_data" => format!("{start}{content}data: {{\"unfinished\":"),
        "reset_field" => format!("{start}{content}dat"),
        "reset_line" => format!("{start}{content}data: {{\"unfinished\":true}}\n"),
        "unknown_name" => format!("{unknown}{content}{terminal}"),
        "extension" => format!("{start}event: extension\ndata: {{\"extra\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\n{terminal}"),
        "missing_type" => format!("{missing}{terminal}"),
        "conflicting_types" => format!("{conflict}{terminal}"),
        "error_first" => format!("{failure}{terminal}"),
        "rate_limit" => format!("{rate}{terminal}"),
        "error_middle" => format!("{start}{content}{failure}{terminal}"),
        "error_after_terminal" => format!("{valid}{failure}"),
        "usage_after_error" => format!("{failure}{usage_event}{terminal}"),
        "usage_after_terminal" => format!("{valid}{usage_event}"),
        "non_json" => format!("data: not JSON\n\n{terminal}"),
        "non_json_after_terminal" => format!("{valid}data: not JSON\n\n"),
        "malformed_json" => format!("data: {{\"bad\":\n\n{terminal}"),
        "empty_data" => format!("data:\n\n{terminal}"),
        "multiline" => format!("data: {{\"usage\":\ndata: {usage}}}\n\n{terminal}"),
        "crlf" => valid.replace('\n', "\r\n"),
        "pending" => valid.trim_end_matches('\n').to_string() + "\n",
        "comments" => format!(": upstream thinking\n\n{valid}"),
        "repeat_failure" => format!("{failure}{failure}{terminal}"),
        "eof_lf" => valid.trim_end_matches('\n').to_string() + "\n",
        "eof_crlf" => (valid.trim_end_matches('\n').to_string() + "\n").replace('\n', "\r\n"),
        "eof_cr" => (valid.trim_end_matches('\n').to_string() + "\n").replace('\n', "\r"),
        _ => unreachable!(),
    };
    (
        source,
        matches!(
            pattern,
            "transport" | "terminal_transport" | "reset_field" | "reset_data" | "reset_line"
        ),
    )
}

fn cases() -> Vec<Case> {
    let mut rng = StdRng::seed_from_u64(56_320_261_111);
    let mut cases = Vec::new();
    for (kind, destination, endpoint) in [
        ("chat", "chat", Endpoint::ChatComplete),
        ("chat", "messages", Endpoint::Messages),
        ("chat", "responses", Endpoint::CreateModelResponse),
        ("chat", "complete", Endpoint::Complete),
        ("messages", "chat", Endpoint::ChatComplete),
        ("messages", "messages", Endpoint::Messages),
        ("messages", "responses", Endpoint::CreateModelResponse),
        ("legacy", "complete", Endpoint::Complete),
        ("responses", "responses", Endpoint::CreateModelResponse),
    ] {
        for pattern in PATTERNS {
            let alternate = [2, 3, 7, 13, 64, 4096][rng.gen_range(0..6)];
            for split in [1, alternate] {
                let prefix = if kind == "responses" {
                    "native_responses_diff"
                } else {
                    "diff"
                };
                let name = format!("{prefix}_{kind}_{destination}_{pattern}_{split}");
                let (source, transport) = sources(kind, pattern);
                let mut case = Case::new(
                    &name,
                    endpoint,
                    &source,
                    if matches!(kind, "messages" | "legacy") {
                        "anthropic"
                    } else {
                        "openai"
                    },
                );
                case.chunk_size = split;
                case.transport_error = transport;
                cases.push(case);
            }
        }
    }
    cases
}

#[tokio::test]
async fn seeded_wire_differential() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let baseline_path = root.join("tests/fixtures/streaming_differential_main.jsonl");
    let mut actual = Vec::new();
    for case in cases() {
        actual.push(capture(&case).await);
    }
    let lines = actual
        .iter()
        .map(|v| serde_json::to_string(v).unwrap() + "\n")
        .collect::<String>();
    if std::env::var_os("UPDATE_STREAMING_DIFFERENTIAL").is_some() {
        std::fs::write(baseline_path, lines).unwrap();
        return;
    }
    if std::env::var_os("RECORD_STREAMING_DIFFERENTIAL").is_some() {
        let path = root.join("target/eof-review");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("generated_actual.jsonl"), lines).unwrap();
    }
    let main = std::fs::read_to_string(baseline_path).unwrap();
    let baseline = main
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let corrections: Value = serde_json::from_slice(
        &std::fs::read(root.join("tests/fixtures/streaming_differential_corrections.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(baseline.len(), actual.len());
    let mut corrected = 0;
    for (old, new) in baseline.iter().zip(&actual) {
        assert_eq!(old["name"], new["name"]);
        let name = new["name"].as_str().unwrap();
        if name.starts_with("diff_messages_")
            && (name.contains("unknown_name") || name.contains("missing_type"))
        {
            assert_eq!(
                old["reports"], new["reports"],
                "payload-schema usage: {name}"
            );
        }
        let expected = if let Some(correction) = corrections.get(new["name"].as_str().unwrap()) {
            assert!(!correction["reason"].as_str().unwrap().is_empty());
            corrected += 1;
            &correction["expected"]
        } else {
            old
        };
        assert_eq!(new, expected, "case {}", new["name"]);
    }
    for snapshots in [&baseline, &actual] {
        for pair in snapshots.as_chunks::<2>().0 {
            let mut first = pair[0].clone();
            let mut second = pair[1].clone();
            first.as_object_mut().unwrap().remove("name");
            second.as_object_mut().unwrap().remove("name");
            assert_eq!(first, second, "chunk splits: {}", pair[0]["name"]);
        }
    }
    assert_eq!(corrected, corrections.as_object().unwrap().len());
    eprintln!(
        "differential: {} cases, {} exact, {corrected} approved corrections",
        actual.len(),
        actual.len() - corrected
    );
}
