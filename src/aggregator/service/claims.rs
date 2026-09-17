use serde_json::Value;

use super::AciService;
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
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

/// Route a provider *type* to its claim mapper; an absent/unknown type gets
/// the generic mapper. This is the only place that branches on the provider
/// type string — the per-provider logic lives in the `ProviderClaimMapper` impls.
pub(super) fn claim_mapper(provider_type: Option<&str>) -> &'static dyn ProviderClaimMapper {
    match provider_type {
        Some("tinfoil") => &TinfoilClaims,
        Some("secret-ai") => &SecretAiClaims,
        Some("near-ai") | Some("chutes") | Some("phala-direct") => &IntelTdxClaims,
        _ => &GenericClaims,
    }
}

/// Build the typed claim set for a verified event. Raw `provider_claims` are
/// always preserved verbatim in `claims.extra` so a deep auditor sees the full
/// provider scope, typed or not.
pub(super) fn session_claims_for_event(event: &UpstreamVerifiedEvent) -> SessionClaims {
    let mut claims = if event.result == VerificationResult::Verified {
        claim_mapper(event.provider_type.as_deref()).claims(event)
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

/// The Chutes instance id a binding attests, if this is a per-instance Chutes
/// channel. Chutes seals one session per instance; every other provider has a
/// single channel (None), so its claims/evidence apply to the event as a whole.
pub(super) fn chutes_instance_id<'a>(
    event: &UpstreamVerifiedEvent,
    binding: &'a ChannelBinding,
) -> Option<&'a str> {
    if event.provider_type.as_deref() != Some("chutes") {
        return None;
    }
    match binding {
        ChannelBinding::E2eePublicKeySha256 {
            key_id: Some(id), ..
        } => Some(id.as_str()),
        _ => None,
    }
}

/// Typed claims for one Chutes instance: the verifier's facts for that instance
/// only, so its session is content-addressed on its own data and survives fleet
/// churn (a sibling instance joining or failing does not change it).
pub(super) fn per_instance_session_claims(
    event: &UpstreamVerifiedEvent,
    instance_id: &str,
) -> SessionClaims {
    let instance_event = UpstreamVerifiedEvent {
        provider_type: event.provider_type.clone(),
        verifier_id: event.verifier_id.clone(),
        result: event.result,
        provider_claims: Some(per_instance_provider_claims(
            event.provider_claims.as_ref(),
            instance_id,
        )),
        ..Default::default()
    };
    session_claims_for_event(&instance_event)
}

/// One instance's slice of the Chutes `provider_claims` — that instance's own
/// attestation facts, so its session binds its CPU and GPU evidence while staying
/// stable under fleet churn. Everything fleet-shaped is dropped (the lists, the
/// per-instance maps, and the *fleet-aggregate* GPU scalars — `all`/`any` over
/// instances). What is kept is per-instance and nonce-free: the matched CPU
/// measurement profile, this instance's TCB status, and this instance's GPU
/// verification outcome (verified / arch). The raw nonce-bound quotes stay out.
fn per_instance_provider_claims(full: Option<&Value>, instance_id: &str) -> Value {
    let mut pc = serde_json::Map::new();
    let Some(Value::Object(map)) = full else {
        return Value::Object(pc);
    };
    for key in [
        "trust_boundary",
        "evidence_scope",
        "chute_id",
        "canonical_model_id",
    ] {
        if let Some(value) = map.get(key) {
            pc.insert(key.to_string(), value.clone());
        }
    }
    pc.insert(
        "instance_id".to_string(),
        Value::String(instance_id.to_string()),
    );
    if let Some(status) = map
        .get("instance_tcb_statuses")
        .and_then(|statuses| statuses.get(instance_id))
    {
        pc.insert("tcb_status".to_string(), status.clone());
    }
    if let Some(measurement) = map
        .get("instance_measurements")
        .and_then(|measurements| measurements.get(instance_id))
    {
        pc.insert("measurement".to_string(), measurement.clone());
    }
    // This instance's own GPU outcome (NOT the fleet aggregate), lifted to the
    // top of the slice so `gpu_attested` reflects this instance and the slice
    // stays stable when a sibling instance's GPU does not.
    if let Some(Value::Object(gpu)) = map
        .get("instance_gpu")
        .and_then(|instances| instances.get(instance_id))
    {
        for key in ["gpu_verified", "gpu_arch", "gpu_evidence_present"] {
            if let Some(value) = gpu.get(key) {
                pc.insert(key.to_string(), value.clone());
            }
        }
    }
    Value::Object(pc)
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

/// SecretAI: a verified TDX or SEV-SNP quote, an NRAS-verified GPU bound by
/// report_data, and an exact measured production SecretVM workload. An optional
/// operator pin upgrades measured software identity to known-good software.
pub(super) struct SecretAiClaims;
impl ProviderClaimMapper for SecretAiClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        SessionClaims {
            tee_attested: hardware_tee_attested(event),
            tcb_up_to_date: tcb_up_to_date_claim(event),
            serving_software_known_good: secret_ai_software_claim(event),
            os_known_good: secret_ai_os_claim(event),
            gpu_attested: secret_ai_gpu_claim(event),
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

pub(super) fn secret_ai_software_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let workload_id = event
        .provider_claims
        .as_ref()
        .and_then(|claims| claims.get("accepted_workload_id"))
        .and_then(Value::as_str);
    match workload_id {
        Some(workload_id) => Claim::asserted(
            ClaimSource::VerifierDerived,
            format!("measured SecretVM workload matches operator-configured pin {workload_id}"),
        ),
        None => Claim::unknown(),
    }
}

pub(super) fn secret_ai_os_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let production = event
        .provider_claims
        .as_ref()
        .and_then(|claims| claims.get("production_os_image"))
        .and_then(Value::as_bool);
    match production {
        Some(true) => Claim::asserted(
            ClaimSource::VerifierDerived,
            "measured SecretVM launch matches a production entry in the pinned verifier registry",
        ),
        Some(false) => Claim::refuted(
            ClaimSource::VerifierDerived,
            "measured SecretVM launch does not match a production registry entry",
        ),
        None => Claim::unknown(),
    }
}

pub(super) fn secret_ai_gpu_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let claims = event.provider_claims.as_ref();
    let verified = claims
        .and_then(|claims| claims.get("gpu_verified"))
        .and_then(Value::as_bool);
    match verified {
        Some(true) => {
            let models = claims
                .and_then(|claims| claims.get("gpu_models"))
                .and_then(Value::as_array)
                .map(|models| {
                    models
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|models| !models.is_empty());
            let reason = match models {
                Some(models) => format!(
                    "NVIDIA confidential-computing GPU attestation verified by NRAS and its nonce \
                     matches the CPU report_data (signed models {models})"
                ),
                None => "NVIDIA confidential-computing GPU attestation verified by NRAS and its \
                         nonce matches the CPU report_data"
                    .to_string(),
            };
            Claim::asserted(ClaimSource::VerifierDerived, reason)
        }
        _ => Claim::unknown(),
    }
}

#[cfg(test)]
mod claim_mapping_tests {
    use super::session_claims_for_event;
    use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
    use crate::aggregator::session::{ClaimSource, ClaimStatus};
    use serde_json::{json, Value};

    fn event(
        provider_type: Option<&str>,
        result: VerificationResult,
        provider_claims: Option<Value>,
    ) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: "operator-config-name".to_string(),
            provider_type: provider_type.map(str::to_string),
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
    fn secret_ai_maps_pinned_workload_and_cpu_bound_gpu_claims() {
        let workload_id = concat!(
            "secretvm:sev-snp:gpu_prod:4xlarge:v0.0.33:sha256:",
            "ea08d2b8a03bea1d3206286e50da41e437b3fd4a9e6e3415a0d6169f05bb7cf2"
        );
        let claims = session_claims_for_event(&event(
            Some("secret-ai"),
            VerificationResult::Verified,
            Some(json!({
                "workload_id": workload_id,
                "accepted_workload_id": workload_id,
                "production_os_image": true,
                "gpu_verified": true,
                "gpu_models": ["GH100"],
                "gpu_count": 1,
                "tcb_status": "UpToDate",
            })),
        ));

        assert_eq!(claims.tee_attested.status, ClaimStatus::Asserted);
        assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Asserted);
        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Asserted
        );
        assert_eq!(claims.os_known_good.status, ClaimStatus::Asserted);
        assert_eq!(claims.gpu_attested.status, ClaimStatus::Asserted);
        assert_eq!(claims.model_weights_provenance.status, ClaimStatus::Unknown);
    }

    #[test]
    fn secret_ai_unpinned_workload_leaves_software_provenance_unknown() {
        let claims = session_claims_for_event(&event(
            Some("secret-ai"),
            VerificationResult::Verified,
            Some(json!({
                "workload_id": "secretvm:tdx:prod:large:v1:sha256:abc",
            })),
        ));

        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Unknown
        );
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

    #[test]
    fn chutes_instance_id_only_for_chutes_e2ee_bindings() {
        use super::chutes_instance_id;
        let e2ee = ChannelBinding::E2eePublicKeySha256 {
            provider: "chutes".to_string(),
            key_id: Some("inst-1".to_string()),
            algorithm: "chutes-ml-kem-768".to_string(),
            public_key_sha256: "aa".repeat(32),
        };
        let tls = ChannelBinding::TlsSpkiSha256 {
            origin: "https://up".to_string(),
            spki_sha256: "aa".repeat(32),
        };
        let chutes = event(Some("chutes"), VerificationResult::Verified, None);
        let near = event(Some("near-ai"), VerificationResult::Verified, None);
        assert_eq!(chutes_instance_id(&chutes, &e2ee), Some("inst-1"));
        assert_eq!(chutes_instance_id(&chutes, &tls), None); // not an e2ee instance
        assert_eq!(chutes_instance_id(&near, &e2ee), None); // not Chutes
    }

    #[test]
    fn per_instance_claims_pick_the_instance_and_drop_fleet_data() {
        use super::per_instance_session_claims;
        // Fleet TCB + GPU are aggregates (inst-2 is bad on both); per-instance
        // maps carry each instance's own facts. A slice must bind the instance's
        // own facts and ignore the fleet, so it survives sibling churn.
        let pc = json!({
            "trust_boundary": "model_instance",
            "chute_id": "chute-x",
            "tcb_status": "OutOfDate",
            "gpu_verified": false,
            "verified_instance_ids": ["inst-1", "inst-2"],
            "instance_tcb_statuses": { "inst-1": "UpToDate", "inst-2": "OutOfDate" },
            "instance_measurements": { "inst-1": "profile-x", "inst-2": "profile-y" },
            "instance_gpu": {
                "inst-1": { "gpu_verified": true, "gpu_arch": "hopper", "gpu_evidence_present": true },
                "inst-2": { "gpu_verified": false }
            },
        });
        let ev = event(Some("chutes"), VerificationResult::Verified, Some(pc));

        // inst-1's session binds inst-1's own TCB, measurement, and GPU — even
        // though the fleet TCB is OutOfDate and the fleet gpu_verified is false.
        let c1 = per_instance_session_claims(&ev, "inst-1");
        assert_eq!(c1.tcb_up_to_date.status, ClaimStatus::Asserted);
        assert_eq!(c1.tcb_up_to_date.source, Some(ClaimSource::HardwareProven));
        assert_eq!(c1.gpu_attested.status, ClaimStatus::Asserted);
        assert_eq!(
            c1.extra.get("instance_id").and_then(Value::as_str),
            Some("inst-1")
        );
        assert_eq!(
            c1.extra.get("tcb_status").and_then(Value::as_str),
            Some("UpToDate")
        );
        assert_eq!(
            c1.extra.get("measurement").and_then(Value::as_str),
            Some("profile-x")
        );
        assert_eq!(
            c1.extra.get("gpu_verified").and_then(Value::as_bool),
            Some(true)
        );
        // Fleet-wide fields are dropped, so the slice (and the session content id)
        // does not change when a sibling instance does (per-instance idempotency).
        assert!(!c1.extra.contains_key("verified_instance_ids"));
        assert!(!c1.extra.contains_key("instance_tcb_statuses"));
        assert!(!c1.extra.contains_key("instance_measurements"));
        assert!(!c1.extra.contains_key("instance_gpu"));

        // inst-2 differs (its own TCB refuted, its own GPU not asserted), so its
        // session id will too.
        let c2 = per_instance_session_claims(&ev, "inst-2");
        assert_eq!(c2.tcb_up_to_date.status, ClaimStatus::Refuted);
        assert_eq!(c2.gpu_attested.status, ClaimStatus::Unknown);
    }
}
