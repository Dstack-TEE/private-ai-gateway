//! Chutes E2EE: ML-KEM key agreement, ChaCha20-Poly1305, gzip, and the
//! streaming decryptor for the `/e2e/invoke` transport.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use chacha20poly1305::{
    aead::{Aead, KeyInit as AeadKeyInit},
    ChaCha20Poly1305, Nonce,
};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use futures_util::Stream;
use ml_kem::{
    kem::{Decapsulate, Encapsulate, Kem, KeyExport, TryKeyInit},
    ml_kem_768::{
        Ciphertext as MlKemCiphertext768, DecapsulationKey as MlKemDecapsulationKey768,
        EncapsulationKey as MlKemEncapsulationKey768,
    },
    MlKem768,
};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    ChutesE2eeRequest, CHUTES_INFO_REQ, CHUTES_INFO_RESP, CHUTES_INFO_STREAM, CHUTES_MLKEM_CT_SIZE,
    CHUTES_TAG_SIZE,
};
use crate::aci::upstream::{UpstreamBodyStream, UpstreamError};

pub(super) fn chutes_e2ee_pubkey_sha256(e2e_pubkey_b64: &str) -> Result<String, UpstreamError> {
    let pubkey = BASE64
        .decode(e2e_pubkey_b64)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes E2EE public key: {e}")))?;
    Ok(hex::encode(Sha256::digest(&pubkey)))
}

pub(super) fn build_chutes_e2ee_request(
    e2e_pubkey_b64: &str,
    payload: Value,
) -> Result<ChutesE2eeRequest, UpstreamError> {
    let (response_sk, response_pk) = MlKem768::generate_keypair();
    let e2e_pubkey = BASE64
        .decode(e2e_pubkey_b64)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes E2EE public key: {e}")))?;
    let e2e_pubkey = MlKemEncapsulationKey768::new_from_slice(&e2e_pubkey)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes ML-KEM public key: {e}")))?;
    let (mlkem_ct, shared_secret) = e2e_pubkey.encapsulate();
    let sym_key = chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_REQ,
    )?;
    let mut payload = payload;
    let Some(obj) = payload.as_object_mut() else {
        return Err(UpstreamError::Routing(
            "request body must be a JSON object".to_string(),
        ));
    };
    obj.insert(
        "e2e_response_pk".to_string(),
        Value::String(BASE64.encode(response_pk.to_bytes().as_slice())),
    );
    let payload =
        serde_json::to_vec(&payload).map_err(|e| UpstreamError::Transport(e.to_string()))?;
    let compressed = gzip_compress(&payload)?;
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let encrypted = chacha_encrypt(&sym_key, &nonce, &compressed)?;

    let mut blob = Vec::with_capacity(mlkem_ct.as_slice().len() + nonce.len() + encrypted.len());
    blob.extend_from_slice(mlkem_ct.as_slice());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&encrypted);
    Ok(ChutesE2eeRequest { blob, response_sk })
}

pub(super) fn decrypt_chutes_response(
    response_blob: &[u8],
    response_sk: &MlKemDecapsulationKey768,
) -> Result<Vec<u8>, UpstreamError> {
    if response_blob.len() <= CHUTES_MLKEM_CT_SIZE + 12 + CHUTES_TAG_SIZE {
        return Err(UpstreamError::Transport(
            "Chutes E2EE response blob is too short".to_string(),
        ));
    }
    let mlkem_ct = MlKemCiphertext768::try_from(&response_blob[..CHUTES_MLKEM_CT_SIZE])
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes response ML-KEM CT: {e}")))?;
    let nonce = &response_blob[CHUTES_MLKEM_CT_SIZE..CHUTES_MLKEM_CT_SIZE + 12];
    let ciphertext = &response_blob[CHUTES_MLKEM_CT_SIZE + 12..];
    let shared_secret = response_sk.decapsulate(&mlkem_ct);
    let sym_key = chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_RESP,
    )?;
    let plaintext = chacha_decrypt(&sym_key, nonce, ciphertext)?;
    gzip_decompress(&plaintext)
}

pub(super) fn decrypt_chutes_stream_init(
    response_sk: &MlKemDecapsulationKey768,
    mlkem_ct_b64: &str,
) -> Result<Vec<u8>, UpstreamError> {
    let mlkem_ct = BASE64
        .decode(mlkem_ct_b64)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes stream init: {e}")))?;
    let mlkem_ct = MlKemCiphertext768::try_from(mlkem_ct.as_slice())
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes stream ML-KEM CT: {e}")))?;
    let shared_secret = response_sk.decapsulate(&mlkem_ct);
    chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_STREAM,
    )
}

pub(super) fn decrypt_chutes_stream_chunk(
    stream_key: &[u8],
    chunk_b64: &str,
) -> Result<Vec<u8>, UpstreamError> {
    let raw = BASE64
        .decode(chunk_b64)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes stream chunk: {e}")))?;
    if raw.len() <= 12 + CHUTES_TAG_SIZE {
        return Err(UpstreamError::Transport(
            "Chutes E2EE stream chunk is too short".to_string(),
        ));
    }
    chacha_decrypt(stream_key, &raw[..12], &raw[12..])
}

fn chutes_derive_key(
    shared_secret: &[u8],
    mlkem_ct: &[u8],
    info: &[u8],
) -> Result<Vec<u8>, UpstreamError> {
    let salt = mlkem_ct.get(..16).ok_or_else(|| {
        UpstreamError::Transport("Chutes ML-KEM ciphertext is too short".to_string())
    })?;
    let hkdf = hkdf::Hkdf::<Sha256>::new(Some(salt), shared_secret);
    let mut key = [0u8; 32];
    hkdf.expand(info, &mut key)
        .map_err(|_| UpstreamError::Transport("Chutes HKDF failed".to_string()))?;
    Ok(key.to_vec())
}

#[allow(deprecated)]
fn chacha_encrypt(key: &[u8], nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| UpstreamError::Transport("invalid Chutes ChaCha20 key".to_string()))?;
    cipher
        .encrypt(Nonce::from_slice(nonce), plaintext)
        .map_err(|_| UpstreamError::Transport("Chutes E2EE encryption failed".to_string()))
}

#[allow(deprecated)]
fn chacha_decrypt(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| UpstreamError::Transport("invalid Chutes ChaCha20 key".to_string()))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| UpstreamError::Transport("Chutes E2EE decryption failed".to_string()))
}

fn gzip_compress(plaintext: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(plaintext)
        .map_err(|e| UpstreamError::Transport(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| UpstreamError::Transport(e.to_string()))
}

fn gzip_decompress(compressed: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut decoder = GzDecoder::new(compressed);
    let mut plaintext = Vec::new();
    decoder
        .read_to_end(&mut plaintext)
        .map_err(|e| UpstreamError::Transport(e.to_string()))?;
    Ok(plaintext)
}

pub(super) struct ChutesE2eeDecryptingStream {
    inner: UpstreamBodyStream,
    response_sk: MlKemDecapsulationKey768,
    stream_key: Option<Vec<u8>>,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    finished: bool,
}

impl ChutesE2eeDecryptingStream {
    pub(super) fn new(inner: UpstreamBodyStream, response_sk: MlKemDecapsulationKey768) -> Self {
        Self {
            inner,
            response_sk,
            stream_key: None,
            buffer: Vec::new(),
            pending: VecDeque::new(),
            finished: false,
        }
    }

    fn process_buffer(&mut self) -> Result<(), UpstreamError> {
        while let Some(pos) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=pos).collect::<Vec<_>>();
            if line.ends_with(b"\n") {
                line.pop();
            }
            if line.ends_with(b"\r") {
                line.pop();
            }
            self.process_sse_line(&line)?;
        }
        Ok(())
    }

    fn process_sse_line(&mut self, line: &[u8]) -> Result<(), UpstreamError> {
        let Some(data) = line.strip_prefix(b"data: ") else {
            return Ok(());
        };
        let raw = String::from_utf8(data.to_vec()).map_err(|_| {
            UpstreamError::Transport("Chutes E2EE stream line is not UTF-8".to_string())
        })?;
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(());
        }
        if raw == "[DONE]" {
            self.pending
                .push_back(Bytes::from_static(b"data: [DONE]\n\n"));
            return Ok(());
        }
        let event: Value = serde_json::from_str(raw)
            .map_err(|e| UpstreamError::Transport(format!("invalid Chutes E2EE SSE event: {e}")))?;
        if let Some(init) = event.get("e2e_init").and_then(Value::as_str) {
            self.stream_key = Some(decrypt_chutes_stream_init(&self.response_sk, init)?);
            return Ok(());
        }
        if let Some(chunk) = event.get("e2e").and_then(Value::as_str) {
            let stream_key = self.stream_key.as_deref().ok_or_else(|| {
                UpstreamError::Transport(
                    "received Chutes E2EE stream chunk before e2e_init".to_string(),
                )
            })?;
            let mut plaintext = decrypt_chutes_stream_chunk(stream_key, chunk)?;
            plaintext.extend_from_slice(b"\n\n");
            self.pending.push_back(Bytes::from(plaintext));
            return Ok(());
        }
        if event.get("usage").is_some() {
            let mut line = Vec::with_capacity(raw.len() + 8);
            line.extend_from_slice(b"data: ");
            line.extend_from_slice(raw.as_bytes());
            line.extend_from_slice(b"\n\n");
            self.pending.push_back(Bytes::from(line));
            return Ok(());
        }
        if let Some(error) = event.get("e2e_error") {
            let body = serde_json::to_vec(&json!({ "error": error }))
                .map_err(|e| UpstreamError::Transport(e.to_string()))?;
            let mut line = Vec::with_capacity(body.len() + 8);
            line.extend_from_slice(b"data: ");
            line.extend_from_slice(&body);
            line.extend_from_slice(b"\n\n");
            self.pending.push_back(Bytes::from(line));
        }
        Ok(())
    }
}

impl Unpin for ChutesE2eeDecryptingStream {}

impl Stream for ChutesE2eeDecryptingStream {
    type Item = Result<Bytes, UpstreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(chunk) = this.pending.pop_front() {
            return Poll::Ready(Some(Ok(chunk)));
        }
        if this.finished {
            return Poll::Ready(None);
        }

        loop {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.extend_from_slice(&chunk);
                    if let Err(err) = this.process_buffer() {
                        this.finished = true;
                        return Poll::Ready(Some(Err(err)));
                    }
                    if let Some(chunk) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                }
                Poll::Ready(Some(Err(err))) => {
                    this.finished = true;
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(None) => {
                    if !this.buffer.is_empty() {
                        let line = std::mem::take(&mut this.buffer);
                        if let Err(err) = this.process_sse_line(&line) {
                            this.finished = true;
                            return Poll::Ready(Some(Err(err)));
                        }
                    }
                    this.finished = true;
                    if let Some(chunk) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                    return Poll::Ready(None);
                }
            }
        }
    }
}
