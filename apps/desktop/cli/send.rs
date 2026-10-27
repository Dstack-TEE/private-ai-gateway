//! `private-ai-proxy send`: one verified chat completion, end to end.
//!
//! Verifies the service (fail closed), sends the prompt over an
//! SPKI-pinned connection capturing the exact wire bytes, then fetches
//! and verifies the receipt (spec 9.3) and the session it cites (9.2).

use std::io::IsTerminal;

use crate::aci::types::{PROVIDER_ACI_SESSION_IDS, PROVIDER_ACI_VERIFIED};
use desktop_core::sse::DataLines;
use serde_json::{json, Value};

use crate::args::SendArgs;
use crate::checks::{
    fetch_live_session, parse_receipt_document, run_response_checks, BodyDigest, UpstreamContext,
};
use crate::client::HttpResult;
use crate::renewal::{receipt_identity as select_receipt_identity, EstablishedMember};
use crate::verify::{verify_service, ServiceVerification};
use crate::{print_json, safe, safe_multiline, write_stdout};

const DEFAULT_PROMPT: &str = "Say hello and name the model serving this request.";

pub async fn run(args: SendArgs) -> Result<i32, String> {
    let bearer = api_key(&args)?;
    let policy = args.policy.verifier_policy()?;
    let verification = verify_service(&args.base_url, None, &policy, false).await?;
    let base_url = verification.base_url.clone();
    run_verified(args, bearer, verification, || {
        verify_service(&base_url, None, &policy, false)
    })
    .await
}

async fn run_verified<F>(
    args: SendArgs,
    bearer: Option<String>,
    verification: ServiceVerification,
    refresh: impl FnOnce() -> F,
) -> Result<i32, String>
where
    F: std::future::Future<Output = Result<ServiceVerification, crate::verify::VerifyError>>,
{
    if !args.json {
        write_stdout(
            format!(
                "== service verification: {} ==\n{}\n",
                safe(&verification.base_url),
                verification.transcript.render_human()
            )
            .as_bytes(),
        )?;
    }
    if !verification.transcript.verified() {
        if args.json {
            print_json(&verification.transcript.to_json())?;
        }
        return Err("service verification failed; not sending the prompt (fail closed)".into());
    }

    let ServiceVerification {
        mut transcript,
        report,
        identity,
        client,
        base_url,
        host,
        tls_pins: pins,
        ..
    } = verification;

    // Enforce the just-verified TLS keys on every further connection to this host.
    client.pin(&host, &pins)?;

    let model = match &args.model {
        Some(model) => model.clone(),
        None => first_model(&client, &base_url, bearer.as_deref()).await?,
    };
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_PROMPT.to_string());
    let stream = !args.no_stream;
    let request_body = chat_request(&args, &model, &prompt, stream)?;

    if !args.json {
        write_stdout(
            format!(
                "model:  {}\nprompt: {}\nreply:  ",
                safe(&model),
                safe(&prompt)
            )
            .as_bytes(),
        )?;
    }
    let mut collector = SseTextCollector::new(stream && !args.json);
    let response = client
        .post_chat_captured(
            &base_url,
            bearer.as_deref(),
            request_body.clone(),
            |chunk| {
                if stream {
                    collector.feed(chunk)?;
                }
                Ok(())
            },
        )
        .await?;
    collector.finish()?;
    if let Err(e) = response.error_for_status("chat completion") {
        // The `reply:  ` prefix was already printed (no newline); close the line
        // so the error does not glue onto it as if it were the reply.
        if !args.json {
            write_stdout(b"\n")?;
        }
        return Err(e);
    }

    let response_text = if stream {
        collector.text
    } else {
        buffered_response_text(&response)
    };
    if !args.json {
        if !stream {
            write_stdout(safe(&response_text).as_bytes())?;
        }
        write_stdout(b"\n\n")?;
    }

    let receipt_id = receipt_id(&response, args.allow_unverified && stream)?;
    let receipt_resp = client
        .fetch_receipt(&base_url, &receipt_id, bearer.as_deref())
        .await?;
    receipt_resp.error_for_status("receipt fetch")?;
    let receipt = parse_receipt_document(receipt_resp.json()?)?;

    // id-2 already established the identity; a verified run always carries it.
    let identity = identity.ok_or("verified run carried no established identity")?;
    let admitted = EstablishedMember::new(report, identity);
    let receipt_identity = if let Some(identity) =
        select_receipt_identity(&admitted.report, std::slice::from_ref(&admitted), &receipt)
    {
        identity
    } else {
        // As in serve, attest once more without resending the inference.
        // A same-family replica authorizes only digests actually established.
        let refreshed = refresh().await?;
        if !refreshed.transcript.verified() {
            return Err("service re-verification did not reach VERIFIED".into());
        }
        let identity = refreshed
            .identity
            .ok_or("verified run carried no established identity")?;
        let member = EstablishedMember::new(refreshed.report, identity);
        let selected =
            select_receipt_identity(&admitted.report, std::slice::from_ref(&member), &receipt)
                .ok_or("receipt digest was not established for the admitted renewal family")?;
        transcript = refreshed.transcript;
        selected
    };
    // Audit the cited session (§9.2): its id is the hash of the parsed
    // document's JCS form, so any served encoding verifies.
    let (session_resp, no_session_reason) = fetch_live_session(&client, &base_url, &receipt).await;
    run_response_checks(
        &mut transcript,
        &receipt,
        &receipt_identity.identity,
        Some(&BodyDigest::of(&request_body)),
        Some(&BodyDigest::of(&response.body)),
        UpstreamContext {
            session_bytes: session_resp.as_ref().map(|resp| resp.body.as_slice()),
            no_session_reason: &no_session_reason,
            pinned: (!args.sessions.is_empty()).then_some(args.sessions.as_slice()),
            // §5.3: pinning sessions implies verified serving.
            requires_verified: !args.allow_unverified || !args.sessions.is_empty(),
            serving: &receipt_identity.report.service_capabilities.serving,
            required_claims: &args.require_claims,
        },
    );

    if args.json {
        let mut output = transcript.to_json();
        output["model"] = json!(model);
        output["receipt_id"] = json!(receipt_id);
        output["request_body"] = json!(String::from_utf8_lossy(&request_body));
        output["response_text"] = json!(response_text);
        print_json(&output)?;
    } else {
        write_stdout(
            format!(
                "== receipt verification: {} ==\n{}",
                safe(&receipt_id),
                transcript.render_human()
            )
            .as_bytes(),
        )?;
    }
    Ok(if transcript.verified() { 0 } else { 1 })
}

/// The bearer key: piped to stdin, the deprecated `--api-key`, or `ACI_API_KEY`.
fn api_key(args: &SendArgs) -> Result<Option<String>, String> {
    if args.api_key_stdin {
        if std::io::stdin().is_terminal() {
            return Err("Refusing to read the API key from a terminal with --api-key-stdin; pipe it in or set ACI_API_KEY.".into());
        }
        return crate::read_api_key(std::io::stdin()).map(Some);
    }
    if let Some(key) = &args.api_key {
        // Like the `aci` alias note, never mixed into JSON-mode stderr.
        if !args.json {
            tracing::warn!("warning: --api-key exposes the key to other local processes and will be removed in 0.3; use --api-key-stdin or ACI_API_KEY.");
        }
        return Ok(Some(key.clone()));
    }
    Ok(std::env::var("ACI_API_KEY").ok())
}

/// The chat completion request. §5.3 serving constraints ride in the body;
/// the service consumes and strips the member before forwarding. Verified
/// serving is the default — `--allow-unverified` drops the demand.
fn chat_request(
    args: &SendArgs,
    model: &str,
    prompt: &str,
    stream: bool,
) -> Result<Vec<u8>, String> {
    let mut body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": stream,
    });
    if let Some(max_tokens) = args.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if !args.allow_unverified || !args.sessions.is_empty() {
        let mut provider = serde_json::Map::new();
        if !args.allow_unverified {
            provider.insert(PROVIDER_ACI_VERIFIED.to_string(), json!(true));
        }
        if !args.sessions.is_empty() {
            provider.insert(PROVIDER_ACI_SESSION_IDS.to_string(), json!(args.sessions));
        }
        body["provider"] = Value::Object(provider);
    }
    serde_json::to_vec(&body).map_err(|e| format!("failed to serialize request body: {e}"))
}

/// The response's receipt id. A streaming response the aggregator committed
/// before selecting an upstream (pre-first-byte keep-alive, §5.2) carries no
/// X-Receipt-Id header; the receipt still exists once the stream finalized
/// and is retrievable by the response's own id. That is only legitimate for
/// an unverified stream (`early_commit`) — a constrained request (the
/// default) is never committed early, so a missing header there is a §5.2
/// violation this command must keep surfacing, not paper over.
fn receipt_id(response: &HttpResult, early_commit: bool) -> Result<String, String> {
    let header = response
        .headers
        .get("x-receipt-id")
        .and_then(|value| value.to_str().ok());
    match header {
        Some(id) => Ok(id.to_string()),
        None if early_commit => response_chat_id(&response.body).ok_or_else(|| {
            "response carried no X-Receipt-Id header and no readable response id".into()
        }),
        None => Err("response carried no X-Receipt-Id header".into()),
    }
}

async fn first_model(
    client: &crate::client::AciClient,
    base_url: &str,
    bearer: Option<&str>,
) -> Result<String, String> {
    let resp = client.fetch_models(base_url, bearer).await?;
    resp.error_for_status("GET /v1/models")?;
    resp.json()?["data"][0]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "GET /v1/models returned no models; pass --model".to_string())
}

/// The response's own `id`, from a buffered JSON body or the first SSE data
/// event that carries one. The receipt endpoint accepts it as a lookup key.
fn response_chat_id(body: &[u8]) -> Option<String> {
    let id = |data: &[u8]| {
        serde_json::from_slice::<Value>(data)
            .ok()
            .and_then(|value| value["id"].as_str().map(str::to_string))
    };
    if let Some(id) = id(body) {
        return Some(id);
    }
    let mut found = None;
    let mut lines = DataLines::new();
    let mut first = |data: &str| {
        if found.is_none() {
            found = id(data.as_bytes());
        }
    };
    lines.push(body, &mut first);
    lines.finish(&mut first);
    found
}

fn buffered_response_text(response: &HttpResult) -> String {
    serde_json::from_slice::<Value>(&response.body)
        .ok()
        .and_then(|value| {
            value["choices"][0]["message"]["content"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&response.body).into_owned())
}

/// Collects the streamed `choices[0].delta.content` text across chunk
/// boundaries, and optionally echoes it as it arrives. The wire bytes
/// themselves are captured separately, untouched.
struct SseTextCollector {
    lines: DataLines,
    text: String,
    echo: bool,
}

impl SseTextCollector {
    fn new(echo: bool) -> Self {
        Self {
            lines: DataLines::new(),
            text: String::new(),
            echo,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<(), String> {
        self.collect_with(Some(chunk), write_stdout)
    }

    fn collect_with(
        &mut self,
        chunk: Option<&[u8]>,
        mut output: impl FnMut(&[u8]) -> Result<bool, String>,
    ) -> Result<(), String> {
        let (text, echo) = (&mut self.text, &mut self.echo);
        let mut result = Ok(());
        let append = |data: &str| {
            if result.is_ok() {
                result = append_delta(text, echo, data, &mut output);
            }
        };
        match chunk {
            Some(chunk) => self.lines.push(chunk, append),
            None => self.lines.finish(append),
        }
        result
    }

    fn finish(&mut self) -> Result<(), String> {
        self.collect_with(None, write_stdout)
    }
}

/// Appends an event's content delta; `[DONE]` and other events carry none.
fn append_delta(
    text: &mut String,
    echo: &mut bool,
    data: &str,
    output: &mut impl FnMut(&[u8]) -> Result<bool, String>,
) -> Result<(), String> {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Ok(());
    };
    if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
        text.push_str(delta);
        if *echo {
            *echo = output(safe_multiline(delta).as_bytes())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{response_chat_id, SseTextCollector};

    #[test]
    fn chat_request_includes_max_tokens_only_when_set() {
        use crate::args::Command;
        use clap::{FromArgMatches, Subcommand};

        let matches = Command::augment_subcommands(clap::Command::new("pap"))
            .try_get_matches_from(["pap", "send", "https://example.com"])
            .unwrap();
        let Command::Send(args) = Command::from_arg_matches(&matches).unwrap() else {
            panic!("expected send command");
        };
        let original = br#"{"model":"demo-model","messages":[{"role":"user","content":"hi"}],"stream":true,"provider":{"aci_verified":true}}"#;
        assert_eq!(
            super::chat_request(&args, "demo-model", "hi", true).unwrap(),
            original
        );

        let matches = Command::augment_subcommands(clap::Command::new("pap"))
            .try_get_matches_from(["pap", "send", "https://example.com", "--max-tokens", "8"])
            .unwrap();
        let Command::Send(args) = Command::from_arg_matches(&matches).unwrap() else {
            panic!("expected send command");
        };
        let body = super::chat_request(&args, "demo-model", "hi", true).unwrap();
        let mut body: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(body.remove("max_tokens"), Some(serde_json::json!(8)));
        assert_eq!(serde_json::to_vec(&body).unwrap(), original);
    }

    async fn send_receipt_fixture(
        refreshed_report: crate::aci::types::AttestationReport,
        digest: String,
    ) -> Result<i32, String> {
        use crate::aci::receipt::receipt_signing_input;
        use crate::checks::{established_identity, BodyDigest};
        use crate::spec_fixtures::{
            vector_receipt_envelope, vector_report, vector_session_bytes, RESPONSE_BODY,
        };
        use crate::transcript::{Transcript, ID_1, ID_2, ID_3, ID_4, ID_5, ID_6};
        use crate::verify::ServiceVerification;
        use axum::{
            routing::{get, post},
            Json, Router,
        };
        use ed25519_dalek::{Signer, SigningKey};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let initial_report = vector_report();
        let receipt = Arc::new(std::sync::Mutex::new(vector_receipt_envelope()));
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post({
                    let receipt = receipt.clone();
                    let calls = calls.clone();
                    move |body: axum::body::Bytes| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let mut receipt = receipt.lock().unwrap();
                        receipt["workload_keyset_digest"] = serde_json::json!(digest);
                        receipt["event_log"][0]["body_hash"] =
                            serde_json::json!(BodyDigest::of(&body).sha256);
                        receipt["event_log"][1]["body_hash"] =
                            receipt["event_log"][0]["body_hash"].clone();
                        let signature = SigningKey::from_bytes(&[2; 32])
                            .sign(&receipt_signing_input(&receipt).unwrap());
                        receipt["signature"] = serde_json::json!(hex::encode(signature.to_bytes()));
                        async { ([("x-receipt-id", "rcpt-0001")], RESPONSE_BODY) }
                    }
                }),
            )
            .route(
                "/v1/aci/receipts/{id}",
                get({
                    let receipt = receipt.clone();
                    move || {
                        let receipt = receipt.lock().unwrap().clone();
                        async { Json(receipt) }
                    }
                }),
            )
            .route(
                "/v1/aci/sessions/{id}",
                get(|| async { vector_session_bytes() }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let verified = |report| {
            let mut transcript = Transcript::default();
            for check in [ID_1, ID_2, ID_3, ID_4, ID_5, ID_6] {
                transcript.pass(check, "freshly verified fixture");
            }
            ServiceVerification {
                transcript,
                identity: Some(established_identity(&report).unwrap()),
                report,
                client: crate::client::AciClient::new().unwrap(),
                base_url: base_url.clone(),
                host: crate::client::host_of(&base_url).unwrap(),
                observed_spki: None,
                tls_pins: vec!["c0".repeat(32)],
            }
        };
        let args = crate::args::SendArgs {
            base_url: base_url.clone(),
            policy: Default::default(),
            model: Some("demo-model".into()),
            prompt: Some("hi".into()),
            max_tokens: None,
            api_key_stdin: false,
            api_key: None,
            no_stream: true,
            json: true,
            allow_unverified: true,
            require_claims: Vec::new(),
            sessions: Vec::new(),
        };
        let refreshed = verified(refreshed_report);
        let result = super::run_verified(args, None, verified(initial_report), || async {
            Ok(refreshed)
        })
        .await;
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        result
    }

    fn renewed_report() -> crate::aci::types::AttestationReport {
        let mut report = crate::spec_fixtures::vector_report();
        report.attestation.workload_keyset["not_after"] =
            serde_json::json!(crate::spec_fixtures::KEYSET_NOT_AFTER + 86_400);
        report.workload_keyset_digest = crate::checks::established_identity(&report)
            .unwrap()
            .keyset_digest;
        report
    }

    #[tokio::test]
    async fn send_resolves_a_renewal_receipt_without_repeating_inference() {
        let report = renewed_report();
        let digest = report.workload_keyset_digest.clone();
        assert_eq!(send_receipt_fixture(report, digest).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn send_rejects_an_unseen_replica_digest_after_a_successful_refresh() {
        let error = send_receipt_fixture(
            crate::spec_fixtures::vector_report(),
            renewed_report().workload_keyset_digest,
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("receipt digest was not established"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn send_rejects_a_receipt_from_a_different_renewal_family() {
        let changes: [fn(&mut crate::aci::types::AttestationReport); 3] = [
            |report| report.attestation.workload_keyset["subject"] = serde_json::json!("other"),
            |report| report.attestation.source_provenance.repo_commit = Some("other".into()),
            |report| {
                report.attestation.workload_keyset["e2ee_public_keys"][0]["public_key"] =
                    serde_json::json!("00".repeat(32))
            },
        ];
        for (change, mutate) in changes.into_iter().enumerate() {
            let mut report = renewed_report();
            mutate(&mut report);
            report.workload_keyset_digest = crate::checks::established_identity(&report)
                .unwrap()
                .keyset_digest;
            let digest = report.workload_keyset_digest.clone();
            let error = send_receipt_fixture(report, digest).await.unwrap_err();
            assert!(
                error.contains("receipt digest was not established"),
                "{change}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn streamed_reply_surfaces_writer_errors_and_only_silences_closed_pipes() {
        for final_newline in [false, true] {
            let body = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"hello\"}}}}]}}\n\n\
                 data: {{\"choices\":[{{\"delta\":{{\"content\":\" world\"}}}}]}}{}",
                if final_newline { "\n\n" } else { "" }
            );
            let app = axum::Router::new().route(
                "/v1/chat/completions",
                axum::routing::post(move || {
                    let body = body.clone();
                    async move { body }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            for closed_pipe in [false, true] {
                let mut collector = SseTextCollector::new(true);
                let error = std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected output failure",
                )
                .to_string();
                let mut writes = 0;
                let mut output = |_: &[u8]| {
                    writes += 1;
                    if closed_pipe {
                        Ok(false)
                    } else if writes == 1 {
                        Ok(true)
                    } else {
                        Err(error.clone())
                    }
                };
                let result = crate::client::AciClient::new()
                    .unwrap()
                    .post_chat_captured(&base, None, b"{}".to_vec(), |chunk| {
                        collector.collect_with(Some(chunk), &mut output)
                    })
                    .await
                    .and_then(|_| collector.collect_with(None, &mut output));
                assert_eq!(writes, if closed_pipe { 1 } else { 2 });
                if closed_pipe {
                    result.unwrap();
                    assert_eq!(collector.text, "hello world");
                } else {
                    assert_eq!(result.unwrap_err(), error);
                }
            }
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
        }
    }

    #[test]
    fn reads_the_response_id_from_json_or_the_first_event_carrying_one() {
        assert_eq!(
            response_chat_id(br#"{"id":"chat-1"}"#).as_deref(),
            Some("chat-1")
        );
        assert_eq!(
            response_chat_id(
                b": keep-alive\n\ndata: {\"id\":\"chat-2\"}\r\n\ndata: {\"id\":\"chat-3\"}\n"
            )
            .as_deref(),
            Some("chat-2")
        );
        assert_eq!(response_chat_id(b"data: [DONE]\n"), None);
    }

    #[test]
    fn collects_deltas_across_chunk_boundaries() {
        let mut c = SseTextCollector::new(false);
        c.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choi")
            .unwrap();
        c.feed(b"ces\":[{\"delta\":{\"content\":\"lo\"}}]}\n\ndata: [DONE]\n\n")
            .unwrap();
        c.finish().unwrap();
        assert_eq!(c.text, "Hello");
    }

    #[test]
    fn split_escape_sequences_in_reply_deltas_are_inert_but_captured_exactly() {
        let mut collector = SseTextCollector::new(true);
        let mut rendered = Vec::new();
        for delta in [
            "hello\u{1b}[",
            "31mred\u{1b}]8;;https://example.com",
            "\u{7}link\r\u{202e}text\n\t",
        ] {
            let event = serde_json::json!({"choices": [{"delta": {"content": delta}}]});
            collector
                .collect_with(Some(format!("data: {event}\n\n").as_bytes()), |bytes| {
                    rendered.extend_from_slice(bytes);
                    Ok(true)
                })
                .unwrap();
        }
        collector.finish().unwrap();
        assert_eq!(
            String::from_utf8(rendered).unwrap(),
            "hello [31mred ]8;;https://example.com link  text\n\t"
        );
        assert_eq!(
            collector.text,
            "hello\u{1b}[31mred\u{1b}]8;;https://example.com\u{7}link\r\u{202e}text\n\t"
        );
    }

    #[test]
    fn handles_no_space_data_prefix_and_crlf() {
        let mut c = SseTextCollector::new(false);
        c.feed(b"data:{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\r\n")
            .unwrap();
        c.finish().unwrap();
        assert_eq!(c.text, "hi");
    }
}
