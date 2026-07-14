use std::sync::Arc;

use k256::ecdsa::SigningKey;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use super::aci_service::{aci_report_tls_channel_bindings, CachedAciServiceVerification};
use super::dstack::verify_dstack_kms_receipt_custody;
use super::external::ExternalProviderVerifier;
use super::*;
use crate::aci::keys::ALGO_ED25519;
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::types::{KeyedPublicKey, TlsSpki, WorkloadKeyset};
use crate::aci::upstream::ChutesSessionStore;
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};
use crate::aggregator::upstream_config::AttestationScope;

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

fn keyset_with_tls(tls_public_keys: Vec<TlsSpki>) -> WorkloadKeyset {
    WorkloadKeyset {
        subject: None,
        not_after: u64::MAX,
        receipt_signing_keys: Vec::new(),
        e2ee_public_keys: Vec::new(),
        tls_public_keys,
    }
}

/// A keyset + custody evidence for the receipt key derived from the shared
/// 32-byte KMS scalar in `receipt_scalar`: the keyset lists its Ed25519
/// public key, and the evidence publishes the k256 counterpart the KMS chain
/// covers.
fn receipt_custody_fixture(
    receipt_scalar: [u8; 32],
    signature_chain: Vec<String>,
) -> (WorkloadKeyset, Value) {
    let ed25519 = ed25519_dalek::SigningKey::from_bytes(&receipt_scalar);
    let ed25519_public = hex::encode(ed25519.verifying_key().as_bytes());
    let kms_public = public_key_compressed_hex(&SigningKey::from_slice(&receipt_scalar).unwrap());
    let keyset = WorkloadKeyset {
        subject: Some("test-subject".to_string()),
        not_after: u64::MAX,
        receipt_signing_keys: vec![KeyedPublicKey {
            key_id: "receipt-1".to_string(),
            algo: ALGO_ED25519.to_string(),
            public_key_hex: ed25519_public.clone(),
        }],
        e2ee_public_keys: Vec::new(),
        tls_public_keys: Vec::new(),
    };
    let evidence = json!({
        "key_custody": {
            "provider": "dstack-kms",
            "keys": [{
                "role": "receipt",
                "path": "aci/receipt-ed25519/v1",
                "purpose": "aci.receipt.ed25519.v1",
                "algo": ALGO_ED25519,
                "public_key": ed25519_public,
                "kms_public_key": kms_public,
                "signature_chain": signature_chain,
            }]
        }
    });
    (keyset, evidence)
}

/// The scope token a stub verifier declares. Only routers declare a scope in
/// production (near.ai / Tinfoil); per-model and per-instance verifiers omit it
/// and the seam accepts `None`. Stubs mirror that so the real accept paths run.
fn declared_scope(provider: &str) -> Option<&'static str> {
    match provider {
        "near-ai" | "tinfoil" => Some("router"),
        _ => None,
    }
}

fn provider_script(provider: &str, verifier_id: &str, binding: Value) -> Vec<String> {
    let mut output = json!({
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
    });
    if let Some(scope) = declared_scope(provider) {
        output["attested_scope"] = json!(scope);
    }
    let output = output.to_string();
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
    let mut output = json!({
        "result": "verified",
        "verifier_id": verifier_id,
        "evidence": {
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoicHJvdmlkZXItbW9kZWwifQ==",
        },
        "channel_bindings": [binding],
    });
    if let Some(scope) = declared_scope(provider) {
        output["attested_scope"] = json!(scope);
    }
    let output = output.to_string();
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
        AttestationScope::PerRouter,
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
    for (provider, scope, expected_runs) in [
        ("near-ai", AttestationScope::PerRouter, "1"),
        ("phala-direct", AttestationScope::PerModel, "2"),
    ] {
        // The stub declares the provider's scope (routers) or omits it (per-model)
        // just like production, so the fail-closed seam accepts it.
        let mut output = json!({
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
        });
        if let Some(token) = declared_scope(provider) {
            output["attested_scope"] = json!(token);
        }
        let output = output.to_string();
        // Counts every external run and verifies any model (unlike
        // counting_provider_script, which is pinned to one model_id).
        let script = format!(
            r#"cat >/dev/null
count="$(cat "$1" 2>/dev/null || printf '0')"
count="$((count + 1))"
printf '%s' "$count" > "$1"
printf '%s' '{output}'"#
        );
        let counter_path = std::env::temp_dir().join(format!(
            "private-ai-gateway-router-cache-test-{}-{provider}",
            std::process::id(),
        ));
        let _ = std::fs::remove_file(&counter_path);
        let verifier = ExternalProviderVerifier::with_command_and_cache(
            provider,
            scope,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                script,
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
async fn scope_seam_rejects_mismatched_missing_and_unknown_scopes() {
    // The fail-closed seam: a Verified result must attest the scope its provider
    // is declared to use. A router that comes back model-scoped, undeclared, or
    // with a garbage token is rejected before the event is trusted or cached;
    // a non-router that omits the scope (the production path) is accepted.
    async fn verify_with_scope(
        provider: &'static str,
        scope: AttestationScope,
        declared: Option<&str>,
    ) -> UpstreamVerifiedEvent {
        let mut output = json!({
            "result": "verified",
            "verifier_id": "scope-seam-test/v1",
            "channel_bindings": [{
                "type": "tls_spki_sha256",
                "origin": "https://provider.example",
                "spki_sha256": "AA".repeat(32),
            }],
        });
        if let Some(token) = declared {
            output["attested_scope"] = json!(token);
        }
        let script = format!("cat >/dev/null; printf '%s' '{}'", output);
        let verifier = ExternalProviderVerifier::with_command(
            provider,
            scope,
            vec!["/bin/sh".to_string(), "-c".to_string(), script],
            5,
        )
        .unwrap();
        verifier
            .verify(UpstreamVerificationRequest {
                upstream_name: "scope-seam-upstream".to_string(),
                url_origin: Some("https://provider.example".to_string()),
                model_id: "model-a".to_string(),
                forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
                required: true,
            })
            .await
    }

    // Router declaring the wrong (model) scope → rejected.
    let mismatch = verify_with_scope("near-ai", AttestationScope::PerRouter, Some("model")).await;
    assert_eq!(mismatch.result, VerificationResult::Failed);
    assert!(mismatch.reason.unwrap().contains("per-router"));

    // Router declaring no scope at all → rejected (it must declare).
    let missing = verify_with_scope("near-ai", AttestationScope::PerRouter, None).await;
    assert_eq!(missing.result, VerificationResult::Failed);
    assert!(missing.reason.unwrap().contains("did not declare"));

    // Any verifier returning a garbage token → rejected.
    let unknown = verify_with_scope("near-ai", AttestationScope::PerRouter, Some("galaxy")).await;
    assert_eq!(unknown.result, VerificationResult::Failed);
    assert!(unknown.reason.unwrap().contains("unrecognized"));

    // Per-instance provider declaring its matching scope → accepted.
    let instance =
        verify_with_scope("chutes", AttestationScope::PerInstance, Some("instance")).await;
    assert_eq!(instance.result, VerificationResult::Verified);

    // Per-instance provider declaring router scope → rejected (mismatch the other
    // direction, so the seam isn't router-only).
    let instance_mismatch =
        verify_with_scope("chutes", AttestationScope::PerInstance, Some("router")).await;
    assert_eq!(instance_mismatch.result, VerificationResult::Failed);
    assert!(instance_mismatch.reason.unwrap().contains("per-instance"));

    // Per-model provider that omits the scope → accepted (Phala-direct / Chutes
    // don't declare one).
    let omitted = verify_with_scope("phala-direct", AttestationScope::PerModel, None).await;
    assert_eq!(omitted.result, VerificationResult::Verified);
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
        "attested_scope": "router",
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
        AttestationScope::PerRouter,
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
fn cached_aci_service_verification_preserves_channel_bindings() {
    let cached = CachedAciServiceVerification {
        expires_at: 10,
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
        "aci-service/v1",
    );

    assert_eq!(event.result, VerificationResult::Verified);
    assert_eq!(event.channel_bindings, cached.channel_bindings);
}

#[test]
fn aci_report_tls_channel_bindings_preserves_service_wide_pins() {
    let keyset = keyset_with_tls(vec![
        TlsSpki {
            domain: None,
            spki_sha256_hex: "AA".repeat(32),
        },
        TlsSpki {
            domain: None,
            spki_sha256_hex: "bb".repeat(32),
        },
    ]);

    let bindings =
        aci_report_tls_channel_bindings(&keyset, &json!({}), "https://gateway.example").unwrap();

    assert_eq!(
        bindings,
        vec![
            ChannelBinding::TlsSpkiSha256 {
                origin: "https://gateway.example".to_string(),
                spki_sha256: "aa".repeat(32),
            },
            ChannelBinding::TlsSpkiSha256 {
                origin: "https://gateway.example".to_string(),
                spki_sha256: "bb".repeat(32),
            },
        ]
    );
}

#[test]
fn aci_report_tls_channel_bindings_selects_domain_binding_for_origin_host() {
    let keyset = keyset_with_tls(vec![
        TlsSpki {
            domain: Some("api.example.com".to_string()),
            spki_sha256_hex: "AA".repeat(32),
        },
        TlsSpki {
            domain: Some("chat.example.com".to_string()),
            spki_sha256_hex: "bb".repeat(32),
        },
    ]);
    let evidence = json!({
        "downstream_tls_binding": {
            "domain": "API.EXAMPLE.COM.",
            "spki_sha256": "AA".repeat(32),
        }
    });

    let bindings =
        aci_report_tls_channel_bindings(&keyset, &evidence, "https://api.example.com").unwrap();

    assert_eq!(
        bindings,
        vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://api.example.com".to_string(),
            spki_sha256: "aa".repeat(32),
        }]
    );
}

#[test]
fn aci_report_tls_channel_bindings_rejects_domain_keyset_without_selected_binding() {
    let keyset = keyset_with_tls(vec![TlsSpki {
        domain: Some("api.example.com".to_string()),
        spki_sha256_hex: "aa".repeat(32),
    }]);

    let err = aci_report_tls_channel_bindings(&keyset, &json!({}), "https://api.example.com")
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("did not select a downstream TLS binding"));
}

#[test]
fn aci_report_tls_channel_bindings_rejects_selected_binding_for_other_host() {
    let keyset = keyset_with_tls(vec![
        TlsSpki {
            domain: Some("api.example.com".to_string()),
            spki_sha256_hex: "aa".repeat(32),
        },
        TlsSpki {
            domain: Some("chat.example.com".to_string()),
            spki_sha256_hex: "bb".repeat(32),
        },
    ]);
    let evidence = json!({
        "downstream_tls_binding": {
            "domain": "chat.example.com",
            "spki_sha256": "bb".repeat(32),
        }
    });

    let err =
        aci_report_tls_channel_bindings(&keyset, &evidence, "https://api.example.com").unwrap_err();

    assert!(err.to_string().contains("does not match upstream host"));
}

#[test]
fn aci_report_tls_channel_bindings_rejects_selected_binding_outside_keyset() {
    let keyset = keyset_with_tls(vec![TlsSpki {
        domain: Some("api.example.com".to_string()),
        spki_sha256_hex: "aa".repeat(32),
    }]);
    let evidence = json!({
        "downstream_tls_binding": {
            "domain": "api.example.com",
            "spki_sha256": "bb".repeat(32),
        }
    });

    let err =
        aci_report_tls_channel_bindings(&keyset, &evidence, "https://api.example.com").unwrap_err();

    assert!(err
        .to_string()
        .contains("not present in the attested keyset"));
}

#[test]
fn verifies_dstack_kms_receipt_key_custody_chain() {
    let root = signing_key(1);
    let app = signing_key(2);
    let receipt_scalar = [3u8; 32];
    let app_id = [0xab; 20];

    let kms_public = public_key_compressed_hex(&SigningKey::from_slice(&receipt_scalar).unwrap());
    let purpose_message = format!("aci.receipt.ed25519.v1:{kms_public}");
    let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id.as_slice(),
        &app.verifying_key().to_sec1_bytes(),
    ]
    .concat();
    let app_signature = sign_recoverable(&root, &root_message);
    let (keyset, evidence) =
        receipt_custody_fixture(receipt_scalar, vec![purpose_signature, app_signature]);
    let policy = AciServiceVerifierPolicy::new(
        vec!["test-subject".to_string()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&root)],
    )
    .unwrap();

    verify_dstack_kms_receipt_custody(&evidence, &keyset, &app_id, &policy).unwrap();
}

#[test]
fn rejects_dstack_kms_receipt_key_custody_under_unaccepted_root() {
    let root = signing_key(1);
    let other_root = signing_key(4);
    let app = signing_key(2);
    let receipt_scalar = [3u8; 32];
    let app_id = [0xab; 20];

    let kms_public = public_key_compressed_hex(&SigningKey::from_slice(&receipt_scalar).unwrap());
    let purpose_message = format!("aci.receipt.ed25519.v1:{kms_public}");
    let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id.as_slice(),
        &app.verifying_key().to_sec1_bytes(),
    ]
    .concat();
    let app_signature = sign_recoverable(&root, &root_message);
    let (keyset, evidence) =
        receipt_custody_fixture(receipt_scalar, vec![purpose_signature, app_signature]);
    let policy = AciServiceVerifierPolicy::new(
        vec!["test-subject".to_string()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&other_root)],
    )
    .unwrap();

    let err = verify_dstack_kms_receipt_custody(&evidence, &keyset, &app_id, &policy)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "dstack KMS root public key is not accepted by verifier policy"
    );
}

#[test]
fn rejects_receipt_custody_whose_key_is_not_in_the_keyset() {
    let root = signing_key(1);
    let app = signing_key(2);
    let receipt_scalar = [3u8; 32];
    let app_id = [0xab; 20];

    let kms_public = public_key_compressed_hex(&SigningKey::from_slice(&receipt_scalar).unwrap());
    let purpose_message = format!("aci.receipt.ed25519.v1:{kms_public}");
    let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id.as_slice(),
        &app.verifying_key().to_sec1_bytes(),
    ]
    .concat();
    let app_signature = sign_recoverable(&root, &root_message);
    let (mut keyset, evidence) =
        receipt_custody_fixture(receipt_scalar, vec![purpose_signature, app_signature]);
    // The attested keyset lists a different receipt key than the custody entry.
    keyset.receipt_signing_keys[0].public_key_hex = "ff".repeat(32);
    let policy = AciServiceVerifierPolicy::new(
        vec!["test-subject".to_string()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&root)],
    )
    .unwrap();

    let err = verify_dstack_kms_receipt_custody(&evidence, &keyset, &app_id, &policy)
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match the attested keyset"));
}
