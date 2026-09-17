//! The §9.1 report appraisal: which checks run, in what order, and what each
//! outcome means.
//!
//! Both verifiers go through [`appraise_report`] — the gateway folds the
//! outcomes into one accept/reject, the CLI renders each as a transcript line.
//! Deciding separately is how the two drift while each keeps passing its own
//! tests.

use serde_json::Value;

use super::dstack::{
    dstack_app_id, verify_dstack_compose_measurement, verify_dstack_kms_receipt_custody,
};
use super::quote::{
    parse_quote_evidence, quote_binds_report_data, verify_quote_to_root, QuoteStepError,
};
use super::report::{verify_report_binding, AciReportValidationError, ReportBinding};
use super::{verify_dstack_event_log, AciServiceVerifierPolicy};
use crate::aci::receipt::ChannelBinding;
use crate::aci::types::{AttestationReport, SourceProvenance, WorkloadKeyset};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckId {
    Quote,
    Binding,
    Expiry,
    Provenance,
    Custody,
    Channel,
}

/// Why a check failed, typed so a caller can map it onto its own errors.
#[derive(Debug, thiserror::Error)]
pub enum FailureCause {
    #[error(transparent)]
    Binding(#[from] AciReportValidationError),
    #[error(transparent)]
    Quote(#[from] QuoteStepError),
    #[error("keyset expired: now {now} >= not_after {not_after}")]
    Expired { now: u64, not_after: u64 },
    #[error("{0}")]
    Provenance(String),
    #[error("{0}")]
    Evidence(String),
    #[error("key custody did not verify: {0}")]
    Custody(String),
    #[error("{0}")]
    Policy(String),
    #[error("{0}")]
    Channel(String),
    #[error("{0}")]
    NotReached(&'static str),
}

/// `Unevaluable` carries the short reason. Its evidence was absent, so a
/// caller about to release data treats it as a failure.
#[derive(Debug)]
pub enum Outcome {
    Pass,
    Failed(FailureCause),
    Unevaluable(String),
}

#[derive(Debug)]
pub struct CheckResult {
    pub id: CheckId,
    pub outcome: Outcome,
    pub detail: String,
    pub explain: Option<String>,
}

impl CheckResult {
    pub fn passed(&self) -> bool {
        matches!(self.outcome, Outcome::Pass)
    }

    fn with_explain(mut self, explain: Option<String>) -> Self {
        self.explain = explain;
        self
    }
}

/// `Offline` parses the quote and checks its binding, but never verifies it
/// to the vendor root.
pub enum QuoteSource<'a> {
    Online { pccs_url: &'a str },
    Offline { reason: &'a str },
}

pub enum CustodyEvidence<'a> {
    DstackKms {
        policy: &'a AciServiceVerifierPolicy,
    },
    Unimplemented {
        reason: &'a str,
    },
}

/// `Unobservable` is an offline audit; `NotObserved` is a live run that could
/// not see its channel, which §1.1 makes a failure rather than a skip.
pub enum ChannelEvidence<'a> {
    Observed { host: &'a str, spki_sha256: &'a str },
    DeclaredFor { origin: &'a str },
    Unobservable { reason: &'a str },
    NotObserved { reason: &'a str },
}

pub struct AppraisalInputs<'a> {
    pub report: &'a AttestationReport,
    /// The nonce this verifier supplied on the fetch (§3.2).
    pub nonce: Option<&'a str>,
    pub now_secs: u64,
    /// Archival audit (§3.4): expiry is reported, not enforced.
    pub expiry_waived: bool,
    pub quote: QuoteSource<'a>,
    /// Compose hashes this verifier accepts; empty verifies without pinning.
    pub accepted_composes: &'a [String],
    pub custody: CustodyEvidence<'a>,
    pub channel: ChannelEvidence<'a>,
    pub explain: bool,
}

pub struct Appraisal {
    pub results: Vec<CheckResult>,
    pub identity: Option<ReportBinding>,
    pub channel_bindings: Vec<ChannelBinding>,
}

impl Appraisal {
    pub fn first_problem(&self) -> Option<&CheckResult> {
        self.results.iter().find(|r| !r.passed())
    }
}

/// Checks run in dependency order and are returned in §9.1 step order. A step
/// whose inputs never arrived is `NotReached` rather than silently skipped, and
/// no network is spent after a failure that already decides the outcome.
///
/// `Err` means the payload is not an `aci/1` report at all — a protocol gate,
/// not a check.
pub async fn appraise_report(inputs: AppraisalInputs<'_>) -> Result<Appraisal, String> {
    let report = inputs.report;
    if report.api_version != "aci/1" {
        return Err(format!(
            "unsupported ACI api_version {:?} (expected \"aci/1\")",
            report.api_version
        ));
    }
    let mut results = Vec::with_capacity(6);

    // §9.1(1) checks the quote against the report_data the report states;
    // §9.1(2) proves that value is the one the keyset and nonce produce. They
    // are independent, so each reports its own problem.
    let claimed_report_data = super::decode_hex_32(&report.attestation.report_data_hex);
    let (quote_result, verified_quote) = match claimed_report_data {
        Ok(claimed) => appraise_quote(&inputs, claimed).await,
        Err(e) => (
            failed_with(
                CheckId::Quote,
                FailureCause::Quote(QuoteStepError::InvalidQuoteHex(e.clone())),
                format!(
                    "report_data {:?} is not 32 bytes of hex: {e}",
                    report.attestation.report_data_hex
                ),
            ),
            None,
        ),
    };
    results.push(quote_result);

    let (binding_result, identity) = appraise_binding(&inputs);
    results.push(binding_result);

    let keyset = identity.as_ref().map(|b| &b.keyset);
    results.push(appraise_expiry(&inputs, keyset));

    let (provenance_result, app_id) = appraise_provenance(&inputs, verified_quote.as_ref()).await;
    results.push(provenance_result);

    results.push(appraise_custody(&inputs, keyset, app_id.as_deref()));

    let (channel_result, channel_bindings) = appraise_channel(&inputs, keyset);
    results.push(channel_result);

    results.sort_by_key(|r| r.id);
    Ok(Appraisal {
        results,
        identity,
        channel_bindings,
    })
}

fn pass(id: CheckId, detail: impl Into<String>) -> CheckResult {
    CheckResult {
        id,
        outcome: Outcome::Pass,
        detail: detail.into(),
        explain: None,
    }
}

fn failed(id: CheckId, cause: FailureCause) -> CheckResult {
    let detail = cause.to_string();
    failed_with(id, cause, detail)
}

fn failed_with(id: CheckId, cause: FailureCause, detail: impl Into<String>) -> CheckResult {
    CheckResult {
        id,
        outcome: Outcome::Failed(cause),
        detail: detail.into(),
        explain: None,
    }
}

fn unevaluable(id: CheckId, detail: impl Into<String>, why: impl Into<String>) -> CheckResult {
    CheckResult {
        id,
        outcome: Outcome::Unevaluable(why.into()),
        detail: detail.into(),
        explain: None,
    }
}

fn unreached(id: CheckId, why: &'static str) -> CheckResult {
    failed_with(id, FailureCause::NotReached(why), why)
}

/// §9.1(2). The `nonce:null` form proves binding but not freshness, so it is
/// reported as unevaluable rather than passed.
fn appraise_binding(inputs: &AppraisalInputs<'_>) -> (CheckResult, Option<ReportBinding>) {
    let binding = match verify_report_binding(inputs.report, inputs.nonce) {
        Ok(binding) => binding,
        Err(e) => {
            let detail = match inputs.nonce {
                Some(nonce) => format!("{e} (for nonce {nonce:?})"),
                None => format!("{e} (for null nonce)"),
            };
            return (failed_with(CheckId::Binding, e.into(), detail), None);
        }
    };
    let explain = inputs.explain.then(|| {
        format!(
            "keyset JCS ({} bytes): {}\ncomputed digest: {}\nstatement: {}\ncomputed report_data: {}\nexpected report_data: {}",
            binding.keyset_jcs.len(),
            String::from_utf8_lossy(&binding.keyset_jcs),
            binding.keyset_digest,
            String::from_utf8_lossy(&binding.statement),
            hex::encode(binding.report_data),
            inputs.report.attestation.report_data_hex,
        )
    });
    let digest = binding.keyset_digest.clone();
    let result = match inputs.nonce {
        Some(nonce) => pass(
            CheckId::Binding,
            format!(
                "keyset digest {digest}; statement digest for nonce {nonce:?} matches report_data"
            ),
        ),
        None => unevaluable(
            CheckId::Binding,
            format!("keyset digest {digest}; the null statement binds, freshness needs a nonce"),
            "binding shown, freshness not established",
        ),
    };
    (result.with_explain(explain), Some(binding))
}

async fn appraise_quote(
    inputs: &AppraisalInputs<'_>,
    claimed_report_data: [u8; 32],
) -> (CheckResult, Option<dcap_qvl::quote::Report>) {
    let evidence = &inputs.report.attestation.evidence;
    let (raw, quote) = match parse_quote_evidence(evidence) {
        Ok(parsed) => parsed,
        Err(e) => {
            let detail = format!("{e} (hardware evidence is required)");
            return (failed_with(CheckId::Quote, e.into(), detail), None);
        }
    };
    let explain = inputs.explain.then(|| {
        format!(
            "report_data (32 bytes) = {}\nquote report_data slot (64 bytes) = {}",
            inputs.report.attestation.report_data_hex,
            hex::encode(super::dcap_report_data(&quote.report))
        )
    });
    if let Err(e) = quote_binds_report_data(evidence, &quote.report, claimed_report_data) {
        return (
            failed(CheckId::Quote, e.into()).with_explain(explain),
            Some(quote.report),
        );
    }
    let result = match &inputs.quote {
        // Fail closed: verifying live and unable to reach the vendor root
        // means the quote was never checked — the one thing a forged service
        // cannot pass.
        QuoteSource::Online { pccs_url } => match verify_quote_to_root(
            &raw,
            pccs_url,
            inputs.now_secs,
            &inputs.report.attestation.tee_type,
        )
        .await
        {
            Ok(verified) => pass(
                CheckId::Quote,
                format!(
                    "{} quote verified (TCB status {}) and binds report_data; collateral from {pccs_url}",
                    verified.tee_type, verified.status
                ),
            ),
            Err(e) => failed(CheckId::Quote, e.into()),
        },
        QuoteSource::Offline { reason } => unevaluable(
            CheckId::Quote,
            format!("the quote binds report_data, but {reason}: not checked to the vendor root"),
            *reason,
        ),
    };
    (result.with_explain(explain), Some(quote.report))
}

fn appraise_expiry(inputs: &AppraisalInputs<'_>, keyset: Option<&WorkloadKeyset>) -> CheckResult {
    let Some(keyset) = keyset else {
        return unreached(
            CheckId::Expiry,
            "no decoded keyset to read not_after from (see the binding check)",
        );
    };
    let (now, not_after) = (inputs.now_secs, keyset.not_after);
    if inputs.expiry_waived {
        return unevaluable(
            CheckId::Expiry,
            format!("expiry not enforced for this audit: not_after {not_after}"),
            "expiry not enforced",
        );
    }
    if keyset.is_expired_at(now) {
        failed_with(
            CheckId::Expiry,
            FailureCause::Expired { now, not_after },
            format!("keyset EXPIRED: now {now} >= not_after {not_after}"),
        )
    } else {
        pass(
            CheckId::Expiry,
            format!("now {now} < not_after {not_after}"),
        )
    }
}

/// §9.1(4). Returns the RTMR3-measured app-id, which §9.1(5) anchors on.
pub(super) async fn appraise_provenance(
    inputs: &AppraisalInputs<'_>,
    quote_report: Option<&dcap_qvl::quote::Report>,
) -> (CheckResult, Option<Vec<u8>>) {
    let evidence = &inputs.report.attestation.evidence;
    let provenance: &SourceProvenance = &inputs.report.attestation.source_provenance;
    let declared = match (
        provenance.repo_url.as_deref(),
        provenance.repo_commit.as_deref(),
        provenance.image_digest.as_deref(),
    ) {
        (Some(url), Some(commit), _) => format!("repo={url} commit={commit}"),
        (_, _, Some(digest)) => format!("image_digest={digest}"),
        // §4.1: a verifier MUST reject a report without acceptable
        // provenance, measured compose or not.
        _ => {
            return (
                failed_with(
                    CheckId::Provenance,
                    FailureCause::Provenance(
                        "the report declares no source provenance (spec 4.1)".to_string(),
                    ),
                    "the report declares no source provenance (spec 4.1)",
                ),
                None,
            )
        }
    };
    let (Some(_app_compose), Some(quote_report)) = (
        evidence.get("app_compose").and_then(Value::as_str),
        quote_report,
    ) else {
        // §9.1(4): a provenance claim no measurement backs MUST NOT satisfy
        // this check. Only an appraisal that never verified a quote may
        // record this honestly as unevaluable.
        let result = match inputs.quote {
            QuoteSource::Online { .. } => failed_with(
                CheckId::Provenance,
                FailureCause::Provenance(format!(
                    "no measurement backs the declared provenance ({declared}): the service \
                     publishes no app_compose (spec 9.1(4), spec 4.1)"
                )),
                format!("no measurement backs the provenance ({declared}): no app_compose (spec 9.1(4))"),
            ),
            QuoteSource::Offline { .. } => unevaluable(
                CheckId::Provenance,
                format!("no app_compose; provenance is presence-only: {declared}"),
                "no app_compose",
            ),
        };
        return (result, None);
    };
    let events = match verify_dstack_event_log(evidence, quote_report) {
        Ok(events) => events,
        Err(e) => return (failed(CheckId::Provenance, FailureCause::Evidence(e)), None),
    };
    let measured = match verify_dstack_compose_measurement(evidence, &events) {
        Ok(measured) => measured,
        Err(e) => return (failed(CheckId::Provenance, FailureCause::Evidence(e)), None),
    };
    let app_id = dstack_app_id(&events).ok();
    let explain = inputs
        .explain
        .then(|| format!("sha256(app_compose) = measured compose-hash = {measured}; RTMR3 replay matched the quote"));
    // The measured compose is the corroborated value (§4.1), so it is what an
    // allowlist pins; the provenance fields ride along unpinned.
    let result = if !inputs.accepted_composes.is_empty()
        && !inputs
            .accepted_composes
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&measured))
    {
        let detail = format!("measured compose-hash={measured} is not in the accepted list");
        failed_with(
            CheckId::Provenance,
            FailureCause::Policy(detail.clone()),
            detail,
        )
    } else {
        pass(
            CheckId::Provenance,
            format!(
                "compose-hash={measured} measured into RTMR3; {declared} (published, not rebuilt)"
            ),
        )
    };
    (result.with_explain(explain), app_id)
}

fn appraise_custody(
    inputs: &AppraisalInputs<'_>,
    keyset: Option<&WorkloadKeyset>,
    app_id: Option<&[u8]>,
) -> CheckResult {
    let subject = keyset.and_then(|k| k.subject.as_deref()).unwrap_or("null");
    let policy = match &inputs.custody {
        CustodyEvidence::Unimplemented { reason } => {
            return unevaluable(
                CheckId::Custody,
                format!("{reason}; subject: {subject} (no policy constraints applied)"),
                "custody policy not implemented",
            )
        }
        CustodyEvidence::DstackKms { policy } => policy,
    };
    let Some(keyset) = keyset else {
        return unreached(
            CheckId::Custody,
            "no established keyset to check custody for",
        );
    };
    let Some(app_id) = app_id else {
        return unreached(
            CheckId::Custody,
            "the measured app-id the custody chain anchors on was never established",
        );
    };
    if let Err(e) = verify_dstack_kms_receipt_custody(
        &inputs.report.attestation.evidence,
        keyset,
        app_id,
        policy,
    ) {
        let detail = format!("key custody did not verify: {e}");
        return failed_with(
            CheckId::Custody,
            FailureCause::Evidence(e.to_string()),
            detail,
        );
    }
    // The policy anchor compares against the measured app-id, not the
    // report's own claims.
    if !policy.accepts_measured(keyset, &inputs.report.attestation.source_provenance, app_id) {
        return failed_with(
            CheckId::Custody,
            FailureCause::Policy(format!(
                "subject {subject} is not acceptable to the verifier policy"
            )),
            format!("subject {subject} is not acceptable to the verifier policy"),
        );
    }
    pass(
        CheckId::Custody,
        format!("keys held under the dstack KMS chain; subject {subject} accepted"),
    )
}

fn appraise_channel(
    inputs: &AppraisalInputs<'_>,
    keyset: Option<&WorkloadKeyset>,
) -> (CheckResult, Vec<ChannelBinding>) {
    let none = Vec::new();
    let (host, spki) = match &inputs.channel {
        ChannelEvidence::Unobservable { reason } => {
            let r = unevaluable(CheckId::Channel, *reason, "no live channel to bind");
            return (r, none);
        }
        ChannelEvidence::NotObserved { reason } => {
            let r = failed_with(
                CheckId::Channel,
                FailureCause::Channel((*reason).to_string()),
                format!("the channel is not bound to the attested keyset ({reason}); spec 1.1"),
            );
            return (r, none);
        }
        ChannelEvidence::Observed { host, spki_sha256 } => (*host, Some(*spki_sha256)),
        ChannelEvidence::DeclaredFor { origin } => (*origin, None),
    };
    let Some(keyset) = keyset else {
        let r = unreached(
            CheckId::Channel,
            "no decoded keyset to match the channel against (see the binding check)",
        );
        return (r, none);
    };
    let Some(spki) = spki else {
        // The caller will enforce what the report declares clients pin, so the
        // deployment's own narrowing decides which entry that is.
        return match super::aci_service::declared_tls_channel_bindings(
            keyset,
            &inputs.report.attestation.evidence,
            host,
        ) {
            Ok(bindings) => (
                pass(
                    CheckId::Channel,
                    format!("the entry {host} clients pin is attested in the keyset"),
                ),
                bindings,
            ),
            Err(e) => (
                failed(CheckId::Channel, FailureCause::Channel(e.to_string())),
                none,
            ),
        };
    };
    let host = host.to_ascii_lowercase();
    let observed = spki.to_ascii_lowercase();
    let candidates: Vec<&str> = keyset
        .tls_keys_for_host(&host)
        .iter()
        .map(|k| k.spki_sha256_hex.as_str())
        .collect();
    let result = if candidates.iter().any(|c| c.eq_ignore_ascii_case(&observed)) {
        pass(
            CheckId::Channel,
            format!("observed SPKI {observed} for {host} is in the attested keyset"),
        )
    } else if candidates.is_empty() {
        // §9.1(6) also accepts an attested E2EE key, but a caller sending
        // plaintext over TLS has nothing else to pin.
        let why = if keyset.tls_public_keys.is_empty() {
            "the keyset publishes no TLS role".to_string()
        } else {
            format!("no attested TLS key is scoped to {host}")
        };
        failed_with(
            CheckId::Channel,
            FailureCause::Channel(why.clone()),
            format!("{why}: the channel cannot be bound (spec 1.1, spec 9.1(6))"),
        )
    } else {
        failed_with(
            CheckId::Channel,
            FailureCause::Channel(format!("observed SPKI {observed} is not attested")),
            format!("observed SPKI {observed} for {host} is NOT in the attested keyset"),
        )
    };
    let explain = inputs.explain.then(|| {
        format!(
            "observed leaf SPKI sha256 for {host}: {observed}\nattested candidates: {}",
            if candidates.is_empty() {
                "(none)".to_string()
            } else {
                candidates.join(", ")
            }
        )
    });
    (result.with_explain(explain), none)
}
