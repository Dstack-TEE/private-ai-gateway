use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::config::normalize_downstream_domain;
use super::e2ee_crypto::{
    decrypt_legacy_request_payload, legacy_public_keys_match, validate_payload_model,
};
use super::wire::E2eeMode;
use super::{
    AciService, E2eeError, E2eePreparedRequest, E2eeRequestContext, E2eeRequestParts, ServiceError,
};

use crate::aci::e2ee::{
    normalize_x25519_public_key_hex, x25519_public_key_from_hex, E2EE_ALGO_LEGACY_ECDSA,
    E2EE_ALGO_LEGACY_ED25519, E2EE_ALGO_X25519_AESGCM, E2EE_CONTEXT_REQUEST, E2EE_VERSION_V1,
    E2EE_VERSION_V3,
};
use crate::aci::identity::{attestation_statement, report_data};
use crate::aci::keys::{LEGACY_ALGO_ECDSA, LEGACY_ALGO_ED25519};
use crate::aci::types::{AttestationEnvelope, AttestationReport};

impl AciService {
    pub async fn attestation_report(
        &self,
        nonce: Option<String>,
    ) -> Result<AttestationReport, ServiceError> {
        self.attestation_report_for_domain(nonce, None).await
    }

    /// Build a fresh attestation report and annotate it with the downstream
    /// TLS binding selected for `domain`, when the configured keyset contains
    /// an exact domain match.
    pub async fn attestation_report_for_domain(
        &self,
        nonce: Option<String>,
        domain: Option<&str>,
    ) -> Result<AttestationReport, ServiceError> {
        let statement = attestation_statement(self.keyset.digest(), nonce.as_deref())?;
        let rd = report_data(&statement);
        let quote = self.quoter.get_quote(rd).await?;
        self.assemble_report(&rd, quote, domain)
    }

    /// Legacy dstack-vllm-proxy compatibility report (§13). The quote binds
    /// `report_data = identity(32) ‖ nonce(32)` exactly as the proxy does, so
    /// old clients verify against this gateway:
    ///
    /// * `signing_algo`: `ecdsa` → identity starts with the 20-byte secp256k1
    ///   Ethereum address; `ed25519` → the 32-byte ed25519 public key. (Same
    ///   key the shim surfaces as `signing_address`.)
    /// * `version`: 1 → identity is the signing key right-padded to 32 bytes;
    ///   2 → identity is `SHA256(signing_key ‖ tls_spki_fingerprint)`.
    pub async fn legacy_attestation_report_for_domain(
        &self,
        signing_algo: Option<&str>,
        version: u32,
        nonce: Option<&str>,
        domain: Option<&str>,
    ) -> Result<AttestationReport, ServiceError> {
        let rd = self.legacy_report_data(signing_algo, version, nonce, domain)?;
        let quote = self.quoter.get_quote_raw(rd).await?;
        self.assemble_report(&rd, quote, domain)
    }

    /// Build `identity(32) ‖ nonce(32)`. The nonce is the raw 32 bytes when it
    /// decodes as 32-byte hex, otherwise `sha256(nonce)` — both forms the
    /// legacy verifier accepts. An absent nonce leaves the trailing 32 bytes
    /// zeroed.
    fn legacy_report_data(
        &self,
        signing_algo: Option<&str>,
        version: u32,
        nonce: Option<&str>,
        domain: Option<&str>,
    ) -> Result<[u8; 64], ServiceError> {
        let signing_key = self.legacy_signing_key_bytes(signing_algo)?;
        let mut rd = [0u8; 64];
        if version >= 2 {
            // v2 identity = SHA256(signing_key ‖ TLS SPKI fingerprint).
            let cert_fingerprint = self.legacy_tls_spki_fingerprint(domain)?;
            let mut hasher = Sha256::new();
            hasher.update(&signing_key);
            hasher.update(cert_fingerprint);
            rd[..32].copy_from_slice(&hasher.finalize());
        } else {
            // v1 identity = signing key right-padded to 32 bytes.
            rd[..signing_key.len()].copy_from_slice(&signing_key);
        }
        if let Some(nonce) = nonce {
            let nonce_bytes = match hex::decode(nonce) {
                Ok(bytes) if bytes.len() == 32 => bytes,
                _ => Sha256::digest(nonce.as_bytes()).to_vec(),
            };
            rd[32..].copy_from_slice(&nonce_bytes);
        }
        Ok(rd)
    }

    /// The signing-key identity bytes the legacy report_data binds, matching the
    /// `signing_address` the shim reports: the 20-byte secp256k1 Ethereum
    /// address for `ecdsa`, or the 32-byte ed25519 public key for `ed25519`.
    /// Legacy keys come from [`crate::aci::keys::KeyProvider::legacy_e2ee_keys`];
    /// they are not part of the ACI keyset.
    fn legacy_signing_key_bytes(
        &self,
        signing_algo: Option<&str>,
    ) -> Result<Vec<u8>, ServiceError> {
        let signing_algo = signing_algo
            .unwrap_or(LEGACY_ALGO_ECDSA)
            .to_ascii_lowercase();
        let legacy_keys = self.keys.legacy_e2ee_keys();
        let key_err =
            |msg: &str| ServiceError::Key(crate::aci::keys::KeyError::Crypto(msg.to_string()));
        match signing_algo.as_str() {
            LEGACY_ALGO_ECDSA => {
                let key = legacy_keys
                    .iter()
                    .find(|key| key.algo == E2EE_ALGO_LEGACY_ECDSA)
                    .ok_or_else(|| {
                        key_err("no secp256k1 legacy key for legacy report_data binding")
                    })?;
                let address = crate::aci::keys::ethereum_address_from_uncompressed_public_key(
                    &key.public_key_hex,
                )?;
                hex::decode(address.trim_start_matches("0x"))
                    .map_err(|e| key_err(&format!("invalid signing address hex: {e}")))
            }
            LEGACY_ALGO_ED25519 => {
                let key = legacy_keys
                    .iter()
                    .find(|key| key.algo == E2EE_ALGO_LEGACY_ED25519)
                    .ok_or_else(|| {
                        key_err("no ed25519 legacy key for legacy report_data binding")
                    })?;
                hex::decode(&key.public_key_hex)
                    .map_err(|e| key_err(&format!("invalid ed25519 public key hex: {e}")))
            }
            other => Err(ServiceError::Key(
                crate::aci::keys::KeyError::UnsupportedAlgo(other.to_string()),
            )),
        }
    }

    /// The 32-byte TLS SPKI fingerprint bound by an attestation-v2 report, for
    /// the request's `domain`. Errors when no matching TLS key is published
    /// (v2 cannot be produced without one — matching the proxy).
    fn legacy_tls_spki_fingerprint(&self, domain: Option<&str>) -> Result<[u8; 32], ServiceError> {
        let key_err = |msg: String| ServiceError::Key(crate::aci::keys::KeyError::Crypto(msg));
        let spki_hex = domain
            .and_then(normalize_downstream_domain)
            .and_then(|domain| {
                self.keyset()
                    .tls_public_keys
                    .iter()
                    .find(|key| key.domain.as_deref() == Some(domain.as_str()))
            })
            .map(|key| key.spki_sha256_hex.clone())
            .ok_or_else(|| {
                key_err(
                    "attestation version 2 requires a published TLS SPKI for the request host"
                        .to_string(),
                )
            })?;
        let bytes =
            hex::decode(&spki_hex).map_err(|e| key_err(format!("invalid TLS SPKI hex: {e}")))?;
        bytes
            .try_into()
            .map_err(|_| key_err("TLS SPKI fingerprint is not 32 bytes".to_string()))
    }

    fn assemble_report(
        &self,
        report_data_bytes: &[u8],
        quote: crate::aci::keys::Quote,
        domain: Option<&str>,
    ) -> Result<AttestationReport, ServiceError> {
        let mut evidence = json!({
            "quote": hex::encode(&quote.raw_quote),
            "quote_report_data": hex::encode(&quote.report_data),
            "event_log": quote.event_log,
            "vm_config": quote.vm_config,
            "app_compose": quote.app_compose,
        });
        let key_custody = self.keys.key_custody_evidence();
        if !key_custody.is_null() {
            evidence["key_custody"] = key_custody;
        }
        if self.requires_host_for_downstream_tls_binding() {
            // Domain-scoped TLS keys publish one SPKI per public hostname. The
            // report must therefore be requested through a known Host so the
            // relying client pins the SPKI for that same hostname.
            let domain = domain.ok_or(ServiceError::DownstreamTlsDomainMissing)?;
            let binding = self
                .downstream_tls_binding(domain)
                .ok_or_else(|| ServiceError::DownstreamTlsDomainUnknown(domain.to_string()))?;
            evidence["downstream_tls_binding"] = binding;
        } else if let Some(binding) = domain.and_then(|domain| self.downstream_tls_binding(domain))
        {
            evidence["downstream_tls_binding"] = binding;
        }

        Ok(AttestationReport {
            api_version: "aci/1".to_string(),
            workload_keyset_digest: self.keyset.digest().to_string(),
            attestation: AttestationEnvelope {
                tee_type: self.config.tee_type.clone(),
                workload_keyset_b64: BASE64.encode(self.keyset.bytes()),
                report_data_hex: hex::encode(report_data_bytes),
                source_provenance: self.config.source_provenance.clone(),
                evidence,
            },
            service_capabilities: self.config.service_capabilities.clone(),
        })
    }

    pub(super) fn downstream_tls_binding(&self, domain: &str) -> Option<Value> {
        let domain = normalize_downstream_domain(domain)?;
        self.keyset()
            .tls_public_keys
            .iter()
            .find(|key| key.domain.as_deref() == Some(domain.as_str()))
            .map(|key| {
                json!({
                    "domain": domain,
                    "spki_sha256": key.spki_sha256_hex,
                })
            })
    }

    fn requires_host_for_downstream_tls_binding(&self) -> bool {
        self.keyset()
            .tls_public_keys
            .iter()
            .any(|key| key.domain.is_some())
    }

    /// Terminate the client-facing E2EE body: ACI v3 (`X-E2EE-Version: 3`)
    /// unseals the whole request to the client's exact original bytes (§7.2);
    /// `X-Signing-Algo` routes to the §13 legacy per-field mode.
    pub fn prepare_e2ee_request(
        &self,
        parts: E2eeRequestParts<'_>,
        body: &[u8],
        endpoint_path: &str,
    ) -> Result<E2eePreparedRequest, E2eeError> {
        if parts.signing_algo.is_some() {
            return self.prepare_legacy_e2ee_request(parts, body, endpoint_path);
        }

        let version = parts.version.ok_or(E2eeError::HeaderMissing)?;
        if version.trim() != E2EE_VERSION_V3 {
            return Err(E2eeError::InvalidVersion);
        }
        let client_public_key = parts.client_public_key.ok_or(E2eeError::HeaderMissing)?;
        let model_public_key = parts.model_public_key.ok_or(E2eeError::HeaderMissing)?;
        let client_public_key_hex = normalize_x25519_public_key_hex(client_public_key)
            .map_err(|_| E2eeError::InvalidPublicKey)?;
        // §7.4: reject a malformed model key as invalid before the verbatim
        // keyset match below, which would otherwise misreport it as a mismatch.
        x25519_public_key_from_hex(model_public_key).map_err(|_| E2eeError::InvalidPublicKey)?;

        // §7.4: X-Model-Pub-Key must equal an attested §7.1 keyset entry
        // verbatim (§3: hex, no 0x prefix), proving the client encrypted to a
        // key it could have verified.
        let selected_key = self
            .keyset()
            .e2ee_public_keys
            .iter()
            .find(|key| {
                key.algo == E2EE_ALGO_X25519_AESGCM && key.public_key_hex == model_public_key
            })
            .ok_or(E2eeError::ModelKeyMismatch)?;

        // §7.2 envelope: {"model": "<id>", "sealed_b64": "<base64>"}.
        let envelope: Value =
            serde_json::from_slice(body).map_err(|_| E2eeError::DecryptionFailed)?;
        let request_model = envelope
            .get("model")
            .and_then(Value::as_str)
            .ok_or(E2eeError::DecryptionFailed)?
            .to_string();
        let sealed = envelope
            .get("sealed_b64")
            .and_then(Value::as_str)
            .and_then(|b64| BASE64.decode(b64.as_bytes()).ok())
            .ok_or(E2eeError::DecryptionFailed)?;

        let decrypted_body = self
            .keys
            .decrypt_e2ee(
                &selected_key.key_id,
                &sealed,
                E2EE_CONTEXT_REQUEST,
                &request_model,
                &client_public_key_hex,
            )
            .map_err(|_| E2eeError::DecryptionFailed)?;

        Ok(E2eePreparedRequest {
            decrypted_body,
            context: E2eeRequestContext {
                algo: selected_key.algo.clone(),
                mode: E2eeMode::V3,
                request_model,
                client_public_key_hex,
            },
        })
    }

    pub(super) fn prepare_legacy_e2ee_request(
        &self,
        parts: E2eeRequestParts<'_>,
        body: &[u8],
        endpoint_path: &str,
    ) -> Result<E2eePreparedRequest, E2eeError> {
        let signing_algo = parts
            .signing_algo
            .ok_or(E2eeError::HeaderMissing)?
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            signing_algo.as_str(),
            E2EE_ALGO_LEGACY_ECDSA | E2EE_ALGO_LEGACY_ED25519
        ) {
            return Err(E2eeError::InvalidSigningAlgo);
        }
        let client_public_key = parts.client_public_key.ok_or(E2eeError::HeaderMissing)?;
        let model_public_key = parts.model_public_key.ok_or(E2eeError::HeaderMissing)?;
        // The supplied model key must match this service's legacy key for the
        // selected algo; decryption itself is keyed by `signing_algo`.
        self.keys
            .legacy_e2ee_keys()
            .into_iter()
            .find(|key| {
                key.algo == signing_algo
                    && legacy_public_keys_match(
                        &signing_algo,
                        &key.public_key_hex,
                        model_public_key,
                    )
            })
            .ok_or(E2eeError::ModelKeyMismatch)?;

        // Only the no-AAD legacy v1 mode survives; a versioned request here is
        // asking for something else (drop X-Signing-Algo and use ACI v3).
        let version_header = parts.version.unwrap_or("").trim();
        if !version_header.is_empty() && version_header != E2EE_VERSION_V1 {
            return Err(E2eeError::InvalidVersion);
        }

        let mut payload: Value =
            serde_json::from_slice(body).map_err(|_| E2eeError::DecryptionFailed)?;
        let request_model = validate_payload_model(&payload)?;
        decrypt_legacy_request_payload(
            self.keys.as_ref(),
            &signing_algo,
            endpoint_path,
            &mut payload,
        )?;
        let decrypted_body =
            serde_json::to_vec(&payload).map_err(|_| E2eeError::DecryptionFailed)?;
        let client_public_key_hex =
            super::e2ee_crypto::normalize_legacy_public_key(&signing_algo, client_public_key)?;
        Ok(E2eePreparedRequest {
            decrypted_body,
            context: E2eeRequestContext {
                algo: signing_algo,
                mode: E2eeMode::LegacyV1,
                request_model,
                client_public_key_hex,
            },
        })
    }
}
