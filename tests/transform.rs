//! Golden parity test: replays request-transform fixtures under
//! `tests/fixtures/` and asserts the Rust transforms produce canonically equal
//! output (or the same error). The fixtures are a frozen parity snapshot,
//! originally captured from the reference middleware transforms the Rust port
//! replaced; they now serve as a regression guard on the Rust transforms.

use private_ai_gateway::middleware::request_transform::{
    responses_to_chat_params, transform_to_provider_request, Endpoint,
};
use private_ai_gateway::middleware::response_transform::{
    openai_chat_to_responses, transform_response,
};
use private_ai_gateway::middleware::types::{Engine, ProviderFormat};
use serde_json::Value;

const FIXTURES: &str = include_str!("fixtures/transform_golden.json");
const RESPONSE_FIXTURES: &str = include_str!("fixtures/response_golden.json");
const RESPONSES_FIXTURES: &str = include_str!("fixtures/responses_golden.json");

fn parse_format(value: &str) -> ProviderFormat {
    match value {
        "openai" => ProviderFormat::Openai,
        "anthropic" => ProviderFormat::Anthropic,
        other => panic!("unknown format {other}"),
    }
}

fn parse_endpoint(value: &str) -> Endpoint {
    match value {
        "chatComplete" => Endpoint::ChatComplete,
        "complete" => Endpoint::Complete,
        "embed" => Endpoint::Embed,
        "messages" => Endpoint::Messages,
        "createModelResponse" => Endpoint::CreateModelResponse,
        other => panic!("unknown endpoint {other}"),
    }
}

fn parse_engine(value: &Value) -> Option<Engine> {
    match value.as_str() {
        Some("sglang") => Some(Engine::Sglang),
        Some("vllm") => Some(Engine::Vllm),
        _ => None,
    }
}

// Structural equality with number comparison by value, so an integer-valued
// float (e.g. Node `2`) matches a Rust integer `2`. Object key order is ignored;
// array order is significant.
fn canonical_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(xf), Some(yf)) => xf == yf,
            _ => x == y,
        },
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| canonical_eq(x, y))
        }
        (Value::Object(xo), Value::Object(yo)) => {
            xo.len() == yo.len()
                && xo
                    .iter()
                    .all(|(k, xv)| yo.get(k).map(|yv| canonical_eq(xv, yv)).unwrap_or(false))
        }
        _ => a == b,
    }
}

#[test]
fn rust_request_transforms_match_node_fixtures() {
    let cases: Vec<Value> = serde_json::from_str(FIXTURES).expect("parse fixtures");
    assert!(!cases.is_empty(), "no fixtures found");

    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let format = parse_format(case["format"].as_str().unwrap());
        let endpoint = parse_endpoint(case["fn"].as_str().unwrap());
        let engine = parse_engine(&case["engine"]);
        let input = &case["input"];

        let result = transform_to_provider_request(format, input, endpoint, engine);

        if case.get("error").and_then(Value::as_bool) == Some(true) {
            assert!(
                result.is_err(),
                "case {name}: expected an error from the transform"
            );
            continue;
        }

        let output = result.unwrap_or_else(|err| panic!("case {name}: unexpected error: {err}"));
        let expected = &case["output"];
        assert!(
            canonical_eq(&output, expected),
            "case {name}: Rust output does not match Node fixture\n  rust: {output}\n  node: {expected}"
        );
    }
}

// Drop non-deterministic timestamps injected by response transforms.
fn strip_created(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(strip_created).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "created" | "created_at"))
                .map(|(k, v)| (k.clone(), strip_created(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[test]
fn rust_response_transforms_match_node_fixtures() {
    let cases: Vec<Value> =
        serde_json::from_str(RESPONSE_FIXTURES).expect("parse response fixtures");
    assert!(!cases.is_empty(), "no response fixtures found");

    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let format = parse_format(case["format"].as_str().unwrap());
        let endpoint = parse_endpoint(case["fn"].as_str().unwrap());
        let input = case["input"].clone();

        let output = strip_created(&transform_response(format, endpoint, input));
        let expected = &case["output"];
        assert!(
            canonical_eq(&output, expected),
            "case {name}: Rust response output does not match Node fixture\n  rust: {output}\n  node: {expected}"
        );
    }
}

#[test]
fn responses_requests_match_golden_fixtures() {
    let fixture: Value = serde_json::from_str(RESPONSES_FIXTURES).expect("parse fixtures");
    let cases = fixture["requests"].as_array().expect("request fixtures");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let result = responses_to_chat_params(&case["input"]);
        if case.get("error").and_then(Value::as_bool) == Some(true) {
            assert!(result.is_err(), "case {name}: expected an error");
            continue;
        }
        let output = result.unwrap_or_else(|err| panic!("case {name}: unexpected error: {err}"));
        assert!(
            canonical_eq(&output, &case["output"]),
            "case {name}: output mismatch\n  rust: {output}\n  expected: {}",
            case["output"]
        );
    }
}

#[test]
fn chat_responses_match_golden_fixtures() {
    let fixture: Value = serde_json::from_str(RESPONSES_FIXTURES).expect("parse fixtures");
    let cases = fixture["responses"].as_array().expect("response fixtures");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let output = strip_created(&openai_chat_to_responses(
            case["input"].clone(),
            &case["echo"],
        ));
        assert!(
            canonical_eq(&output, &case["output"]),
            "case {name}: output mismatch\n  rust: {output}\n  expected: {}",
            case["output"]
        );
    }
}
