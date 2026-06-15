use serde_json::Value;

use super::AciService;
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aggregator::session::{Claim, ClaimSource, SessionClaims};
use crate::aggregator::upstream_config::UpstreamSessionSink;

impl UpstreamSessionSink for AciService {
    fn record_session(&self, event: &UpstreamVerifiedEvent) {
        if let Err(err) = self.record_attested_upstream_session(event) {
            tracing::warn!(error = %err, "failed to record attested session from verification");
        }
    }
}

/// Maps a verified `UpstreamVerifiedEvent` onto the typed claim vocabulary for
/// one provider. Each provider implements it, so the honesty rules for a
/// provider live with that provider instead of in one central match. `claims`
/// is only invoked for a `Verified` result; the caller folds the raw
/// `provider_claims` into `claims.extra` afterward. A mapper asserts only what
/// its verifier's evidence proves; everything else stays `Unknown`.
pub(super) trait ProviderClaimMapper {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims;
}

/// Route a provider *type* to its claim mapper; an absent/unknown provider gets
/// the generic mapper. This is the only place that branches on the provider
/// string — the per-provider logic lives in the `ProviderClaimMapper` impls.
pub(super) fn claim_mapper(provider: Option<&str>) -> &'static dyn ProviderClaimMapper {
    match provider {
        Some("tinfoil") => &TinfoilClaims,
        Some("near-ai") | Some("chutes") | Some("phala-direct") => &IntelTdxClaims,
        _ => &GenericClaims,
    }
}

/// Build the typed claim set for a verified event. Raw `provider_claims` are
/// always preserved verbatim in `claims.extra` so a deep auditor sees the full
/// provider scope, typed or not.
pub(super) fn session_claims_for_event(event: &UpstreamVerifiedEvent) -> SessionClaims {
    let mut claims = if event.result == VerificationResult::Verified {
        claim_mapper(event.provider.as_deref()).claims(event)
    } else {
        SessionClaims::default()
    };
    if let Some(Value::Object(map)) = event.provider_claims.as_ref() {
        for (key, value) in map {
            claims.extra.insert(key.clone(), value.clone());
        }
    }
    claims
}

/// `tee_attested` rooted in a verified hardware quote with the request channel
/// bound to it. Shared by the providers that verify a real TEE quote.
pub(super) fn hardware_tee_attested(event: &UpstreamVerifiedEvent) -> Claim {
    Claim::asserted(
        ClaimSource::HardwareProven,
        format!(
            "{} verified the TEE quote and bound the request channel",
            event.verifier_id
        ),
    )
}

/// Intel TDX providers (NEAR AI, Chutes, Phala-direct): a real TDX quote, a
/// granular `TcbStatus` from the verified collateral (a HardwareProven
/// tri-state), OS provenance from the attested image hash, and — when the
/// provider supplies it — a verified NVIDIA confidential-computing GPU
/// attestation. (Chutes uses TDX too; it just isn't dstack-based, hence the
/// name is by TEE type, not by stack.)
pub(super) struct IntelTdxClaims;
impl ProviderClaimMapper for IntelTdxClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        // model_weights_provenance stays Unknown: no verifier here checks the
        // served weights.
        SessionClaims {
            tee_attested: hardware_tee_attested(event),
            tcb_up_to_date: tcb_up_to_date_claim(event),
            os_known_good: os_known_good_claim(event),
            gpu_attested: gpu_attested_claim(event),
            ..SessionClaims::default()
        }
    }
}

/// Tinfoil: a verified hardware quote, but its official verifier gates on TCB
/// internally (no separable `TcbStatus`, so freshness is VerifierDerived, never
/// HardwareProven), and it traces serving software to a reviewed Sigstore release.
pub(super) struct TinfoilClaims;
impl ProviderClaimMapper for TinfoilClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        SessionClaims {
            tee_attested: hardware_tee_attested(event),
            tcb_up_to_date: Claim::asserted(
                ClaimSource::VerifierDerived,
                "Tinfoil's verifier requires an up-to-date TCB for a verified \
                 result; no separable TcbStatus is surfaced",
            ),
            serving_software_known_good: tinfoil_software_claim(event),
            os_known_good: os_known_good_claim(event),
            ..SessionClaims::default()
        }
    }
}

/// Generic verifier path: we only know it returned Verified with an enforceable
/// channel binding.
pub(super) struct GenericClaims;
impl ProviderClaimMapper for GenericClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        SessionClaims {
            tee_attested: Claim::asserted(
                ClaimSource::VerifierDerived,
                format!(
                    "{} verified the workload identity and bound the channel",
                    event.verifier_id
                ),
            ),
            ..SessionClaims::default()
        }
    }
}

/// Platform TCB freshness as an honest tri-state from the verifier's reported
/// `tcb_status` (TDX/SEV `TcbStatus`): `UpToDate` asserts, any other reported
/// status refutes — the quote proves a stale TCB even though the gateway does
/// not hard-reject it — and an absent status is Unknown. Freshness is never
/// asserted by policy: a verifier that does not surface a status leaves the
/// claim Unknown, because we cannot prove it is current, not because it is.
pub(super) fn tcb_up_to_date_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let status = event
        .provider_claims
        .as_ref()
        .and_then(|c| c.get("tcb_status"))
        .and_then(Value::as_str);
    match status {
        Some(status) if status.eq_ignore_ascii_case("uptodate") => Claim::asserted(
            ClaimSource::HardwareProven,
            format!("platform TCB status {status}"),
        ),
        Some(status) => Claim::refuted(
            ClaimSource::HardwareProven,
            format!("platform TCB status {status}"),
        ),
        None => Claim::unknown(),
    }
}

/// OS-image provenance from the attested `os_image_hash`. Phala-direct resolves
/// that hash to dstack's published image and reads its prod-vs-dev flag, so
/// `production_os_image` is a verifier-derived verdict: a known production image
/// asserts; a dev image (SSH / serial console enabled — an operator shell that
/// defeats the confidentiality guarantee) **refutes**, recorded rather than
/// hard-rejected; an unresolved hash stays Unknown. Providers that surface no
/// such fact are Unknown.
pub(super) fn os_known_good_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let production = event
        .provider_claims
        .as_ref()
        .and_then(|c| c.get("production_os_image"))
        .and_then(Value::as_bool);
    match production {
        Some(true) => Claim::asserted(
            ClaimSource::VerifierDerived,
            "attested OS image resolves to a known production image",
        ),
        Some(false) => Claim::refuted(
            ClaimSource::VerifierDerived,
            "attested OS image is a dev image (SSH / serial console enabled), not production",
        ),
        None => Claim::unknown(),
    }
}

/// GPU attestation from the provider's NVIDIA confidential-computing evidence.
/// When `gpu_verified` is set, the GPU's own attestation report was
/// cryptographically verified and nonce-bound to this verification round, so we
/// **assert** it — but as `VerifierDerived`, not `HardwareProven`: it attests a
/// genuine CC GPU, not (on its own) that this GPU is bound to the CPU TEE that
/// served the request. Absent GPU evidence (or a provider that doesn't supply
/// it) leaves the claim Unknown — we never assert it by policy.
pub(super) fn gpu_attested_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let claims = event.provider_claims.as_ref();
    let verified = claims
        .and_then(|c| c.get("gpu_verified"))
        .and_then(Value::as_bool);
    match verified {
        Some(true) => {
            let arch = claims
                .and_then(|c| c.get("gpu_arch"))
                .and_then(Value::as_str);
            let reason = match arch {
                Some(arch) => format!(
                    "NVIDIA confidential-computing GPU attestation verified and nonce-bound \
                     (arch {arch}); attests a genuine CC GPU, not its binding to the serving CPU TEE"
                ),
                None => "NVIDIA confidential-computing GPU attestation verified and nonce-bound; \
                         attests a genuine CC GPU, not its binding to the serving CPU TEE"
                    .to_string(),
            };
            Claim::asserted(ClaimSource::VerifierDerived, reason)
        }
        // `false` is ambiguous (no evidence vs. a swallowed verify error), so we
        // do not refute — only assert on a genuine, nonce-bound verification.
        _ => Claim::unknown(),
    }
}

/// Tinfoil traces its serving software to reviewed source: the SEV-SNP launch
/// measurement is compared against the Sigstore golden values published for the
/// build's repo. Cite the source repo and release digest when the verifier
/// reported them.
pub(super) fn tinfoil_software_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let field = |key: &str| {
        event
            .provider_claims
            .as_ref()
            .and_then(|c| c.get(key))
            .and_then(Value::as_str)
    };
    let reason = match (field("config_repo"), field("release_digest")) {
        (Some(repo), Some(digest)) => {
            format!("Sigstore-verified code measurement matches {repo} (release {digest})")
        }
        (Some(repo), None) => format!("Sigstore-verified code measurement matches {repo}"),
        _ => "Sigstore-verified code measurement matches the published golden values".to_string(),
    };
    Claim::asserted(ClaimSource::VerifierDerived, reason)
}

#[cfg(test)]
mod claim_mapping_tests {
    use super::session_claims_for_event;
    use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
    use crate::aggregator::session::{ClaimSource, ClaimStatus};
    use serde_json::{json, Value};

    fn event(
        provider: Option<&str>,
        result: VerificationResult,
        provider_claims: Option<Value>,
    ) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: "operator-config-name".to_string(),
            provider: provider.map(str::to_string),
            model_id: "m".to_string(),
            url_origin: Some("https://up".to_string()),
            verifier_id: "vid/v1".to_string(),
            result,
            required: true,
            channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
                origin: "https://up".to_string(),
                spki_sha256: "aa".repeat(32),
            }],
            provider_claims,
            ..Default::default()
        }
    }

    #[test]
    fn tinfoil_asserts_tee_and_serving_software_with_verifier_derived_tcb() {
        let claims = session_claims_for_event(&event(
            Some("tinfoil"),
            VerificationResult::Verified,
            Some(json!({
                "config_repo": "tinfoilsh/confidential-model",
                "release_digest": "sha256:abc123",
            })),
        ));
        // TEE is hardware-proven.
        assert_eq!(claims.tee_attested.status, ClaimStatus::Asserted);
        assert_eq!(
            claims.tee_attested.source,
            Some(ClaimSource::HardwareProven)
        );
        // TCB is asserted but VerifierDerived — Tinfoil's verifier gates on TCB
        // yet exposes no raw TcbStatus, so it must NOT be labeled HardwareProven
        // (regression guard for the fabricated-"UpToDate" bug).
        assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Asserted);
        assert_eq!(
            claims.tcb_up_to_date.source,
            Some(ClaimSource::VerifierDerived)
        );
        assert_ne!(
            claims.tcb_up_to_date.source,
            Some(ClaimSource::HardwareProven)
        );
        // Serving software is verifier-derived (Sigstore), and cites the source.
        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Asserted
        );
        assert_eq!(
            claims.serving_software_known_good.source,
            Some(ClaimSource::VerifierDerived)
        );
        let reason = claims.serving_software_known_good.reason.unwrap();
        assert!(reason.contains("tinfoilsh/confidential-model"), "{reason}");
        assert!(reason.contains("sha256:abc123"), "{reason}");
        // Honest Unknowns: no OS/GPU/weights provenance proven here.
        assert_eq!(claims.os_known_good.status, ClaimStatus::Unknown);
        assert_eq!(claims.gpu_attested.status, ClaimStatus::Unknown);
        assert_eq!(claims.model_weights_provenance.status, ClaimStatus::Unknown);
        // Raw provider_claims preserved verbatim for deep audit.
        assert_eq!(
            claims.extra.get("config_repo").and_then(Value::as_str),
            Some("tinfoilsh/confidential-model")
        );
    }

    #[test]
    fn near_and_chutes_assert_tee_but_not_serving_software() {
        for provider in ["near-ai", "chutes"] {
            let claims = session_claims_for_event(&event(
                Some(provider),
                VerificationResult::Verified,
                Some(json!({ "tcb_status": "UpToDate" })),
            ));
            assert_eq!(
                claims.tee_attested.status,
                ClaimStatus::Asserted,
                "{provider}"
            );
            // Neither traces serving software to reviewed source.
            assert_eq!(
                claims.serving_software_known_good.status,
                ClaimStatus::Unknown,
                "{provider}"
            );
            assert_eq!(
                claims.gpu_attested.status,
                ClaimStatus::Unknown,
                "{provider}"
            );
        }
    }

    #[test]
    fn os_known_good_refutes_a_dev_image_and_asserts_production() {
        // Phala surfaces production_os_image, resolved from the attested
        // os_image_hash. A dev image (operator console) is refuted, not silently
        // Unknown — a real platform-security signal the client can see.
        let dev = session_claims_for_event(&event(
            Some("phala-direct"),
            VerificationResult::Verified,
            Some(json!({ "production_os_image": false })),
        ));
        assert_eq!(dev.os_known_good.status, ClaimStatus::Refuted);
        assert_eq!(dev.os_known_good.source, Some(ClaimSource::VerifierDerived));

        let prod = session_claims_for_event(&event(
            Some("phala-direct"),
            VerificationResult::Verified,
            Some(json!({ "production_os_image": true })),
        ));
        assert_eq!(prod.os_known_good.status, ClaimStatus::Asserted);

        // Not surfaced / unresolved ⇒ Unknown (e.g. Tinfoil, or an unresolved hash).
        let unknown =
            session_claims_for_event(&event(Some("tinfoil"), VerificationResult::Verified, None));
        assert_eq!(unknown.os_known_good.status, ClaimStatus::Unknown);
    }

    #[test]
    fn gpu_attested_asserts_only_on_a_verified_nonce_bound_gpu() {
        // A verified, nonce-bound GPU attestation asserts — but VerifierDerived,
        // never HardwareProven (it attests a genuine CC GPU, not its binding to
        // the serving CPU TEE).
        let verified = session_claims_for_event(&event(
            Some("phala-direct"),
            VerificationResult::Verified,
            Some(json!({ "gpu_verified": true, "gpu_arch": "hopper" })),
        ));
        assert_eq!(verified.gpu_attested.status, ClaimStatus::Asserted);
        assert_eq!(
            verified.gpu_attested.source,
            Some(ClaimSource::VerifierDerived)
        );
        assert_ne!(
            verified.gpu_attested.source,
            Some(ClaimSource::HardwareProven)
        );

        // No GPU evidence ⇒ Unknown; never asserted by policy.
        let absent = session_claims_for_event(&event(
            Some("phala-direct"),
            VerificationResult::Verified,
            Some(json!({ "tcb_status": "UpToDate" })),
        ));
        assert_eq!(absent.gpu_attested.status, ClaimStatus::Unknown);

        // Present-but-unverified is ambiguous, so we do not refute ⇒ Unknown.
        let unverified = session_claims_for_event(&event(
            Some("chutes"),
            VerificationResult::Verified,
            Some(json!({ "gpu_verified": false })),
        ));
        assert_eq!(unverified.gpu_attested.status, ClaimStatus::Unknown);
    }

    #[test]
    fn tcb_up_to_date_is_a_hardware_proven_tri_state_for_dstack_providers() {
        // The dstack-based providers surface a real TcbStatus from DCAP
        // collateral. (Tinfoil is excluded: its verifier exposes no raw status,
        // so its TCB claim is VerifierDerived, asserted earlier in this module.)
        for provider in ["near-ai", "chutes", "phala-direct"] {
            // UpToDate asserts.
            let up = session_claims_for_event(&event(
                Some(provider),
                VerificationResult::Verified,
                Some(json!({ "tcb_status": "UpToDate" })),
            ));
            assert_eq!(
                up.tcb_up_to_date.status,
                ClaimStatus::Asserted,
                "{provider}"
            );

            // A stale TCB is refuted from the quote — but the session is still
            // created (we do not hard-reject), and TEE attestation still holds.
            let stale = session_claims_for_event(&event(
                Some(provider),
                VerificationResult::Verified,
                Some(json!({ "tcb_status": "OutOfDate" })),
            ));
            assert_eq!(
                stale.tcb_up_to_date.status,
                ClaimStatus::Refuted,
                "{provider}"
            );
            assert_eq!(
                stale.tcb_up_to_date.source,
                Some(ClaimSource::HardwareProven),
                "{provider}"
            );
            assert_eq!(
                stale.tee_attested.status,
                ClaimStatus::Asserted,
                "{provider}"
            );

            // No surfaced status ⇒ Unknown; freshness is never asserted by policy.
            let missing = session_claims_for_event(&event(
                Some(provider),
                VerificationResult::Verified,
                None,
            ));
            assert_eq!(
                missing.tcb_up_to_date.status,
                ClaimStatus::Unknown,
                "{provider}"
            );
            assert_eq!(
                missing.tee_attested.status,
                ClaimStatus::Asserted,
                "{provider}"
            );
        }
    }

    #[test]
    fn generic_provider_asserts_only_tee_verifier_derived() {
        let claims = session_claims_for_event(&event(None, VerificationResult::Verified, None));
        assert_eq!(claims.tee_attested.status, ClaimStatus::Asserted);
        assert_eq!(
            claims.tee_attested.source,
            Some(ClaimSource::VerifierDerived)
        );
        // No TCB/software guarantees from an unidentified verifier.
        assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Unknown);
        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Unknown
        );
    }

    #[test]
    fn failed_result_asserts_nothing_but_preserves_evidence() {
        let claims = session_claims_for_event(&event(
            Some("tinfoil"),
            VerificationResult::Failed,
            Some(json!({ "config_repo": "x" })),
        ));
        assert_eq!(claims.tee_attested.status, ClaimStatus::Unknown);
        assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Unknown);
        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Unknown
        );
        // Raw claims are still recorded for the audit trail.
        assert!(claims.extra.contains_key("config_repo"));
    }
}
