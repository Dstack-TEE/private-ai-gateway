//! 0G verifier against a mock router, chain RPC and brokers. Quotes are the
//! real compute-network-19 quote with report_data patched; DCAP-to-root is
//! stubbed.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::super::zero_g::{PreviewCandidate, ZeroGProviderVerifier};
use crate::aci::receipt::{ChannelBinding, VerificationResult};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

const MODEL: &str = "glm-5.3";
const QUOTE_HEX: &str =
    include_str!("../../../../tests/fixtures/0g-private-computer/quote-cn19.hex");
const REPORT_DATA: usize = 568;
const RTMR2: usize = 472;

/// The compute-network-19 quote's boot chain (dstack-nvidia-0.5.4.1).
const BOOT_CHAIN: &str =
    "tdx-boot-chain:sha256:536a92e7c6204087b2c2910e4c17083b7a9f8b3aa455b6bc14c6df3df2f1a7fc";

/// Candidates: `a` is sound; `i` is sound under 0G's two model ids
/// (`canonical_id` `0gm-1.0-35b-a3b`, `model_id` `0GM-1.0-35B-A3B`); each other
/// letter breaks one check.
const NAMES: [&str; 9] = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];

/// What the router offers for `name`: (canonical_id, model_id).
fn offered(name: &str) -> (&'static str, &'static str) {
    match name {
        "i" => ("0gm-1.0-35b-a3b", "0GM-1.0-35B-A3B"),
        _ => (MODEL, MODEL),
    }
}

fn candidate(name: &str) -> PreviewCandidate {
    let (canonical_id, model_id) = offered(name);
    PreviewCandidate {
        address: address(name),
        provider_identity: None,
        canonical_id: canonical_id.into(),
        model_id: model_id.into(),
    }
}

fn address(name: &str) -> String {
    format!("0x{}", hex::encode([name.as_bytes()[0]; 20]))
}

fn signer(name: &str) -> String {
    format!("0x{}", hex::encode([name.as_bytes()[0] ^ 0xff; 20]))
}

fn word(value: usize) -> Vec<u8> {
    let mut word = vec![0; 32];
    word[24..].copy_from_slice(&(value as u64).to_be_bytes());
    word
}

/// ABI-encode a `getService` return for `name`.
fn service_record(origin: &str, name: &str) -> String {
    // `d` serves a model other than the one the router names for it.
    let model = if name == "d" {
        "glm-5"
    } else {
        offered(name).1
    };
    let separated = name == "b";
    let strings = [
        "chatbot".to_string(),
        format!("{origin}/{name}"),
        model.to_string(),
        "TeeML".to_string(),
        format!(r#"{{"TargetSeparated":{separated}}}"#),
    ];
    let (mut head, mut tail) = (word(0), Vec::new());
    for (slot, value) in [1, 2, 6, 7, 8].into_iter().zip(&strings) {
        while head.len() < slot * 32 {
            head.extend(word(0));
        }
        head.extend(word(11 * 32 + tail.len()));
        tail.extend(word(value.len()));
        tail.extend(value.as_bytes());
        tail.resize(tail.len().div_ceil(32) * 32, 0);
    }
    head.extend([vec![0; 12], hex::decode(&signer(name)[2..]).unwrap()].concat());
    head.extend(word((name != "c") as usize));
    format!("0x{}", hex::encode([word(32), head, tail].concat()))
}

fn quote(name: &str) -> Value {
    let mut quote = hex::decode(QUOTE_HEX.trim()).unwrap();
    let quote_signer = signer(if name == "e" { "x" } else { name });
    let mut report_data = vec![name.as_bytes()[0]; 32];
    report_data.extend(hex::decode(&quote_signer[2..]).unwrap());
    report_data.extend([0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    quote[REPORT_DATA..REPORT_DATA + 64].copy_from_slice(&report_data);
    if name == "f" {
        quote[RTMR2] ^= 1;
    }
    json!({"quote": hex::encode(quote)})
}

/// Stubbed DCAP-to-root: provider `h`'s quote is refused.
fn quote_status(raw: &[u8]) -> Result<String, String> {
    match raw[REPORT_DATA] {
        b'h' => Err("collateral rejected the quote".to_string()),
        _ => Ok("UpToDate".to_string()),
    }
}

#[derive(Default)]
struct Mock {
    origin: String,
    preview: Mutex<Vec<&'static str>>,
    preview_requests: Mutex<Vec<(HeaderMap, Value)>>,
}

async fn preview(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    mock.preview_requests.lock().unwrap().push((headers, body));
    let providers: Vec<Value> = mock
        .preview
        .lock()
        .unwrap()
        .iter()
        .map(|name| {
            let (canonical_id, model_id) = offered(name);
            json!({"address": address(name), "canonical_id": canonical_id, "model_id": model_id})
        })
        .collect();
    Json(json!({"providers": providers}))
}

async fn rpc(State(mock): State<Arc<Mock>>, Json(call): Json<Value>) -> Json<Value> {
    let data = call["params"][0]["data"].as_str().unwrap();
    let name = NAMES
        .into_iter()
        .find(|name| data.ends_with(&address(name)[2..]))
        .unwrap();
    Json(json!({"jsonrpc": "2.0", "id": 1, "result": service_record(&mock.origin, name)}))
}

async fn broker_quote(Path(name): Path<String>) -> Response {
    if name == "g" {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "/a/v1/quote")],
        )
            .into_response();
    }
    Json(quote(&name)).into_response()
}

async fn fixture() -> (
    Arc<Mock>,
    ZeroGProviderVerifier,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mock = Arc::new(Mock {
        origin: origin.clone(),
        ..Default::default()
    });
    let app = Router::new()
        .route("/v1/routing/preview", post(preview))
        .route("/rpc", post(rpc))
        .route("/:name/v1/quote", get(broker_quote))
        .with_state(mock.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let verifier = ZeroGProviderVerifier::new(
        origin.clone(),
        "zg-test-key",
        format!("{origin}/rpc"),
        "0x47340d900bdfec2bd393c626e12ea0656f938d84",
        None,
        5,
        300,
    )
    .unwrap()
    .with_accepted_subjects([BOOT_CHAIN.to_string()])
    .with_quote_status(quote_status);
    (mock, verifier, server)
}

fn request() -> UpstreamVerificationRequest {
    UpstreamVerificationRequest {
        upstream_name: "zero-g".into(),
        url_origin: None,
        model_id: MODEL.into(),
        forwarded_body_hash: String::new(),
        required: true,
    }
}

#[tokio::test]
async fn rejects_each_unsafe_candidate_before_sealing() {
    let (_, verifier, server) = fixture().await;
    for (name, expected) in [
        ("b", "TargetSeparated"),
        ("c", "not acknowledged"),
        ("d", "serves model"),
        ("e", "is not the on-chain teeSignerAddress"),
        ("f", "boot chain"),
        ("g", "307"),
        ("h", "collateral rejected"),
    ] {
        let err = verifier
            .verify_candidate(&candidate(name), MODEL)
            .await
            .expect_err(name);
        assert!(err.contains(expected), "{name}: {err}");
    }
    server.abort();
}

#[tokio::test]
async fn model_identity_follows_0g_canonical_and_provider_ids() {
    let (_, verifier, server) = fixture().await;
    // The configured model is the router's canonical_id; the on-chain model is
    // the provider's model_id, which the backend seals. Both compare exactly.
    let verified = verifier
        .verify_candidate(&candidate("i"), "0gm-1.0-35b-a3b")
        .await
        .unwrap();
    assert_eq!(verified.provider.model, "0gm-1.0-35b-a3b");
    assert_eq!(verified.provider.model_id, "0GM-1.0-35B-A3B");
    let verified = verifier
        .verify_candidate(&candidate("a"), MODEL)
        .await
        .unwrap();
    assert_eq!(verified.provider.model_id, MODEL);

    for model in ["0GM-1.0-35B-A3B", "0gm-1.0-35b"] {
        let err = verifier
            .verify_candidate(&candidate("i"), model)
            .await
            .unwrap_err();
        assert!(err.contains("canonical model"), "{model}: {err}");
    }
    server.abort();
}

#[tokio::test]
async fn binds_only_verified_candidates_and_fails_closed_without_one() {
    let (mock, verifier, server) = fixture().await;
    *mock.preview.lock().unwrap() = vec!["b", "a", "a"];
    let event = verifier.verify(request()).await;
    assert_eq!(
        event.result,
        VerificationResult::Verified,
        "{:?}",
        event.reason
    );
    assert_eq!(
        event.channel_bindings,
        vec![ChannelBinding::E2eePublicKeySha256 {
            provider: "0g-private-computer".into(),
            key_id: Some(address("a")),
            algorithm: "0g-pc-v1-hpke-x25519-hkdf-sha256-chacha20poly1305".into(),
            public_key_sha256: hex::encode(Sha256::digest([b'a'; 32])),
        }]
    );
    let instance = &event.provider_claims.unwrap()["instances"][address("a")];
    assert_eq!(instance["signing_address"], signer("a"));
    assert_eq!(instance["tcb_status"], "UpToDate");
    assert_eq!(instance["boot_chain"], BOOT_CHAIN);
    let (headers, body) = mock.preview_requests.lock().unwrap()[0].clone();
    assert_eq!(headers["x-0g-provider-trust-mode"], "private");
    assert_eq!(body, json!({"model": MODEL, "service_type": "chatbot"}));

    verifier.invalidate(&request());
    *mock.preview.lock().unwrap() = vec!["b", "c"];
    let event = verifier.verify(request()).await;
    assert_eq!(event.result, VerificationResult::Failed);
    assert!(event.channel_bindings.is_empty());
    assert!(verifier.cached(&request()).is_none());
    server.abort();
}

#[tokio::test]
async fn rejection_reasons_never_carry_the_chain_rpc_credential() {
    let (mock, _, server) = fixture().await;
    let candidate = candidate("a");
    // An unreachable RPC and one that answers 404 both embed path and query
    // secrets in their URL.
    for rpc in [
        "http://127.0.0.1:1/v2/DEMO_RPC_SECRET?api_key=DEMO_QUERY_SECRET".to_string(),
        format!(
            "{}/v2/DEMO_RPC_SECRET?api_key=DEMO_QUERY_SECRET",
            mock.origin
        ),
    ] {
        let verifier =
            ZeroGProviderVerifier::new("http://127.0.0.1:1", "k", rpc, "0x00", None, 5, 0).unwrap();
        let err = verifier
            .verify_candidate(&candidate, MODEL)
            .await
            .unwrap_err();
        assert!(!err.contains("DEMO_"), "{err}");
    }
    server.abort();
}
