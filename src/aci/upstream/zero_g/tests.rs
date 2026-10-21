//! Interop with 0G's Go library (`tests/fixtures/0g-private-computer`) and the
//! backend against a mock router and broker that run the enclave side.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hpke::aead::{AeadCtxS, ChaCha20Poly1305};
use hpke::kdf::HkdfSha256;
use hpke::{Deserializable, Kem as _, OpModeR, OpModeS, Serializable};
use k256::ecdsa::SigningKey;
use serde_json::json;
use sha3::Keccak256;

use super::protocol::{
    binding_hash, decode, envelope_aad, recover_signer, Kem, E2EE, RESP_INFO, SEAL_INFO,
};
use super::*;

const MODEL: &str = "glm-5.3";
const ADDRESS: &str = "0x7dcfe6aea70350c2090041524c9b4a9262dce87d";
/// A field the broker declares unbound and the mock router injects.
const ROUTER_NOTE: &str = "router_note";
const INTEROP: &str = include_str!("../../../../tests/fixtures/0g-private-computer/interop.json");

type Secret = <Kem as hpke::Kem>::PrivateKey;
/// A request after the enclave's `Open`: request, client key, request hash.
type OpenedRequest = (Map<String, Value>, Vec<u8>, [u8; 32]);

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().expect("object").clone()
}

fn signer(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_slice(&[seed; 32]).unwrap();
    let point = key.verifying_key().to_encoded_point(false);
    let address = hex::encode(&Keccak256::digest(&point.as_bytes()[1..])[12..]);
    (key, format!("0x{address}"))
}

fn sign(key: &SigningKey, text: &str) -> String {
    let digest = Keccak256::new()
        .chain_update(format!("\x19Ethereum Signed Message:\n{}", text.len()))
        .chain_update(text)
        .finalize();
    let (sig, recovery_id) = key.sign_prehash_recoverable(&digest).unwrap();
    format!(
        "0x{}{:02x}",
        hex::encode(sig.to_bytes()),
        27 + recovery_id.to_byte()
    )
}

/// The enclave side of `Open` (§6).
fn open_request(secret: &Secret, envelope: &Map<String, Value>) -> Option<OpenedRequest> {
    let meta = &envelope[E2EE];
    let field = |name: &str| decode(meta[name].as_str().unwrap_or_default()).ok();
    let ciphertext = field("ciphertext")?;
    let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(&field("enc")?).ok()?;
    let aad = envelope_aad(envelope).ok()?;
    let mut context = hpke::setup_receiver::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &OpModeR::Base,
        secret,
        &encapped,
        SEAL_INFO,
    )
    .ok()?;
    let sealed: Map<String, Value> =
        serde_json::from_slice(&context.open(&ciphertext, &aad).ok()?).ok()?;
    let mut request = envelope.clone();
    request.remove(E2EE);
    request.extend(sealed);
    Some((
        request,
        field("client_eph_pub")?,
        binding_hash(&aad, &ciphertext),
    ))
}

/// Seals `choices` out of response frames and computes the §8 texts.
struct ResponseSealer {
    context: AeadCtxS<ChaCha20Poly1305, HkdfSha256, Kem>,
    enc: Option<String>,
    request_hash: [u8; 32],
    frame_hashes: Vec<u8>,
}

impl ResponseSealer {
    fn new(client_key: &[u8], request_hash: [u8; 32]) -> Self {
        let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(client_key).unwrap();
        let (encapped, context) = hpke::setup_sender::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
            &OpModeS::Base,
            &recipient,
            RESP_INFO,
            &mut rand::rngs::OsRng,
        )
        .unwrap();
        Self {
            context,
            enc: Some(URL_SAFE_NO_PAD.encode(encapped.to_bytes())),
            request_hash,
            frame_hashes: Vec::new(),
        }
    }

    fn seal(&mut self, mut frame: Map<String, Value>, is_final: bool) -> Value {
        let sealed = json!({"choices": frame.remove("choices").unwrap()});
        self.seal_as(frame, sealed, json!(["choices"]), is_final)
    }

    /// Seal `sealed` into `frame` under an arbitrary `sealed_fields` claim.
    fn seal_as(
        &mut self,
        mut frame: Map<String, Value>,
        sealed: Value,
        sealed_fields: Value,
        is_final: bool,
    ) -> Value {
        let mut meta = json!({"sealed_fields": sealed_fields, "unbound_fields": ["model", ROUTER_NOTE], "final": is_final});
        if let Some(enc) = self.enc.take() {
            meta["v"] = 1.into();
            meta["enc"] = enc.into();
        }
        frame.insert(E2EE.into(), meta);
        let aad = envelope_aad(&frame).unwrap();
        let ciphertext = self
            .context
            .seal(sealed.to_string().as_bytes(), &aad)
            .unwrap();
        self.frame_hashes.extend(binding_hash(&aad, &ciphertext));
        frame[E2EE]["ciphertext"] = URL_SAFE_NO_PAD.encode(&ciphertext).into();
        Value::Object(frame)
    }

    fn signed_text(&self, stream: bool) -> String {
        let (scheme, hash): (_, [u8; 32]) = if stream {
            (
                "zg-sig-v1/e2ee-ct-stream",
                Sha256::digest(&self.frame_hashes).into(),
            )
        } else {
            (
                "zg-sig-v1/e2ee-ct",
                self.frame_hashes[..32].try_into().unwrap(),
            )
        };
        format!(
            "{scheme}:{}:{}",
            hex::encode(self.request_hash),
            hex::encode(hash)
        )
    }
}

fn fixture_opener(f: &Value) -> ResponseOpener {
    let secret =
        Secret::from_bytes(&decode(f["client_eph_private_key"].as_str().unwrap()).unwrap())
            .unwrap();
    let mut request_hash = [0u8; 32];
    hex::decode_to_slice(
        f["request_binding_hash"].as_str().unwrap(),
        &mut request_hash,
    )
    .unwrap();
    ResponseOpener::new(
        secret,
        request_hash,
        Some(f["request_plain"]["model"].clone()),
    )
}

fn fixture_signature(value: &Value) -> ChatSignature {
    serde_json::from_value(value.clone()).unwrap()
}

#[test]
fn interoperates_with_the_0g_go_library() {
    let f: Value = serde_json::from_str(INTEROP).unwrap();
    let signer = f["signer_address"].as_str().unwrap();
    let enc_secret =
        Secret::from_bytes(&decode(f["enc_private_key"].as_str().unwrap()).unwrap()).unwrap();
    let (request, _, request_hash) =
        open_request(&enc_secret, &object(&f["request_sealed"])).unwrap();
    assert_eq!(Value::Object(request), f["request_plain"]);
    assert_eq!(hex::encode(request_hash), f["request_binding_hash"]);

    let mut buffered = fixture_opener(&f);
    let body = buffered.open_frame(object(&f["response_sealed"])).unwrap();
    assert_eq!(Value::Object(body), f["response_plain"]);
    let text = buffered.signed_text(false).unwrap();
    verify_signature(&fixture_signature(&f["response_signature"]), &text, signer).unwrap();

    let mut stream = fixture_opener(&f);
    for (frame, plain) in f["stream_frames"]
        .as_array()
        .unwrap()
        .iter()
        .zip(f["stream_plain"].as_array().unwrap())
    {
        assert_eq!(
            &Value::Object(stream.open_frame(object(frame)).unwrap()),
            plain
        );
    }
    let text = stream.signed_text(true).unwrap();
    verify_signature(&fixture_signature(&f["stream_signature"]), &text, signer).unwrap();
    // The other response's signature does not verify this one.
    assert!(verify_signature(&fixture_signature(&f["response_signature"]), &text, signer).is_err());

    // KAT from 0g-pc-e2ee client/sig: the broker's go-ethereum signer, key 0x11 * 32.
    let kat = format!("zg-sig-v1/e2ee-ct:{}:{}", "a".repeat(64), "b".repeat(64));
    assert_eq!(
        recover_signer(&kat, "0xbd8367999ec7d94c979edeb5538ae98e8cc565b30d3e9be41fdc27675180892a7a2a2273ce88e3f5d1a2422b7326b14ab1ccf4acc6c05375463b4cda85a9b1c31b").unwrap(),
        "0x19e7e376e7c213b7e7e7e46cc70a5dd086daff2a"
    );
}

#[test]
fn opener_refuses_tampered_reordered_and_unsealed_frames() {
    let f: Value = serde_json::from_str(INTEROP).unwrap();
    let frames: Vec<_> = f["stream_frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(object)
        .collect();

    let mut tampered = object(&f["response_sealed"]);
    tampered["usage"]["completion_tokens"] = 999.into();
    assert!(fixture_opener(&f).open_frame(tampered).is_err());

    let mut reordered = fixture_opener(&f);
    reordered.open_frame(frames[0].clone()).unwrap();
    assert!(reordered.open_frame(frames[2].clone()).is_err());

    let mut truncated = fixture_opener(&f);
    truncated.open_frame(frames[0].clone()).unwrap();
    assert!(truncated.signed_text(true).is_err());

    let mut unbound_usage = object(&f["response_sealed"]);
    unbound_usage[E2EE]["unbound_fields"] = json!(["model", "usage"]);
    assert!(fixture_opener(&f).open_frame(unbound_usage).is_err());
    assert!(fixture_opener(&f)
        .open_frame(object(&f["response_plain"]))
        .is_err());
}

#[test]
fn opener_refuses_duplicate_sealed_field_declarations() {
    // Declaring `choices` twice must not let an undeclared encrypted `usage`
    // overwrite the bound cleartext one.
    let (secret, public) = Kem::derive_keypair(&[4; 32]);
    let mut sealer = ResponseSealer::new(&public.to_bytes(), [0; 32]);
    let frame = sealer.seal_as(
        object(&json!({"usage": {"total_tokens": 1}})),
        json!({"choices": [], "usage": {"total_tokens": 999}}),
        json!(["choices", "choices"]),
        true,
    );
    let mut opener = ResponseOpener::new(secret, [0; 32], None);
    assert!(opener.open_frame(object(&frame)).is_err());
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Ok,
    WrongSigner,
    SignatureMissing,
    Unsealed,
    RouterError,
    Truncated,
    Reordered,
    Tampered,
}

struct Broker {
    mode: Mode,
    secret: Secret,
    signing_key: SigningKey,
    requests: Mutex<Vec<(HeaderMap, Value)>>,
    signed_text: Mutex<String>,
}

struct Fixture {
    broker: Arc<Broker>,
    backend: ZeroGProviderBackend,
    event: UpstreamVerifiedEvent,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new(mode: Mode) -> Self {
        let (secret, enc_pub) = Kem::derive_keypair(&[1; 32]);
        let enc_pub = enc_pub.to_bytes();
        let (signing_key, _) = signer(if mode == Mode::WrongSigner { 3 } else { 2 });
        let broker = Arc::new(Broker {
            mode,
            secret,
            signing_key,
            requests: Mutex::default(),
            signed_text: Mutex::default(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/v1/proxy/signature/:key", get(signature))
            .with_state(broker.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = VerifiedProvider {
            provider_address: ADDRESS.into(),
            provider_identity: None,
            endpoint: origin.clone(),
            model: MODEL.into(),
            signing_address: signer(2).1,
            e2ee_public_key: hex::encode(enc_pub),
        };
        let event = UpstreamVerifiedEvent {
            provider_type: Some(ZERO_G_PROVIDER.into()),
            model_id: MODEL.into(),
            result: VerificationResult::Verified,
            channel_bindings: vec![ChannelBinding::E2eePublicKeySha256 {
                provider: ZERO_G_PROVIDER.into(),
                key_id: Some(ADDRESS.into()),
                algorithm: ZERO_G_E2EE_ALGORITHM.into(),
                public_key_sha256: hex::encode(Sha256::digest(enc_pub)),
            }],
            provider_claims: Some(json!({"instances": {ADDRESS: provider}})),
            ..Default::default()
        };
        Self {
            broker,
            backend: ZeroGProviderBackend::new_with_timeouts(&origin, "zg-test-key", 5, 5).unwrap(),
            event,
            server,
        }
    }

    fn request(&self, body: Value) -> PreparedUpstreamRequest {
        PreparedUpstreamRequest {
            request: UpstreamRequest {
                body: serde_json::to_vec(&body).unwrap(),
                // Caller-supplied 0G controls must never reach the router.
                headers: HashMap::from([(TRUST_MODE_HEADER.to_string(), "standard".to_string())]),
                path: Some(CHAT_COMPLETIONS_PATH.into()),
                target_route_id: None,
            },
            upstream_name: "zero-g".into(),
            url_origin: None,
            model_id: MODEL.into(),
            route_id: None,
            is_tee: Some(true),
        }
    }

    fn chat(&self, stream: bool) -> PreparedUpstreamRequest {
        self.request(json!({"model": MODEL, "stream": stream, "messages": [{"role": "user", "content": "secret prompt"}]}))
    }

    async fn buffered(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.backend
            .forward_verified_prepared(self.chat(false), &self.event)
            .await
    }

    async fn stream(&self) -> Vec<Result<String, String>> {
        let response = self
            .backend
            .forward_stream_verified_prepared(self.chat(true), &self.event)
            .await
            .unwrap();
        response
            .body
            .map(|item| {
                item.map(|b| String::from_utf8(b.to_vec()).unwrap())
                    .map_err(|e| e.to_string())
            })
            .collect()
            .await
    }
}

fn completion(stream: bool) -> Vec<Map<String, Value>> {
    let frames = if stream {
        json!([
            {"id": "c1", "model": MODEL, "choices": [{"index": 0, "delta": {"content": "o"}}]},
            {"id": "c1", "model": MODEL, "choices": [{"index": 0, "delta": {"content": "k"}, "finish_reason": "stop"}]},
            {"id": "c1", "model": MODEL, "usage": {"total_tokens": 5}, "choices": []},
            // As live, the last frame carries only an empty `choices` and
            // the router's unbound fields.
            {"choices": []},
        ])
    } else {
        json!([{"id": "c1", "model": MODEL, "usage": {"total_tokens": 4},
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}]}])
    };
    frames.as_array().unwrap().iter().map(object).collect()
}

async fn chat(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    broker
        .requests
        .lock()
        .unwrap()
        .push((headers, body.clone()));
    if broker.mode == Mode::RouterError {
        return (StatusCode::BAD_GATEWAY, "provider-controlled text").into_response();
    }
    let (request, client_key, request_hash) = open_request(&broker.secret, &object(&body)).unwrap();
    let stream = request["stream"] == json!(true);
    let plain = completion(stream);
    let mut sealer = ResponseSealer::new(&client_key, request_hash);
    let last = plain.len() - 1;
    let mut frames: Vec<Value> = plain
        .iter()
        .enumerate()
        .map(|(i, f)| sealer.seal(f.clone(), i == last))
        .collect();
    // In transit the router sets the frames' unbound fields as it likes.
    for frame in &mut frames {
        frame[ROUTER_NOTE] = json!({"billing": {"total_cost": "42"}, "provider": ADDRESS});
        if frame.get("model").is_some() {
            frame["model"] = "router-alias".into();
        }
    }
    *broker.signed_text.lock().unwrap() = sealer.signed_text(stream);
    match broker.mode {
        Mode::Unsealed => frames = plain.into_iter().map(Value::Object).collect(),
        Mode::Truncated => {
            frames.pop();
        }
        Mode::Reordered => frames.swap(0, 1),
        Mode::Tampered => frames[1]["id"] = "forged".into(),
        _ => {}
    }
    let mut response = if stream {
        let events: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
        (
            [("content-type", "text/event-stream")],
            events + "data: [DONE]\n\n",
        )
            .into_response()
    } else {
        Json(frames.remove(0)).into_response()
    };
    response
        .headers_mut()
        .insert("zg-res-key", "res-1".parse().unwrap());
    response
}

async fn signature(State(broker): State<Arc<Broker>>) -> Response {
    if broker.mode == Mode::SignatureMissing {
        return StatusCode::NOT_FOUND.into_response();
    }
    let text = broker.signed_text.lock().unwrap().clone();
    let signature = sign(&broker.signing_key, &text);
    Json(json!({"text": text, "signature": signature, "signing_address": "0x00"})).into_response()
}

#[tokio::test]
async fn buffered_request_is_sealed_pinned_and_verified() {
    let fixture = Fixture::new(Mode::Ok).await;
    let response = fixture.buffered().await.unwrap();
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
    // Unbound fields do not reach the caller; bound ones and usage do, and
    // `model` is the one the request bound, not the router's.
    assert!(body.get(ROUTER_NOTE).is_none());
    assert_eq!(body["id"], "c1");
    assert_eq!(body["usage"]["total_tokens"], 4);
    assert_eq!(body["model"], MODEL);
    assert_eq!(response.served_instance_id.as_deref(), Some(ADDRESS));

    let requests = fixture.broker.requests.lock().unwrap();
    let (headers, sent) = &requests[0];
    assert_eq!(headers[TRUST_MODE_HEADER], "private");
    assert_eq!(headers["x-0g-provider-address"], ADDRESS);
    assert_eq!(headers["x-0g-provider-allow-fallbacks"], "false");
    assert_eq!(headers["authorization"], "Bearer zg-test-key");
    assert!(!sent.to_string().contains("secret prompt"));
    assert_eq!(sent["model"], MODEL, "the configured model is sealed");
}

#[tokio::test]
async fn buffered_response_without_a_verified_signature_is_a_generic_error() {
    for mode in [
        Mode::WrongSigner,
        Mode::SignatureMissing,
        Mode::Unsealed,
        Mode::RouterError,
    ] {
        let err = Fixture::new(mode).await.buffered().await.unwrap_err();
        assert!(
            !err.to_string().contains("provider-controlled"),
            "{mode:?}: {err}"
        );
        // The router's status survives for failover and the client; its body
        // does not.
        if mode == Mode::RouterError {
            assert!(
                matches!(err, UpstreamError::Upstream { status: 502 }),
                "{err}"
            );
        }
    }
}

#[tokio::test]
async fn stream_ends_in_done_only_after_the_signature_verifies() {
    let fixture = Fixture::new(Mode::Ok).await;
    let events: Vec<String> = fixture
        .stream()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(events.len(), 4);
    assert!(events[0].contains(r#""content":"o""#) && !events[0].contains(E2EE));
    assert!(events[..3]
        .iter()
        .all(|e| !e.contains(ROUTER_NOTE) && !e.contains("router-alias") && e.contains(MODEL)));
    // The last frame held only unbound fields and an empty `choices`, so it
    // is not emitted.
    assert!(events[2].contains(r#""usage":{"total_tokens":5}"#));
    assert_eq!(events[3], "data: [DONE]\n\n");
    // Usage is requested before sealing, as 0G's client does, so it stays bound.
    let sent = fixture.broker.requests.lock().unwrap()[0].1.clone();
    assert_eq!(sent["stream_options"]["include_usage"], true);

    for (mode, delivered) in [
        (Mode::WrongSigner, 3),
        (Mode::Truncated, 3),
        (Mode::Reordered, 0),
        (Mode::Tampered, 1),
    ] {
        let items = Fixture::new(mode).await.stream().await;
        assert_eq!(items.len(), delivered + 1, "{mode:?}");
        assert!(items[delivered].is_err(), "{mode:?}");
        assert!(items
            .iter()
            .flatten()
            .all(|event| !event.contains("[DONE]")));
    }
}

#[tokio::test]
async fn refuses_before_sending_without_a_binding_or_with_an_e2ee_field() {
    let fixture = Fixture::new(Mode::Ok).await;
    let mut unbound = fixture.event.clone();
    unbound.channel_bindings.clear();
    let err = fixture
        .backend
        .forward_verified_prepared(fixture.chat(false), &unbound)
        .await
        .unwrap_err();
    assert!(matches!(err, UpstreamError::ChannelBindingMismatch(_)));

    let body = json!({"model": MODEL, "messages": [], E2EE: {"v": 1}});
    let err = fixture
        .backend
        .forward_verified_prepared(fixture.request(body), &fixture.event)
        .await
        .unwrap_err();
    assert!(matches!(err, UpstreamError::InvalidRequest(_)), "{err}");
    assert!(fixture.broker.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn fields_outside_the_cleartext_set_are_sealed() {
    let fixture = Fixture::new(Mode::Ok).await;
    let schema = json!({"type": "json_schema", "json_schema": {"name": "secret_schema"}});
    // The middleware's reasoning controls for a `reasoning_effort` route, a
    // direct caller's chat template switch, sampling controls that can carry
    // private text or token ids, and fields OpenAI SDKs and agents send that
    // the transport has no rule for.
    let sealed = json!({
        "messages": [{"role": "user", "content": "secret prompt"}],
        "response_format": schema,
        "reasoning_effort": "high",
        "chat_template_kwargs": {"enable_thinking": true},
        "temperature": 0.5,
        "stop": ["secret-stop"],
        "logit_bias": {"42": -100},
        "user": "secret-user",
        "store": false,
        "metadata": {"session": "secret-session"},
        "prompt_cache_key": "secret-cache-key",
    });
    let mut body = object(&sealed);
    body.extend(object(
        &json!({"model": MODEL, "stream": false, "max_tokens": 64}),
    ));
    fixture
        .backend
        .forward_verified_prepared(fixture.request(Value::Object(body)), &fixture.event)
        .await
        .unwrap();
    let sent = object(&fixture.broker.requests.lock().unwrap()[0].1);
    let mut cleartext: Vec<_> = sent.keys().map(String::as_str).collect();
    cleartext.sort_unstable();
    assert_eq!(cleartext, [E2EE, "max_tokens", "model", "stream"]);
    assert!(!Value::Object(sent.clone()).to_string().contains("secret"));
    let (opened, _, _) = open_request(&fixture.broker.secret, &sent).unwrap();
    for (field, value) in object(&sealed) {
        assert_eq!(opened[&field], value, "{field}");
    }

    // Cleartext fields are bound: rewriting one fails the enclave's `Open`.
    let mut rewritten = sent;
    rewritten["max_tokens"] = 4096.into();
    assert!(open_request(&fixture.broker.secret, &rewritten).is_none());
}
