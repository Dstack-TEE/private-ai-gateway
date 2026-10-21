//! 0G Private Computer E2EE, chat profile (0g-pc-e2ee `protocol/SPEC.md` at
//! release-2026.10.19.2): request sealing (§5–§6), response opening (§7) and
//! the §8 response signature.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hpke::aead::{AeadCtxR, ChaCha20Poly1305};
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, Kem as _, OpModeR, OpModeS, Serializable};
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sha3::Keccak256;

use super::UpstreamError;

pub(super) type Kem = X25519HkdfSha256;
pub(super) const SEAL_INFO: &[u8] = b"0g-pc/v1/seal";
pub(super) const RESP_INFO: &[u8] = b"0g-pc/v1/resp";
pub(super) const E2EE: &str = "_e2ee";

/// The chat profile's default sealed set, plus the output schema and the
/// reasoning controls, which §5.1 lets a client seal too. The broker merges
/// them back into the request it serves.
const SEALED_FIELDS: &[&str] = &[
    "messages",
    "tools",
    "tool_choice",
    "response_format",
    "reasoning_effort",
    "chat_template_kwargs",
];
/// Fields sent in the clear but bound in the AAD. Anything else is refused
/// rather than sent in the clear (§5.1 would leave it cleartext).
const CLEARTEXT_FIELDS: &[&str] = &[
    "model",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "max_tokens",
    "max_completion_tokens",
    "stop",
    "n",
    "presence_penalty",
    "frequency_penalty",
    "repetition_penalty",
    "seed",
    "logprobs",
    "top_logprobs",
    "logit_bias",
    "parallel_tool_calls",
];

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ProtocolError(String);

impl From<ProtocolError> for UpstreamError {
    fn from(err: ProtocolError) -> Self {
        UpstreamError::Transport(format!("0G E2EE: {err}"))
    }
}

fn fail<T>(message: impl Into<String>) -> Result<T, ProtocolError> {
    Err(ProtocolError(message.into()))
}

/// Seal a chat request to `enc_pub`, pinned to `signer`. Returns the envelope
/// and the opener for its response.
pub(crate) fn seal_chat_request(
    mut request: Map<String, Value>,
    enc_pub: &[u8; 32],
    signer: &str,
) -> Result<(Value, ResponseOpener), UpstreamError> {
    if let Some(field) = request.keys().find(|key| {
        !SEALED_FIELDS.contains(&key.as_str()) && !CLEARTEXT_FIELDS.contains(&key.as_str())
    }) {
        // The middleware writes reasoning in the route's `reasoning_format`;
        // the nested object is its default dialect, which 0G does not take.
        let hint = if field == "reasoning" {
            "; the route's reasoning_format must be reasoning_effort"
        } else {
            ""
        };
        return Err(UpstreamError::InvalidRequest(format!(
            "request field {field:?} is not supported by the 0G E2EE transport{hint}"
        )));
    }
    if !request.contains_key("messages") {
        return Err(UpstreamError::InvalidRequest(
            "0G E2EE chat requests must carry messages".to_string(),
        ));
    }
    // As 0G's client does: a stream must request usage, and setting it before
    // sealing keeps `stream_options` bound instead of rewritten in transit.
    if request.get("stream") == Some(&Value::Bool(true)) {
        let options = request
            .entry("stream_options")
            .or_insert_with(|| serde_json::json!({}));
        if !options.is_object() {
            *options = serde_json::json!({});
        }
        options["include_usage"] = true.into();
    }
    let model = request.get("model").cloned();
    let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(enc_pub)
        .map_err(|_| ProtocolError("invalid provider encryption key".into()))?;
    let mut rng = rand::rngs::OsRng;
    let (response_secret, response_public) = Kem::gen_keypair(&mut rng);
    let (encapped, mut context) = hpke::setup_sender::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
        &OpModeS::Base,
        &recipient,
        SEAL_INFO,
        &mut rng,
    )
    .map_err(|e| ProtocolError(e.to_string()))?;

    let (sealed, mut envelope): (Map<_, _>, Map<_, _>) = request
        .into_iter()
        .partition(|(key, _)| SEALED_FIELDS.contains(&key.as_str()));
    envelope.insert(
        E2EE.into(),
        serde_json::json!({
            "v": 1,
            "kem_id": "0x0020",
            "key_id": URL_SAFE_NO_PAD.encode(&Sha256::digest(enc_pub)[..8]),
            "signer_addr": signer,
            "client_eph_pub": URL_SAFE_NO_PAD.encode(response_public.to_bytes()),
            "enc": URL_SAFE_NO_PAD.encode(encapped.to_bytes()),
            "sealed_fields": sealed.keys().collect::<Vec<_>>(),
        }),
    );
    let aad = envelope_aad(&envelope)?;
    let plaintext = serde_json::to_vec(&sealed).map_err(|e| ProtocolError(e.to_string()))?;
    let ciphertext = context
        .seal(&plaintext, &aad)
        .map_err(|e| ProtocolError(e.to_string()))?;
    envelope[E2EE]["ciphertext"] = URL_SAFE_NO_PAD.encode(&ciphertext).into();
    let opener = ResponseOpener::new(response_secret, binding_hash(&aad, &ciphertext), model);
    Ok((Value::Object(envelope), opener))
}

#[derive(Deserialize)]
struct FrameMeta {
    v: Option<u64>,
    enc: Option<String>,
    sealed_fields: Vec<String>,
    unbound_fields: Option<Vec<String>>,
    #[serde(default, rename = "final")]
    is_final: bool,
    ciphertext: String,
}

/// Opens one response's frames in order and accumulates the §8 binding.
pub(crate) struct ResponseOpener {
    secret: <Kem as hpke::Kem>::PrivateKey,
    context: Option<AeadCtxR<ChaCha20Poly1305, HkdfSha256, Kem>>,
    request_hash: [u8; 32],
    /// The request's `model`, which this client binds (nothing is unbound).
    model: Option<Value>,
    frame_hashes: Vec<[u8; 32]>,
    saw_final: bool,
}

impl ResponseOpener {
    pub(super) fn new(
        secret: <Kem as hpke::Kem>::PrivateKey,
        request_hash: [u8; 32],
        model: Option<Value>,
    ) -> Self {
        Self {
            secret,
            context: None,
            request_hash,
            model,
            frame_hashes: Vec::new(),
            saw_final: false,
        }
    }

    /// Open the next frame into its bound cleartext fields merged with the
    /// decrypted ones. Refuses unsealed, out-of-order, tampered or post-final
    /// frames and the sealed/unbound sets the chat profile forbids (§12).
    ///
    /// An unbound field is outside the AAD and the §8 binding, so an
    /// intermediary can set it undetected (§5.2, §8.2: "a router-injected
    /// `x_0g_trace` is unauthenticated by construction"); none is returned.
    /// 0G unbinds `model` so the router can restore its alias (§5.2); its
    /// place is taken by the model this request bound.
    pub fn open_frame(
        &mut self,
        mut frame: Map<String, Value>,
    ) -> Result<Map<String, Value>, ProtocolError> {
        if self.saw_final {
            return fail("0G response carried a frame after its final frame");
        }
        let meta: FrameMeta = match frame.get(E2EE) {
            Some(meta) => serde_json::from_value(meta.clone())
                .map_err(|e| ProtocolError(format!("malformed 0G frame envelope: {e}")))?,
            None => return fail("0G response frame is not sealed"),
        };
        let sealed_fields = meta.sealed_fields;
        let unbound = meta.unbound_fields.unwrap_or_default();
        if !sealed_fields.iter().any(|f| f == "choices")
            || sealed_fields
                .iter()
                .any(|f| matches!(f.as_str(), "usage" | "model" | E2EE))
            || unbound.iter().any(|f| matches!(f.as_str(), "usage" | E2EE))
        {
            return fail("0G response frame seals or unbinds fields the chat profile forbids");
        }
        if self.context.is_none() {
            let (Some(1), Some(enc)) = (meta.v, meta.enc) else {
                return fail("first 0G response frame lacks version 1 and enc");
            };
            let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(&decode(&enc)?)
                .map_err(|_| ProtocolError("invalid 0G response enc".into()))?;
            self.context = Some(
                hpke::setup_receiver(&OpModeR::Base, &self.secret, &encapped, RESP_INFO)
                    .map_err(|e| ProtocolError(e.to_string()))?,
            );
        }
        let ciphertext = decode(&meta.ciphertext)?;
        let aad = envelope_aad(&frame)?;
        let context = self.context.as_mut().expect("context set above");
        let plaintext = context.open(&ciphertext, &aad).map_err(|_| {
            ProtocolError("0G response frame failed authenticated decryption".into())
        })?;
        let sealed: Map<String, Value> = serde_json::from_slice(&plaintext)
            .map_err(|_| ProtocolError("decrypted 0G response is not a JSON object".into()))?;
        frame.remove(E2EE);
        let mut declared = sealed_fields;
        declared.sort();
        // Map keys are unique, so equality also rejects duplicate declarations.
        if !declared
            .iter()
            .eq(sealed.keys().collect::<std::collections::BTreeSet<_>>())
            || sealed.keys().any(|key| frame.contains_key(key))
        {
            return fail("decrypted 0G response fields do not match sealed_fields");
        }
        for field in &unbound {
            if frame.remove(field).is_some() && field == "model" {
                if let Some(model) = &self.model {
                    frame.insert(field.clone(), model.clone());
                }
            }
        }
        frame.extend(sealed);
        self.frame_hashes.push(binding_hash(&aad, &ciphertext));
        self.saw_final = meta.is_final;
        Ok(frame)
    }

    /// The §8 text the provider must have signed: one final frame for a
    /// buffered response, every frame through the final one for a stream.
    pub fn signed_text(&self, stream: bool) -> Result<String, ProtocolError> {
        let response_hash: [u8; 32] = match (stream, self.frame_hashes.as_slice()) {
            _ if !self.saw_final => return fail("0G response ended before its final frame"),
            (false, [frame]) => *frame,
            (false, _) => return fail("a buffered 0G response must be one frame"),
            (true, frames) => Sha256::digest(frames.concat()).into(),
        };
        let scheme = if stream {
            "zg-sig-v1/e2ee-ct-stream"
        } else {
            "zg-sig-v1/e2ee-ct"
        };
        Ok(format!(
            "{scheme}:{}:{}",
            hex::encode(self.request_hash),
            hex::encode(response_hash)
        ))
    }
}

/// The §8 `ChatSignature` a provider broker serves.
#[derive(Debug, Deserialize)]
pub(crate) struct ChatSignature {
    pub text: String,
    pub signature: String,
}

/// §8: the signed text must be exactly `expected_text` and the EIP-191
/// signature must recover to `signer`, never the self-reported address.
pub(crate) fn verify_signature(
    signature: &ChatSignature,
    expected_text: &str,
    signer: &str,
) -> Result<(), ProtocolError> {
    if signature.text != expected_text {
        return fail("0G signature does not bind this request and response");
    }
    let recovered = recover_signer(&signature.text, &signature.signature)?;
    if !recovered.eq_ignore_ascii_case(signer) {
        return fail(format!(
            "0G signature recovered {recovered}, expected {signer}"
        ));
    }
    Ok(())
}

pub(super) fn recover_signer(text: &str, signature: &str) -> Result<String, ProtocolError> {
    let invalid = || ProtocolError("malformed 0G signature".into());
    let bytes = hex::decode(signature.trim_start_matches("0x")).map_err(|_| invalid())?;
    let [rs @ .., v] = bytes.as_slice() else {
        return Err(invalid());
    };
    let sig = Signature::from_slice(rs).map_err(|_| invalid())?;
    let recovery_id = RecoveryId::from_byte(v.checked_sub(27).unwrap_or(*v)).ok_or_else(invalid)?;
    let digest = Keccak256::new()
        .chain_update(format!("\x19Ethereum Signed Message:\n{}", text.len()))
        .chain_update(text)
        .finalize();
    let key =
        VerifyingKey::recover_from_prehash(&digest, &sig, recovery_id).map_err(|_| invalid())?;
    let point = key.to_encoded_point(false);
    Ok(format!(
        "0x{}",
        hex::encode(&Keccak256::digest(&point.as_bytes()[1..])[12..])
    ))
}

/// `H(aad, ct) = sha256(sha256(aad) ‖ sha256(ct))` (§8.1).
pub(super) fn binding_hash(aad: &[u8], ciphertext: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(Sha256::digest(aad))
        .chain_update(Sha256::digest(ciphertext))
        .finalize()
        .into()
}

/// §5.2: JCS of the envelope without `_e2ee.ciphertext` and the fields named
/// in `_e2ee.unbound_fields`.
pub(super) fn envelope_aad(envelope: &Map<String, Value>) -> Result<Vec<u8>, ProtocolError> {
    let Some(Value::Object(meta)) = envelope.get(E2EE) else {
        return fail("envelope carries no _e2ee object");
    };
    let mut meta = meta.clone();
    let unbound: Option<Vec<String>> =
        serde_json::from_value(meta.get("unbound_fields").cloned().unwrap_or_default())
            .map_err(|_| ProtocolError("unbound_fields must be an array of strings".into()))?;
    let mut bound = envelope.clone();
    for field in unbound.unwrap_or_default() {
        bound.remove(&field);
    }
    meta.remove("ciphertext");
    bound.insert(E2EE.into(), Value::Object(meta));
    serde_json_canonicalizer::to_vec(&Value::Object(bound))
        .map_err(|e| ProtocolError(e.to_string()))
}

pub(super) fn decode(value: &str) -> Result<Vec<u8>, ProtocolError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ProtocolError("0G field is not unpadded base64url".into()))
}
