use std::sync::Arc;

use k256::ecdsa::SigningKey;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use super::dcap::CachedAciDcapVerification;
use super::dstack::verify_dstack_kms_identity_custody;
use super::external::ExternalProviderVerifier;
use super::*;
use crate::aci::keys::ALGO_ECDSA_SECP256K1;
use crate::aci::receipt::{ChannelBinding, VerificationResult};
use crate::aci::types::{
    AttestationEnvelope, AttestationReport, Freshness, KeysetEndorsement, KeysetEpoch,
    PublicKeyMaterial, ServiceCapabilities, SourceProvenance, WorkloadIdentity, WorkloadKeyset,
};
use crate::aci::upstream::ChutesSessionStore;
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

fn signing_key(byte: u8) -> SigningKey {
    SigningKey::from_slice(&[byte; 32]).unwrap()
}

fn public_key_uncompressed_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_encoded_point(false).as_bytes())
}

fn public_key_compressed_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_sec1_bytes())
}

fn sign_recoverable(key: &SigningKey, message: &[u8]) -> String {
    let digest = Keccak256::new_with_prefix(message);
    let (signature, recid) = key.sign_digest_recoverable(digest).unwrap();
    let mut out = signature.to_vec();
    out.push(recid.to_byte());
    hex::encode(out)
}

fn custody_report(identity: &SigningKey, signature_chain: Vec<String>) -> AttestationReport {
    let identity_public_key = public_key_uncompressed_hex(identity);
    AttestationReport {
        api_version: "aci/1".to_string(),
        workload_id: "test-workload".to_string(),
        workload_keyset_digest: "test-keyset".to_string(),
        attestation: AttestationEnvelope {
            vendor: "test".to_string(),
            tee_type: "tdx".to_string(),
            workload_keyset: WorkloadKeyset {
                workload_identity: WorkloadIdentity {
                    public_key: PublicKeyMaterial {
                        algo: ALGO_ECDSA_SECP256K1.to_string(),
                        public_key_hex: identity_public_key.clone(),
                    },
                    subject: None,
                },
                keyset_epoch: KeysetEpoch {
                    version: 1,
                    not_after: u64::MAX,
                },
                receipt_signing_keys: Vec::new(),
                e2ee_public_keys: Vec::new(),
                tls_public_keys: Vec::new(),
            },
            report_data_hex: String::new(),
            keyset_endorsement: KeysetEndorsement {
                algo: ALGO_ECDSA_SECP256K1.to_string(),
                value_hex: String::new(),
            },
            source_provenance: SourceProvenance::default(),
            freshness: Freshness {
                fetched_at: 0,
                stale_after: u64::MAX,
            },
            evidence: json!({
                "key_custody": {
                    "provider": "dstack-kms",
                    "keys": [{
                        "role": "identity",
                        "path": "aci/identity/v1",
                        "purpose": "aci.identity.v1",
                        "algo": ALGO_ECDSA_SECP256K1,
                        "public_key": identity_public_key,
                        "signature_chain": signature_chain,
                    }]
                }
            }),
        },
        service_capabilities: ServiceCapabilities::default(),
    }
}

fn provider_script(provider: &str, verifier_id: &str, binding: Value) -> Vec<String> {
    let output = json!({
        "result": "verified",
        "verifier_id": verifier_id,
        "evidence": {
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoicHJvdmlkZXItbW9kZWwifQ==",
        },
        "channel_bindings": [binding],
        "provider_claims": {
            "fixture_provider": provider,
            "model_evidence_present": true,
        },
    })
    .to_string();
    let script = format!(
        r#"payload="$(cat)"
case "$payload" in
  *'"provider":"{provider}"'*'"model_id":"provider-model"'*) printf '%s' '{output}' ;;
  *) printf '%s' '{{"result":"failed","reason":"unexpected verifier input"}}' ;;
esac"#
    );
    vec!["/bin/sh".to_string(), "-c".to_string(), script]
}

fn counting_provider_script(
    counter_path: &std::path::Path,
    provider: &str,
    verifier_id: &str,
    binding: Value,
) -> Vec<String> {
    let output = json!({
        "result": "verified",
        "verifier_id": verifier_id,
        "evidence": {
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoicHJvdmlkZXItbW9kZWwifQ==",
        },
        "channel_bindings": [binding],
    })
    .to_string();
    let script = format!(
        r#"payload="$(cat)"
case "$payload" in
  *'"provider":"{provider}"'*'"model_id":"provider-model"'*)
    count="$(cat "$1" 2>/dev/null || printf '0')"
    count="$((count + 1))"
    printf '%s' "$count" > "$1"
    printf '%s' '{output}'
    ;;
  *) printf '%s' '{{"result":"failed","reason":"unexpected verifier input"}}' ;;
esac"#
    );
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        script,
        "provider-cache-test".to_string(),
        counter_path.display().to_string(),
    ]
}

async fn assert_provider_script_verifier(
    verifier: &dyn UpstreamVerifier,
    provider: &str,
    verifier_id: &str,
    expected_binding: ChannelBinding,
) {
    let event = verifier
        .verify(UpstreamVerificationRequest {
            upstream_name: "provider-upstream".to_string(),
            url_origin: Some("https://provider.example".to_string()),
            model_id: "provider-model".to_string(),
            forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
            required: true,
        })
        .await;

    assert_eq!(event.result, VerificationResult::Verified);
    assert_eq!(event.verifier_id, verifier_id);
    assert_eq!(event.channel_bindings, vec![expected_binding]);
    assert_eq!(
        event.provider_claims,
        Some(json!({
            "fixture_provider": provider,
            "model_evidence_present": true,
        }))
    );
}

#[tokio::test]
async fn chutes_provider_verifier_runs_provider_owned_external_verifier() {
    let verifier = ChutesProviderVerifier::with_command(
        provider_script(
            "chutes",
            "chutes/external-test/v1",
            json!({
                "type": "e2ee_public_key_sha256",
                "provider": "chutes",
                "key_id": "instance-a",
                "algorithm": "chutes-ml-kem-768",
                "public_key_sha256": "AA".repeat(32),
            }),
        ),
        5,
    )
    .unwrap();
    assert_provider_script_verifier(
        &verifier,
        "chutes",
        "chutes/external-test/v1",
        ChannelBinding::E2eePublicKeySha256 {
            provider: "chutes".to_string(),
            key_id: Some("instance-a".to_string()),
            algorithm: "chutes-ml-kem-768".to_string(),
            public_key_sha256: "aa".repeat(32),
        },
    )
    .await;
}

#[tokio::test]
async fn chutes_provider_verifier_records_provider_session_material() {
    let session_store = Arc::new(ChutesSessionStore::new());
    let output = json!({
        "result": "verified",
        "verifier_id": "chutes/external-test/v1",
        "evidence": {
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoicHJvdmlkZXItbW9kZWwifQ==",
        },
        "channel_bindings": [{
            "type": "e2ee_public_key_sha256",
            "provider": "chutes",
            "key_id": "instance-a",
            "algorithm": "chutes-ml-kem-768",
            "public_key_sha256": "AA".repeat(32),
        }],
        "chutes_session": {
            "chute_id": "chute-a",
            "nonce_expires_in": 55,
            "instances": [{
                "instance_id": "instance-a",
                "e2e_pubkey": "fixture-pubkey",
                "public_key_sha256": "AA".repeat(32),
                "nonces": ["nonce-a", "nonce-b"],
            }]
        }
    })
    .to_string();
    let script = format!("cat >/dev/null; printf '%s' '{output}'");
    let verifier = ChutesProviderVerifier::with_command_and_session_store(
        vec!["/bin/sh".to_string(), "-c".to_string(), script],
        5,
        session_store.clone(),
    )
    .unwrap();
    let event = verifier
        .verify(UpstreamVerificationRequest {
            upstream_name: "provider-upstream".to_string(),
            url_origin: Some("https://provider.example".to_string()),
            model_id: "provider-model".to_string(),
            forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
            required: true,
        })
        .await;

    assert_eq!(event.result, VerificationResult::Verified);
    assert_eq!(session_store.pooled_nonce_count("chute-a"), 2);
}

#[tokio::test]
async fn tinfoil_provider_verifier_runs_provider_owned_external_verifier() {
    let verifier = TinfoilProviderVerifier::with_command(
        provider_script(
            "tinfoil",
            "tinfoil/external-test/v1",
            json!({
                "type": "tls_spki_sha256",
                "origin": "https://provider.example",
                "spki_sha256": "AA".repeat(32),
            }),
        ),
        5,
    )
    .unwrap();
    assert_provider_script_verifier(
        &verifier,
        "tinfoil",
        "tinfoil/external-test/v1",
        ChannelBinding::TlsSpkiSha256 {
            origin: "https://provider.example".to_string(),
            spki_sha256: "aa".repeat(32),
        },
    )
    .await;
}

#[tokio::test]
async fn near_ai_provider_verifier_runs_provider_owned_external_verifier() {
    let verifier = NearAiProviderVerifier::with_command(
        provider_script(
            "near-ai",
            "near-ai/external-test/v1",
            json!({
                "type": "tls_spki_sha256",
                "origin": "https://provider.example",
                "spki_sha256": "AA".repeat(32),
            }),
        ),
        5,
    )
    .unwrap();
    assert_provider_script_verifier(
        &verifier,
        "near-ai",
        "near-ai/external-test/v1",
        ChannelBinding::TlsSpkiSha256 {
            origin: "https://provider.example".to_string(),
            spki_sha256: "aa".repeat(32),
        },
    )
    .await;
}

#[tokio::test]
async fn phala_direct_provider_verifier_runs_provider_owned_external_verifier() {
    let verifier = PhalaDirectProviderVerifier::with_command(
        provider_script(
            "phala-direct",
            "phala-direct/external-test/v1",
            json!({
                "type": "tls_spki_sha256",
                "origin": "https://provider.example",
                "spki_sha256": "AA".repeat(32),
            }),
        ),
        5,
    )
    .unwrap();
    assert_provider_script_verifier(
        &verifier,
        "phala-direct",
        "phala-direct/external-test/v1",
        ChannelBinding::TlsSpkiSha256 {
            origin: "https://provider.example".to_string(),
            spki_sha256: "aa".repeat(32),
        },
    )
    .await;
}

#[tokio::test]
async fn provider_external_verifier_rejects_verified_without_binding() {
    let verifier = ChutesProviderVerifier::with_command(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "cat >/dev/null; printf '%s' '{\"result\":\"verified\",\"verifier_id\":\"bad/v1\"}'"
                .to_string(),
        ],
        5,
    )
    .unwrap();
    let event = verifier
        .verify(UpstreamVerificationRequest {
            upstream_name: "provider-upstream".to_string(),
            url_origin: Some("https://provider.example".to_string()),
            model_id: "provider-model".to_string(),
            forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
            required: true,
        })
        .await;

    assert_eq!(event.result, VerificationResult::Failed);
    assert!(event
        .reason
        .unwrap()
        .contains("without an enforceable channel binding"));
}

#[tokio::test]
async fn external_provider_verifier_caches_verified_bindings() {
    let counter_path = std::env::temp_dir().join(format!(
        "private-ai-gateway-provider-cache-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&counter_path);
    let verifier = ExternalProviderVerifier::with_command_and_cache(
        "tinfoil",
        counting_provider_script(
            &counter_path,
            "tinfoil",
            "tinfoil/external-test/v1",
            json!({
                "type": "tls_spki_sha256",
                "origin": "https://provider.example",
                "spki_sha256": "AA".repeat(32),
            }),
        ),
        5,
        300,
    )
    .unwrap();
    let request = UpstreamVerificationRequest {
        upstream_name: "provider-upstream".to_string(),
        url_origin: Some("https://provider.example".to_string()),
        model_id: "provider-model".to_string(),
        forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
        required: true,
    };
    let first = verifier.verify(request.clone()).await;
    let second_request = UpstreamVerificationRequest {
        forwarded_body_hash: format!("sha256:{}", "33".repeat(32)),
        required: false,
        ..request
    };
    let second = verifier.verify(second_request.clone()).await;

    assert_eq!(first.result, VerificationResult::Verified);
    assert_eq!(second.result, VerificationResult::Verified);
    assert!(!second.required);
    assert_eq!(
        std::fs::read_to_string(&counter_path).unwrap(),
        "1",
        "cached provider verifier should not run the external verifier twice"
    );

    verifier.invalidate(&second_request);
    let third = verifier.verify(second_request).await;
    assert_eq!(third.result, VerificationResult::Verified);
    assert_eq!(
        std::fs::read_to_string(&counter_path).unwrap(),
        "2",
        "invalidating the provider verifier cache should force a fresh external verifier run"
    );
    let _ = std::fs::remove_file(counter_path);
}

#[tokio::test]
async fn router_shares_one_channel_verification_across_models() {
    // Security-critical: a router keys its verifier cache on the channel, not the
    // model, so verifying a second model reuses the first model's verification
    // (one external run) and event_for re-tags it with the requesting model. A
    // per-model provider must NOT share — each model is its own channel.
    let output = json!({
        "result": "verified",
        "verifier_id": "router-cache-test/v1",
        "evidence": {
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoicm91dGVyIn0=",
        },
        "channel_bindings": [{
            "type": "tls_spki_sha256",
            "origin": "https://router.example",
            "spki_sha256": "AA".repeat(32),
        }],
    })
    .to_string();
    // Counts every external run and verifies any model (unlike
    // counting_provider_script, which is pinned to one model_id).
    let script = format!(
        r#"cat >/dev/null
count="$(cat "$1" 2>/dev/null || printf '0')"
count="$((count + 1))"
printf '%s' "$count" > "$1"
printf '%s' '{output}'"#
    );
    for (provider, expected_runs) in [("near-ai", "1"), ("phala-direct", "2")] {
        let counter_path = std::env::temp_dir().join(format!(
            "private-ai-gateway-router-cache-test-{}-{provider}",
            std::process::id(),
        ));
        let _ = std::fs::remove_file(&counter_path);
        let verifier = ExternalProviderVerifier::with_command_and_cache(
            provider,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                script.clone(),
                "router-cache-test".to_string(),
                counter_path.display().to_string(),
            ],
            5,
            300,
        )
        .unwrap();
        let base = UpstreamVerificationRequest {
            upstream_name: "router-upstream".to_string(),
            url_origin: Some("https://router.example".to_string()),
            model_id: "model-a".to_string(),
            forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
            required: true,
        };
        let _ = verifier.verify(base.clone()).await;
        let second = verifier
            .verify(UpstreamVerificationRequest {
                model_id: "model-b".to_string(),
                ..base
            })
            .await;

        assert_eq!(second.result, VerificationResult::Verified);
        // The served event always reports the requesting model, even on reuse.
        assert_eq!(second.model_id, "model-b");
        assert_eq!(
            std::fs::read_to_string(&counter_path).unwrap(),
            expected_runs,
            "{provider}: external verifier runs (a router reuses one channel \
             verification across models; a per-model provider verifies each)"
        );
        let _ = std::fs::remove_file(counter_path);
    }
}

#[tokio::test]
async fn external_provider_refresh_keeps_existing_cache_on_failure() {
    let counter_path = std::env::temp_dir().join(format!(
        "private-ai-gateway-provider-refresh-cache-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&counter_path);
    let output = json!({
        "result": "verified",
        "verifier_id": "tinfoil/external-test/v1",
        "evidence": {
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoicHJvdmlkZXItbW9kZWwifQ==",
        },
        "channel_bindings": [{
            "type": "tls_spki_sha256",
            "origin": "https://provider.example",
            "spki_sha256": "AA".repeat(32),
        }],
    })
    .to_string();
    let script = format!(
        r#"cat >/dev/null
count="$(cat "$1" 2>/dev/null || printf '0')"
count="$((count + 1))"
printf '%s' "$count" > "$1"
if [ "$count" -eq 1 ]; then
  printf '%s' '{output}'
else
  printf '%s\n' 'refresh failed' >&2
  exit 42
fi"#
    );
    let verifier = ExternalProviderVerifier::with_command_and_cache(
        "tinfoil",
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            script,
            "provider-refresh-cache-test".to_string(),
            counter_path.display().to_string(),
        ],
        5,
        300,
    )
    .unwrap();
    let request = UpstreamVerificationRequest {
        upstream_name: "provider-upstream".to_string(),
        url_origin: Some("https://provider.example".to_string()),
        model_id: "provider-model".to_string(),
        forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
        required: true,
    };

    let first = verifier.verify(request.clone()).await;
    let refresh = verifier.refresh(request.clone()).await;
    let after_failed_refresh = verifier.verify(request).await;

    assert_eq!(first.result, VerificationResult::Verified);
    assert_eq!(refresh.result, VerificationResult::Failed);
    assert_eq!(after_failed_refresh.result, VerificationResult::Verified);
    assert_eq!(
        std::fs::read_to_string(&counter_path).unwrap(),
        "2",
        "failed refresh must not remove the previous verified cache entry"
    );
    let _ = std::fs::remove_file(counter_path);
}

#[test]
fn cached_aci_dcap_verification_preserves_channel_bindings() {
    let cached = CachedAciDcapVerification {
        expires_at: 10,
        vendor: "gpu-a".to_string(),
        evidence: Some(json!({
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJwcm92aWRlciI6ImdwdS1hIiwiZml4dHVyZSI6ImF0dGVzdGF0aW9uLXJlcG9ydCJ9",
        })),
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://gpu-a.example".to_string(),
            spki_sha256: "aa".repeat(32),
        }],
    };
    let event = cached.event_for(
        UpstreamVerificationRequest {
            upstream_name: "ignored".to_string(),
            url_origin: Some("https://gpu-a.example".to_string()),
            model_id: "model-a".to_string(),
            forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
            required: true,
        },
        "aci-dcap/v1",
    );

    assert_eq!(event.result, VerificationResult::Verified);
    assert_eq!(event.channel_bindings, cached.channel_bindings);
}

#[test]
fn verifies_dstack_kms_identity_key_custody_chain() {
    let root = signing_key(1);
    let app = signing_key(2);
    let identity = signing_key(3);
    let app_id = [0xab; 20];

    let purpose_message = format!("aci.identity.v1:{}", public_key_compressed_hex(&identity));
    let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id.as_slice(),
        &app.verifying_key().to_sec1_bytes(),
    ]
    .concat();
    let app_signature = sign_recoverable(&root, &root_message);
    let report = custody_report(&identity, vec![purpose_signature, app_signature]);
    let policy = AciDcapVerifierPolicy::new(
        vec![report.workload_id.clone()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&root)],
    )
    .unwrap();

    verify_dstack_kms_identity_custody(&report, &app_id, &policy).unwrap();
}

#[test]
fn rejects_dstack_kms_identity_key_custody_under_unaccepted_root() {
    let root = signing_key(1);
    let other_root = signing_key(4);
    let app = signing_key(2);
    let identity = signing_key(3);
    let app_id = [0xab; 20];

    let purpose_message = format!("aci.identity.v1:{}", public_key_compressed_hex(&identity));
    let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id.as_slice(),
        &app.verifying_key().to_sec1_bytes(),
    ]
    .concat();
    let app_signature = sign_recoverable(&root, &root_message);
    let report = custody_report(&identity, vec![purpose_signature, app_signature]);
    let policy = AciDcapVerifierPolicy::new(
        vec![report.workload_id.clone()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&other_root)],
    )
    .unwrap();

    let err = verify_dstack_kms_identity_custody(&report, &app_id, &policy)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "dstack KMS root public key is not accepted by verifier policy"
    );
}
