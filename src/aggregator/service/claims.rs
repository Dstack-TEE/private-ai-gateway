use serde_json::Value;

use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::verifier::{
    AciServiceUpstreamVerifier, ChutesProviderVerifier, NearAiProviderVerifier,
    PhalaDirectProviderVerifier, PrivatemodeProviderVerifier,
};
use crate::aggregator::session::{Claim, ClaimSource, SessionClaims};

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
/// the generic mapper. Typed per-provider logic lives in the
/// `ProviderClaimMapper` implementations.
pub(super) fn claim_mapper(provider_type: Option<&str>) -> &'static dyn ProviderClaimMapper {
    match provider_type {
        Some("aci-service") => &AciServiceClaims,
        Some("tinfoil") => &TinfoilClaims,
        Some("secret-ai") => &SecretAiClaims,
        Some("armet-ai") => &ArmetAiClaims,
        Some("near-ai") | Some("chutes") | Some("phala-direct") | Some("0g-private-computer") => {
            &IntelTdxClaims
        }
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

/// Reviewed adapter capability, never inferred from a provider name alone.
/// Shape validation does not authenticate facts: the configured verifier still
/// owns that responsibility. Unknown/incomplete output remains usable with
/// evidence-sensitive dedup, including legacy Chutes output.
pub(crate) fn reviewed_session_material(event: &UpstreamVerifiedEvent, claims: &Value) -> bool {
    let Some(provider) = event.provider_type.as_deref() else {
        return false;
    };
    let expected_id = match provider {
        "aci-service" => AciServiceUpstreamVerifier::VERIFIER_ID,
        "near-ai" => NearAiProviderVerifier::VERIFIER_ID,
        "phala-direct" => PhalaDirectProviderVerifier::VERIFIER_ID,
        "chutes" => ChutesProviderVerifier::VERIFIER_ID,
        "privatemode" => PrivatemodeProviderVerifier::VERIFIER_ID,
        _ => return false,
    };
    if event.verifier_id != expected_id
        || (provider == "aci-service" && event.aci_identity.is_none())
        || claims["session_material_version"].as_str() != Some(format!("{provider}/1").as_str())
    {
        return false;
    }
    crate::aci::verifier::session_material::valid_profile(provider, claims)
}

pub(super) fn is_multi_instance(provider_type: Option<&str>) -> bool {
    matches!(provider_type, Some("chutes") | Some("0g-private-computer"))
}

/// The instance a binding attests (a Chutes instance id or a 0G provider
/// address). Those providers seal one session per instance; every other
/// provider has a single channel (None), so its claims/evidence apply to the
/// event as a whole.
pub(super) fn instance_id<'a>(
    event: &UpstreamVerifiedEvent,
    binding: &'a ChannelBinding,
) -> Option<&'a str> {
    if !is_multi_instance(event.provider_type.as_deref()) {
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
/// attestation facts, so its session retains CPU material and GPU outcomes while staying
/// stable under fleet churn. Everything fleet-shaped is dropped (the lists, the
/// per-instance maps, and the *fleet-aggregate* GPU scalars — `all`/`any` over
/// instances). What is kept is per-instance and nonce-free: the matched CPU
/// measurements and profile, this instance's TCB/advisories, and this instance's
/// supplemental GPU outcome. The raw nonce-bound quotes stay out.
fn per_instance_provider_claims(full: Option<&Value>, instance_id: &str) -> Value {
    let mut pc = serde_json::Map::new();
    let Some(Value::Object(map)) = full else {
        return Value::Object(pc);
    };
    for key in [
        "session_material_version",
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
    // 0G records each provider's facts under `instances`.
    if let Some(Value::Object(instance)) = map
        .get("instances")
        .and_then(|instances| instances.get(instance_id))
    {
        pc.extend(instance.clone());
    }
    for (map_key, slice_key) in [
        ("instance_tdx_reports", "tdx_report"),
        ("instance_tdx_report_sha256", "tdx_report_sha256"),
        ("instance_advisory_ids", "advisory_ids"),
    ] {
        if let Some(value) = map
            .get(map_key)
            .and_then(|instances| instances.get(instance_id))
        {
            pc.insert(slice_key.to_string(), value.clone());
        }
    }
    // This instance's own GPU outcome (NOT the fleet aggregate), lifted to the
    // top of the slice so `gpu_attested` reflects this instance and the slice
    // stays stable when a sibling instance's GPU does not.
    if let Some(Value::Object(gpu)) = map
        .get("instance_gpu")
        .and_then(|instances| instances.get(instance_id))
    {
        for key in [
            "gpu_verified",
            "gpu_arch",
            "gpu_evidence_present",
            "gpu_evidence_nonce_matched",
        ] {
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

/// Native ACI verifies a hardware quote and channel, and reports collateral TCB
/// status. Measured compose identity is preserved in `extra`; measurement alone
/// does not establish that serving software or the OS is known good.
pub(super) struct AciServiceClaims;
impl ProviderClaimMapper for AciServiceClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        SessionClaims {
            tee_attested: hardware_tee_attested(event),
            tcb_up_to_date: tcb_up_to_date_claim(event),
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

/// armet-ai: a dcap-verified TDX quote whose report_data commits to the
/// channel key, the exact GPU evidence and the model, with the measured TD
/// (MRTD, RTMR0-3 and launch configuration) matching an operator pin, and a
/// mandatory NRAS-signed GPU check. The pin covers firmware, kernel, command
/// line and runtime, but it is an operator's acceptance of a measurement, not a
/// reproducible build, so it asserts serving software only and leaves
/// `os_known_good` Unknown.
pub(super) struct ArmetAiClaims;
impl ProviderClaimMapper for ArmetAiClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        SessionClaims {
            tee_attested: hardware_tee_attested(event),
            tcb_up_to_date: tcb_up_to_date_claim(event),
            serving_software_known_good: armet_ai_software_claim(event),
            gpu_attested: nras_gpu_claim(
                event,
                "the TD commits to the exact GPU evidence in its report_data",
            ),
            ..SessionClaims::default()
        }
    }
}

pub(super) fn armet_ai_software_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let subject = event
        .provider_claims
        .as_ref()
        .and_then(|claims| claims.get("accepted_subject"))
        .and_then(Value::as_str);
    match subject {
        Some(subject) => Claim::asserted(
            ClaimSource::VerifierDerived,
            format!(
                "measured TD (MRTD, RTMR0-3, launch configuration) matches operator-configured \
                 pin {subject}"
            ),
        ),
        None => Claim::unknown(),
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

/// Supplemental GPU check outcome, recorded as verifier-derived metadata. The
/// current NVIDIA paths decode JWTs without signature verification and do not
/// bind GPU identity to the serving CPU. This claim establishes no authenticated
/// device identity or continued use of a particular GPU.
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
                    "Supplemental NVIDIA GPU check accepted and nonce-matched \
                     (arch {arch}); does not authenticate GPU identity or bind it to the serving CPU TEE"
                ),
                None => "Supplemental NVIDIA GPU check accepted and nonce-matched; \
                         does not authenticate GPU identity or bind it to the serving CPU TEE"
                    .to_string(),
            };
            Claim::asserted(ClaimSource::VerifierDerived, reason)
        }
        // `false` is ambiguous (no evidence vs. a swallowed verify error), so we
        // do not refute — only record an accepted, nonce-matched outcome.
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
    nras_gpu_claim(event, "its nonce matches the CPU report_data")
}

/// A mandatory, NRAS-signed GPU gate. `binding` says how the verifier tied the
/// GPU evidence to the CPU quote.
fn nras_gpu_claim(event: &UpstreamVerifiedEvent, binding: &str) -> Claim {
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
                    "NVIDIA confidential-computing GPU attestation verified by NRAS and \
                     {binding} (signed models {models})"
                ),
                None => format!(
                    "NVIDIA confidential-computing GPU attestation verified by NRAS and {binding}"
                ),
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

    fn reviewed_profile(provider: Option<&str>, verifier_id: &str, claims: &Value) -> bool {
        let mut event = event(provider, VerificationResult::Verified, None);
        event.verifier_id = verifier_id.to_string();
        super::reviewed_session_material(&event, claims)
    }

    fn profiles() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/session_material.json"
        ))
        .unwrap()
    }

    #[test]
    fn only_complete_matching_reviewed_profiles_are_eligible() {
        for (provider, claims) in profiles().as_object().unwrap() {
            let id = match provider.as_str() {
                "near-ai" => "private-ai-verifier/near-ai-gateway/v1".to_string(),
                "privatemode" => "privatemode-proxy/co-deployed-contrast/v1".to_string(),
                _ => format!("private-ai-verifier/{provider}/v1"),
            };
            assert!(reviewed_profile(Some(provider), &id, claims), "{provider}");
            let mut extended = claims.clone();
            extended["unknown_top_level_claim"] = json!({"preserved": true});
            assert!(reviewed_profile(Some(provider), &id, &extended));
            assert!(!reviewed_profile(Some(provider), "custom/v1", claims));
            for key in claims.as_object().unwrap().keys() {
                let mut malformed = claims.clone();
                malformed[key] = json!({"invalid": true});
                assert!(
                    !reviewed_profile(Some(provider), &id, &malformed),
                    "{provider}: malformed {key}"
                );
                let mut incomplete = claims.clone();
                incomplete.as_object_mut().unwrap().remove(key);
                assert!(
                    !reviewed_profile(Some(provider), &id, &incomplete),
                    "{provider}: missing {key}"
                );
            }
            let mut unknown = claims.clone();
            unknown["session_material_version"] = json!(format!("{provider}/2"));
            assert!(!reviewed_profile(Some(provider), &id, &unknown));
            for wrong in ["armet-ai", "secret-ai", "custom"] {
                assert!(!reviewed_profile(Some(wrong), &id, claims));
            }
        }
    }

    #[test]
    fn malformed_tdx_projections_and_digests_cannot_enable_reuse() {
        let base = profiles()["near-ai"].clone();
        let id = "private-ai-verifier/near-ai-gateway/v1";
        for key in base["tdx_report"].as_object().unwrap().keys() {
            let mut changed = base.clone();
            changed["tdx_report"][key] = json!("bad");
            // Recompute so field validation, not just digest mismatch, is tested.
            changed["tdx_report_sha256"] = json!(crate::aci::digest::sha256_hex(
                &crate::aci::digest::jcs_bytes(&changed["tdx_report"]).unwrap()
            ));
            assert!(!reviewed_profile(Some("near-ai"), id, &changed), "{key}");
        }
        // Serde structs accept sequence input by default; the profile does not.
        let report: crate::aci::verifier::session_material::TdxReportMaterial =
            serde_json::from_value(base["tdx_report"].clone()).unwrap();
        let report = serde_json::to_value(report).unwrap();
        for (key, array) in [
            (
                "tdx_report",
                json!(report.as_object().unwrap().values().collect::<Vec<_>>()),
            ),
            ("gateway_gpu_check", json!([false, false])),
        ] {
            let mut changed = base.clone();
            changed[key] = array;
            assert!(!reviewed_profile(Some("near-ai"), id, &changed));
        }
        for value in [json!("22".repeat(16)), Value::Null] {
            let mut changed = base.clone();
            changed["tdx_report"]["tee_tcb_svn2"] = value;
            changed["tdx_report_sha256"] = json!(crate::aci::digest::sha256_hex(
                &crate::aci::digest::jcs_bytes(&changed["tdx_report"]).unwrap()
            ));
            assert!(!reviewed_profile(Some("near-ai"), id, &changed));
        }
        let mut changed = base.clone();
        changed["tdx_report_sha256"] = json!(format!("sha256:{}", "00".repeat(32)));
        assert!(!reviewed_profile(Some("near-ai"), id, &changed));
        changed = base.clone();
        changed["tdx_report"]["report_format"] = json!("td15");
        assert!(!reviewed_profile(Some("near-ai"), id, &changed));
        changed["tdx_report"]["tee_tcb_svn2"] = json!("22".repeat(16));
        changed["tdx_report"]["mr_service_td"] = json!("22".repeat(48));
        changed["tdx_report_sha256"] = json!(crate::aci::digest::sha256_hex(
            &crate::aci::digest::jcs_bytes(&changed["tdx_report"]).unwrap()
        ));
        assert!(reviewed_profile(Some("near-ai"), id, &changed));
        for key in ["tee_tcb_svn2", "mr_service_td"] {
            let mut malformed = changed.clone();
            malformed["tdx_report"][key] = json!("zz");
            assert!(!reviewed_profile(Some("near-ai"), id, &malformed));
        }
    }

    #[test]
    fn tdx_profiles_require_string_formats_and_digest_the_supplied_report() {
        let digest = |report: &Value| {
            json!(crate::aci::digest::sha256_hex(
                &crate::aci::digest::jcs_bytes(report).unwrap()
            ))
        };
        for provider in ["near-ai", "phala-direct", "chutes"] {
            let verifier_id = if provider == "near-ai" {
                "private-ai-verifier/near-ai-gateway/v1".to_string()
            } else {
                format!("private-ai-verifier/{provider}/v1")
            };
            for format in ["td10", "td15"] {
                let mut claims = profiles()[provider].clone();
                claims["tdx_report"]["report_format"] = json!(format);
                if format == "td15" {
                    claims["tdx_report"]["tee_tcb_svn2"] = json!("22".repeat(16));
                    claims["tdx_report"]["mr_service_td"] = json!("22".repeat(48));
                }
                claims["tdx_report_sha256"] = digest(&claims["tdx_report"]);
                assert!(reviewed_profile(Some(provider), &verifier_id, &claims));

                // An externally tagged enum object must not be normalized to
                // the string whose digest was supplied by the verifier.
                claims["tdx_report"]["report_format"] = json!({(format): null});
                assert!(!reviewed_profile(Some(provider), &verifier_id, &claims));
                claims["tdx_report_sha256"] = digest(&claims["tdx_report"]);
                assert!(!reviewed_profile(Some(provider), &verifier_id, &claims));

                // A valid string report must hash its own supplied content,
                // not another representation or another valid report.
                claims["tdx_report"]["report_format"] = json!(format);
                assert!(!reviewed_profile(Some(provider), &verifier_id, &claims));
                claims["tdx_report_sha256"] = digest(&claims["tdx_report"]);
                assert!(reviewed_profile(Some(provider), &verifier_id, &claims));
                claims["tdx_report"]["rt_mr0"] = json!("ff".repeat(48));
                assert!(!reviewed_profile(Some(provider), &verifier_id, &claims));
                claims["tdx_report_sha256"] = digest(&claims["tdx_report"]);
                assert!(reviewed_profile(Some(provider), &verifier_id, &claims));
            }
        }
    }

    #[test]
    fn chutes_slice_keeps_own_security_state_and_ignores_sibling_churn() {
        let own = profiles()["chutes"].clone();
        let mut full = json!({
            "session_material_version": "chutes/1", "trust_boundary": "model_instance",
            "evidence_scope": "model_instance", "chute_id": "chute", "canonical_model_id": "model",
            "instance_measurements": {"instance-a": own["measurement"]},
            "instance_tcb_statuses": {"instance-a": own["tcb_status"]},
            "instance_tdx_reports": {"instance-a": own["tdx_report"]},
            "instance_tdx_report_sha256": {"instance-a": own["tdx_report_sha256"]},
            "instance_advisory_ids": {"instance-a": own["advisory_ids"]},
            "instance_gpu": {"instance-a": {
                "gpu_verified": false, "gpu_arch": null, "gpu_evidence_present": false,
                "gpu_evidence_nonce_matched": null,
            }},
        });
        let slice = super::per_instance_provider_claims(Some(&full), "instance-a");
        assert_eq!(slice, own);
        full["instance_tdx_reports"]["sibling"] = json!({"mr_td": "changed"});
        full["instance_gpu"]["sibling"] = json!({"gpu_verified": true});
        full["failed_instance_ids"] = json!(["sibling"]);
        full["nonce_expires_in"] = json!(10);
        assert_eq!(
            slice,
            super::per_instance_provider_claims(Some(&full), "instance-a")
        );
        for (map, key, changed) in [
            ("instance_tcb_statuses", None, json!("OutOfDate")),
            (
                "instance_tdx_reports",
                Some("mr_td"),
                json!("ff".repeat(48)),
            ),
            ("instance_gpu", Some("gpu_verified"), json!(true)),
            (
                "instance_gpu",
                Some("gpu_evidence_nonce_matched"),
                json!(false),
            ),
            ("instance_advisory_ids", None, json!(["SA-C"])),
        ] {
            let mut updated = full.clone();
            match key {
                Some(key) => updated[map]["instance-a"][key] = changed,
                None => updated[map]["instance-a"] = changed,
            }
            assert_ne!(
                slice,
                super::per_instance_provider_claims(Some(&updated), "instance-a"),
                "{map}/{key:?}"
            );
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
    fn armet_ai_asserts_pinned_software_but_never_os_provenance() {
        let subject = format!("tdx-measurement:sha256:{}", "ab".repeat(32));
        let claims = session_claims_for_event(&event(
            Some("armet-ai"),
            VerificationResult::Verified,
            Some(json!({
                "accepted_subject": subject,
                "tcb_status": "OutOfDate",
                "gpu_verified": true,
                "gpu_models": ["GH100"],
                "gpu_count": 1,
                // Even a verifier-supplied OS verdict must not be upgraded:
                // a measurement pin is not a reviewed OS build.
                "production_os_image": true,
            })),
        ));
        assert_eq!(claims.tee_attested.status, ClaimStatus::Asserted);
        assert_eq!(
            claims.tee_attested.source,
            Some(ClaimSource::HardwareProven)
        );
        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Asserted
        );
        assert_eq!(
            claims.serving_software_known_good.source,
            Some(ClaimSource::VerifierDerived)
        );
        assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Refuted);
        assert_eq!(claims.gpu_attested.status, ClaimStatus::Asserted);
        let gpu_reason = claims.gpu_attested.reason.as_deref().unwrap_or_default();
        assert!(
            gpu_reason.contains("exact GPU evidence") && gpu_reason.contains("GH100"),
            "{gpu_reason}"
        );
        assert_eq!(claims.os_known_good.status, ClaimStatus::Unknown);
        assert_eq!(claims.model_weights_provenance.status, ClaimStatus::Unknown);

        let unpinned = session_claims_for_event(&event(
            Some("armet-ai"),
            VerificationResult::Verified,
            Some(json!({"tcb_status": "UpToDate"})),
        ));
        assert_eq!(
            unpinned.serving_software_known_good.status,
            ClaimStatus::Unknown
        );
        assert_eq!(unpinned.gpu_attested.status, ClaimStatus::Unknown);
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
        for provider in ["aci-service", "near-ai", "chutes", "phala-direct"] {
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
    fn aci_service_preserves_measured_identity_without_inventing_good_software() {
        for result in [VerificationResult::Verified, VerificationResult::Failed] {
            let facts = json!({
                "tcb_status": "UpToDate",
                "measured_compose_hash": "aa".repeat(32),
                "measured_app_id": "bb".repeat(20),
                // These fields are not authenticated by the native ACI adapter.
                "production_os_image": true,
                "gpu_verified": true,
            });
            let claims =
                session_claims_for_event(&event(Some("aci-service"), result, Some(facts.clone())));
            assert_eq!(serde_json::to_value(&claims.extra).unwrap(), facts);
            if result == VerificationResult::Verified {
                assert_eq!(claims.tee_attested.status, ClaimStatus::Asserted);
                assert_eq!(
                    claims.tee_attested.source,
                    Some(ClaimSource::HardwareProven)
                );
                assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Asserted);
                assert_eq!(
                    claims.tcb_up_to_date.source,
                    Some(ClaimSource::HardwareProven)
                );
            } else {
                assert_eq!(claims.tee_attested.status, ClaimStatus::Unknown);
                assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Unknown);
            }
            for claim in [
                claims.os_known_good,
                claims.serving_software_known_good,
                claims.gpu_attested,
                claims.model_weights_provenance,
            ] {
                assert_eq!(claim.status, ClaimStatus::Unknown);
            }
        }
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
    fn instance_id_only_for_per_instance_e2ee_bindings() {
        use super::instance_id;
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
        let zero_g = event(
            Some("0g-private-computer"),
            VerificationResult::Verified,
            None,
        );
        assert_eq!(instance_id(&chutes, &e2ee), Some("inst-1"));
        assert_eq!(instance_id(&zero_g, &e2ee), Some("inst-1"));
        assert_eq!(instance_id(&chutes, &tls), None); // not an e2ee instance
        assert_eq!(instance_id(&near, &e2ee), None); // not per-instance
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

    #[test]
    fn zero_g_instance_claims_are_that_providers_entry() {
        use super::per_instance_session_claims;
        let pc = json!({"trust_boundary": "0g", "instances": {
            "0xa": {"signing_address": "0x5a", "tcb_status": "UpToDate"},
            "0xb": {"signing_address": "0x5b", "tcb_status": "OutOfDate"},
        }});
        let ev = event(
            Some("0g-private-computer"),
            VerificationResult::Verified,
            Some(pc),
        );
        let a = per_instance_session_claims(&ev, "0xa");
        assert_eq!(a.tcb_up_to_date.status, ClaimStatus::Asserted);
        assert_eq!(a.extra.get("signing_address"), Some(&json!("0x5a")));
        assert!(!a.extra.contains_key("instances"));
    }
}
