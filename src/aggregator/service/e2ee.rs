use super::config::normalize_downstream_domain;
use super::wire::{E2eeAadMode, E2eeDecryptor};

use serde_json::{json, Value};

use super::e2ee_crypto::{
    decrypt_request_payload, legacy_public_keys_match, normalize_legacy_public_key_for_replay,
    validate_aci_e2ee_nonce, validate_aci_payload_model, validate_legacy_e2ee_nonce,
    validate_payload_model, E2eeFieldCrypto,
};
use super::{
    AciService, E2eeError, E2eePreparedRequest, E2eeReplayKey, E2eeRequestContext,
    E2eeRequestParts, ServiceError,
};
use sha2::{Digest, Sha256};

use crate::aci::e2ee::{
    normalize_secp256k1_public_key_hex, E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519,
    E2EE_ALGO_SECP256K1_AESGCM, E2EE_VERSION_V1, E2EE_VERSION_V2,
};
use crate::aci::identity::{self, attestation_statement, report_data};
use crate::aci::keys::{
    ethereum_address_from_uncompressed_public_key, LEGACY_ALGO_ECDSA, LEGACY_ALGO_ED25519,
};
use crate::aci::types::{AttestationEnvelope, AttestationReport, Freshness, KeysetEndorsement};

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
        // A revoked keyset must stop producing acceptable reports (§4.7).
        if self.is_keyset_revoked() {
            return Err(ServiceError::KeysetRevoked);
        }
        let statement = attestation_statement(&self.keyset, nonce)?;
        let rd = report_data(&statement)?;
        let quote = self.quoter.get_quote(rd).await?;
        self.assemble_report(&rd, quote, domain).await
    }

    /// Legacy dstack-vllm-proxy compatibility report. The quote binds
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
        self.assemble_report(&rd, quote, domain).await
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
    fn legacy_signing_key_bytes(
        &self,
        signing_algo: Option<&str>,
    ) -> Result<Vec<u8>, ServiceError> {
        let signing_algo = signing_algo
            .unwrap_or(LEGACY_ALGO_ECDSA)
            .to_ascii_lowercase();
        let e2ee_keys = self.keys.e2ee_keys();
        let key_err =
            |msg: &str| ServiceError::Key(crate::aci::keys::KeyError::Crypto(msg.to_string()));
        match signing_algo.as_str() {
            LEGACY_ALGO_ECDSA => {
                let key = e2ee_keys
                    .iter()
                    .find(|key| key.algo == E2EE_ALGO_SECP256K1_AESGCM)
                    .ok_or_else(|| {
                        key_err("no secp256k1 E2EE key for legacy report_data binding")
                    })?;
                let address = ethereum_address_from_uncompressed_public_key(&key.public_key_hex)?;
                hex::decode(address.trim_start_matches("0x"))
                    .map_err(|e| key_err(&format!("invalid signing address hex: {e}")))
            }
            LEGACY_ALGO_ED25519 => {
                let key = e2ee_keys
                    .iter()
                    .find(|key| key.algo == E2EE_ALGO_LEGACY_ED25519)
                    .ok_or_else(|| key_err("no ed25519 E2EE key for legacy report_data binding"))?;
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
                self.keyset
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

    async fn assemble_report(
        &self,
        report_data_bytes: &[u8],
        quote: crate::aci::keys::Quote,
        domain: Option<&str>,
    ) -> Result<AttestationReport, ServiceError> {
        let endorsement_payload = identity::keyset_endorsement_payload(&self.keyset)?;
        let endorsement_sig = self.keys.sign_keyset_endorsement(&endorsement_payload)?;
        let endorsement = KeysetEndorsement {
            algo: self.keys.identity_public_key().algo,
            value_hex: hex::encode(endorsement_sig),
        };

        let now = self.clock.now_secs();
        let freshness = Freshness {
            fetched_at: now,
            stale_after: now + self.config.freshness_seconds,
        };

        let mut evidence = json!({
            "quote": hex::encode(&quote.raw_quote),
            "quote_report_data": hex::encode(&quote.report_data),
            "event_log": quote.event_log,
            "vm_config": quote.vm_config,
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

        let envelope = AttestationEnvelope {
            vendor: self.config.vendor.clone(),
            tee_type: self.config.tee_type.clone(),
            workload_keyset: self.keyset.clone(),
            report_data_hex: hex::encode(report_data_bytes),
            keyset_endorsement: endorsement,
            source_provenance: self.config.source_provenance.clone(),
            freshness,
            evidence,
        };

        Ok(AttestationReport {
            api_version: "aci/1".to_string(),
            workload_id: self.workload_id.clone(),
            workload_keyset_digest: self.workload_keyset_digest.clone(),
            attestation: envelope,
            service_capabilities: self.config.service_capabilities.clone(),
        })
    }

    pub(super) fn downstream_tls_binding(&self, domain: &str) -> Option<Value> {
        let domain = normalize_downstream_domain(domain)?;
        self.keyset
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
        self.keyset
            .tls_public_keys
            .iter()
            .any(|key| key.domain.is_some())
    }

    pub fn prepare_e2ee_v2_request(
        &self,
        parts: E2eeRequestParts<'_>,
        body: &[u8],
        endpoint_path: &str,
    ) -> Result<E2eePreparedRequest, E2eeError> {
        if parts.signing_algo.is_some() {
            return self.prepare_legacy_e2ee_request(parts, body, endpoint_path);
        }

        let version = parts.version.ok_or(E2eeError::HeaderMissing)?;
        if version != E2EE_VERSION_V2 {
            return Err(E2eeError::InvalidVersion);
        }
        let client_public_key = parts.client_public_key.ok_or(E2eeError::HeaderMissing)?;
        let model_public_key = parts.model_public_key.ok_or(E2eeError::HeaderMissing)?;
        let nonce = parts.nonce.ok_or(E2eeError::HeaderMissing)?;
        let timestamp = parts.timestamp.ok_or(E2eeError::HeaderMissing)?;

        validate_aci_e2ee_nonce(nonce)?;
        let timestamp = timestamp
            .parse::<u64>()
            .map_err(|_| E2eeError::InvalidTimestamp)?;
        let now = self.clock.now_secs();
        if now.abs_diff(timestamp) > 300 {
            return Err(E2eeError::InvalidTimestamp);
        }

        let client_public_key_hex = normalize_secp256k1_public_key_hex(client_public_key)
            .map_err(|_| E2eeError::InvalidPublicKey)?;
        let model_public_key_hex = normalize_secp256k1_public_key_hex(model_public_key)
            .map_err(|_| E2eeError::InvalidPublicKey)?;
        let selected_key = self
            .keyset
            .e2ee_public_keys
            .iter()
            .find(|key| {
                key.algo == E2EE_ALGO_SECP256K1_AESGCM
                    && normalize_secp256k1_public_key_hex(&key.public_key_hex)
                        .is_ok_and(|normalized| normalized == model_public_key_hex)
            })
            .ok_or(E2eeError::ModelKeyMismatch)?;

        let mut payload: Value =
            serde_json::from_slice(body).map_err(|_| E2eeError::DecryptionFailed)?;
        let request_model = validate_aci_payload_model(&payload)?;
        self.claim_e2ee_replay(
            client_public_key_hex.clone(),
            model_public_key_hex.clone(),
            nonce.to_string(),
            now,
        )?;
        let crypto = E2eeFieldCrypto {
            keys: self.keys.as_ref(),
            decryptor: E2eeDecryptor::AciV2 {
                key_id: selected_key.key_id.as_str(),
            },
            algo: selected_key.algo.as_str(),
            aad_mode: E2eeAadMode::AciV2,
            model: &request_model,
            nonce: Some(nonce),
            timestamp: Some(timestamp),
        };
        decrypt_request_payload(&crypto, endpoint_path, &mut payload)?;
        let decrypted_body =
            serde_json::to_vec(&payload).map_err(|_| E2eeError::DecryptionFailed)?;
        Ok(E2eePreparedRequest {
            decrypted_body,
            context: E2eeRequestContext {
                version: E2EE_VERSION_V2.to_string(),
                algo: selected_key.algo.clone(),
                aad_mode: E2eeAadMode::AciV2,
                request_model,
                client_public_key_hex,
                nonce: Some(nonce.to_string()),
                timestamp: Some(timestamp),
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
        let _selected_key = self
            .keyset
            .e2ee_public_keys
            .iter()
            .find(|key| {
                key.algo == signing_algo
                    && legacy_public_keys_match(
                        &signing_algo,
                        &key.public_key_hex,
                        model_public_key,
                    )
            })
            .ok_or(E2eeError::ModelKeyMismatch)?;

        let version_header = parts.version.unwrap_or("").trim();
        if !version_header.is_empty()
            && version_header != E2EE_VERSION_V1
            && version_header != E2EE_VERSION_V2
        {
            return Err(E2eeError::InvalidVersion);
        }
        let has_nonce = parts.nonce.is_some_and(|nonce| !nonce.is_empty());
        let has_timestamp = parts.timestamp.is_some_and(|ts| !ts.is_empty());
        if has_nonce ^ has_timestamp {
            return Err(E2eeError::HeaderMissing);
        }
        let use_v2 = version_header == E2EE_VERSION_V2 || (has_nonce && has_timestamp);
        let (version, aad_mode, nonce, timestamp) = if use_v2 {
            let nonce = parts.nonce.ok_or(E2eeError::HeaderMissing)?;
            validate_legacy_e2ee_nonce(nonce)?;
            let timestamp = parts
                .timestamp
                .ok_or(E2eeError::HeaderMissing)?
                .parse::<u64>()
                .map_err(|_| E2eeError::InvalidTimestamp)?;
            let now = self.clock.now_secs();
            if now.abs_diff(timestamp) > 300 {
                return Err(E2eeError::InvalidTimestamp);
            }
            self.claim_e2ee_replay(
                normalize_legacy_public_key_for_replay(&signing_algo, client_public_key)?,
                normalize_legacy_public_key_for_replay(&signing_algo, model_public_key)?,
                nonce.to_string(),
                now,
            )?;
            (
                E2EE_VERSION_V2.to_string(),
                E2eeAadMode::LegacyV2,
                Some(nonce.to_string()),
                Some(timestamp),
            )
        } else {
            (
                E2EE_VERSION_V1.to_string(),
                E2eeAadMode::LegacyV1,
                None,
                None,
            )
        };

        let mut payload: Value =
            serde_json::from_slice(body).map_err(|_| E2eeError::DecryptionFailed)?;
        let request_model = validate_payload_model(&payload)?;
        let crypto = E2eeFieldCrypto {
            keys: self.keys.as_ref(),
            decryptor: E2eeDecryptor::Legacy {
                signing_algo: &signing_algo,
            },
            algo: &signing_algo,
            aad_mode,
            model: &request_model,
            nonce: nonce.as_deref(),
            timestamp,
        };
        decrypt_request_payload(&crypto, endpoint_path, &mut payload)?;
        let decrypted_body =
            serde_json::to_vec(&payload).map_err(|_| E2eeError::DecryptionFailed)?;
        let client_public_key_hex =
            normalize_legacy_public_key_for_replay(&signing_algo, client_public_key)?;
        Ok(E2eePreparedRequest {
            decrypted_body,
            context: E2eeRequestContext {
                version,
                algo: signing_algo,
                aad_mode,
                request_model,
                client_public_key_hex,
                nonce,
                timestamp,
            },
        })
    }

    pub(super) fn claim_e2ee_replay(
        &self,
        client_public_key_hex: String,
        model_public_key_hex: String,
        nonce: String,
        now: u64,
    ) -> Result<(), E2eeError> {
        let mut guard = self
            .e2ee_replay
            .write()
            .expect("E2EE replay cache poisoned");
        guard.retain(|_, expires_at| *expires_at > now);
        let key = E2eeReplayKey {
            client_public_key_hex,
            model_public_key_hex,
            nonce,
        };
        if guard.contains_key(&key) {
            return Err(E2eeError::ReplayDetected);
        }
        guard.insert(key, now.saturating_add(300));
        Ok(())
    }
}
