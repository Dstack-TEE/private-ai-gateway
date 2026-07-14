//! Verification transcript: the fixed check vocabulary shared by every `aci`
//! subcommand, plus verdict logic and human/JSON rendering.
//!
//! Honesty rules: `skip` is never treated as `pass`, but only a `fail` blocks
//! the VERIFIED verdict; the verdict line states how many checks were skipped
//! and why. `info` lines record observations (the §10.2 rewrite note) and
//! never count either way. Every line carries its spec section citation
//! (`spec/aci.md` §10).

use serde_json::{json, Value};

/// One entry in the fixed check vocabulary (id, spec citation, title).
#[derive(Debug, Clone, Copy)]
pub struct CheckDef {
    pub id: &'static str,
    pub section: &'static str,
    pub title: &'static str,
}

const fn def(id: &'static str, section: &'static str, title: &'static str) -> CheckDef {
    CheckDef { id, section, title }
}

pub const L2_1: CheckDef = def(
    "L2.1",
    "10.1(1)",
    "hardware quote verifies to TEE vendor root and binds report_data",
);
pub const L2_2: CheckDef = def(
    "L2.2",
    "10.1(2)",
    "binding chain: keyset bytes -> digest -> statement for our nonce -> report_data",
);
pub const L2_3: CheckDef = def("L2.3", "10.1(3)", "keyset not expired (now < not_after)");
pub const L2_4: CheckDef = def(
    "L2.4",
    "10.1(4)",
    "source provenance connects workload to public code",
);
pub const L2_5: CheckDef = def(
    "L2.5",
    "10.1(5)",
    "private-key custody and subject per profile",
);
pub const L2_6: CheckDef = def(
    "L2.6",
    "10.1(6)",
    "the channel actually used is bound to the attested keyset (TLS SPKI or E2EE key)",
);
pub const R_1: CheckDef = def(
    "R.1",
    "10.2(1)",
    "envelope signature over payload bytes under attested receipt key",
);
pub const R_2: CheckDef = def(
    "R.2",
    "10.2(2)",
    "payload workload_keyset_digest matches established digest",
);
pub const R_3: CheckDef = def(
    "R.3",
    "10.2(3)",
    "request.received body_hash matches sent bytes",
);
pub const R_4: CheckDef = def(
    "R.4",
    "10.2(4)",
    "response.returned body_hash matches received wire bytes",
);
/// The §10.2 rewrite note — informational, not one of the four checks:
/// differing `request.forwarded`/`request.received` hashes are the rewrite,
/// and whether a rewrite is acceptable is local policy.
pub const R_NOTE: CheckDef = def(
    "R.note",
    "10.2",
    "service-side rewrite: request.forwarded differs from request.received",
);
pub const U_1: CheckDef = def(
    "U.1",
    "10.3(1)",
    "upstream.verified result is verified and cites a session",
);
pub const U_2: CheckDef = def(
    "U.2",
    "10.3(2-5)",
    "session deep audit: served bytes hash to cited id, served_at in window, evidence digest",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Skip,
    /// A recorded observation, not a check: never a pass, never blocks.
    Info,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Skip => "skip",
            Status::Info => "info",
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
            Status::Info => "INFO",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub def: CheckDef,
    pub status: Status,
    pub detail: String,
    /// Short clause naming why a skipped check was skipped; surfaced in the
    /// verdict line (`detail` carries the full reason).
    pub skip_reason: Option<String>,
    /// `--explain` material: the exact computed inputs/digests for the check.
    pub explain: Option<String>,
}

#[derive(Debug, Default)]
pub struct Transcript {
    pub checks: Vec<Check>,
    pub workload_keyset_digest: Option<String>,
}

impl Transcript {
    pub fn pass(&mut self, def: CheckDef, detail: impl Into<String>) {
        self.push(def, Status::Pass, detail.into(), None);
    }

    pub fn fail(&mut self, def: CheckDef, detail: impl Into<String>) {
        self.push(def, Status::Fail, detail.into(), None);
    }

    pub fn skip(&mut self, def: CheckDef, detail: impl Into<String>, short: impl Into<String>) {
        self.push(def, Status::Skip, detail.into(), Some(short.into()));
    }

    pub fn info(&mut self, def: CheckDef, detail: impl Into<String>) {
        self.push(def, Status::Info, detail.into(), None);
    }

    fn push(&mut self, def: CheckDef, status: Status, detail: String, skip_reason: Option<String>) {
        self.checks.push(Check {
            def,
            status,
            detail,
            skip_reason,
            explain: None,
        });
    }

    /// Attach `--explain` material to the most recently pushed check.
    pub fn explain(&mut self, text: impl Into<String>) {
        if let Some(check) = self.checks.last_mut() {
            check.explain = Some(text.into());
        }
    }

    pub fn count(&self, status: Status) -> usize {
        self.checks.iter().filter(|c| c.status == status).count()
    }

    /// VERIFIED: at least one check, none failed, and the hardware root of
    /// trust passed where present. The exit code and the JSON `verified` field
    /// both report this, so all three signals agree.
    pub fn verified(&self) -> bool {
        self.no_fails() && self.root_of_trust_ok()
    }

    /// At least one check and none failed. Skips never count as passes; the
    /// hardware root is judged separately (root_of_trust_ok) so the verdict can
    /// distinguish a failure from a merely-PARTIAL run.
    fn no_fails(&self) -> bool {
        !self.checks.is_empty() && self.count(Status::Fail) == 0
    }

    /// True unless L2.1 (the hardware root) is present but did not pass — an
    /// offline audit or browser leaves it a skip, which is PARTIAL, not
    /// VERIFIED. A receipt-only transcript has no L2.1 and is unaffected.
    fn root_of_trust_ok(&self) -> bool {
        self.checks
            .iter()
            .find(|c| c.def.id == "L2.1")
            .is_none_or(|c| c.status == Status::Pass)
    }

    pub fn verdict_line(&self) -> String {
        let passed = self.count(Status::Pass);
        let failed = self.count(Status::Fail);
        let skipped = self.count(Status::Skip);
        let mut reasons: Vec<&str> = Vec::new();
        for check in &self.checks {
            if let Some(reason) = check.skip_reason.as_deref() {
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
            }
        }
        let skip_clause = if skipped > 0 {
            format!(", {skipped} skipped: {}", reasons.join(", "))
        } else {
            String::new()
        };
        if !self.no_fails() {
            format!("NOT VERIFIED ({passed} pass, {failed} failed{skip_clause})")
        } else if !self.root_of_trust_ok() {
            format!("PARTIAL — hardware root not verified ({passed} pass{skip_clause})")
        } else {
            format!("VERIFIED ({passed} pass{skip_clause})")
        }
    }

    /// Aligned one-line-per-check rendering, verdict line last. With
    /// `include_explain`, each check's computed material follows it indented.
    pub fn render_human(&self, include_explain: bool) -> String {
        let mut out = String::new();
        for check in &self.checks {
            out.push_str(&format!(
                "{:<4}  {:<6} {} [{}]",
                check.status.marker(),
                check.def.id,
                check.def.title,
                check.def.section
            ));
            if !check.detail.is_empty() {
                out.push_str(" — ");
                out.push_str(&check.detail);
            }
            out.push('\n');
            if include_explain {
                if let Some(explain) = &check.explain {
                    for line in explain.lines() {
                        out.push_str("        | ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
        }
        out.push_str(&self.verdict_line());
        out.push('\n');
        out
    }

    /// Print the transcript to stdout — pretty JSON or the human rendering —
    /// and return the subcommand exit code (0 iff VERIFIED).
    pub fn print(&self, json: bool, explain: bool) -> Result<i32, String> {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&self.to_json(explain))
                    .map_err(|e| format!("failed to serialize transcript: {e}"))?
            );
        } else {
            print!("{}", self.render_human(explain));
        }
        Ok(if self.verified() { 0 } else { 1 })
    }

    pub fn to_json(&self, include_explain: bool) -> Value {
        let checks: Vec<Value> = self
            .checks
            .iter()
            .map(|check| {
                let mut obj = json!({
                    "id": check.def.id,
                    "section": check.def.section,
                    "title": check.def.title,
                    "status": check.status.as_str(),
                    "detail": check.detail,
                });
                if include_explain {
                    if let Some(explain) = &check.explain {
                        obj["explain"] = Value::String(explain.clone());
                    }
                }
                obj
            })
            .collect();
        json!({
            "checks": checks,
            "verdict": {
                "verified": self.verified(),
                "passed": self.count(Status::Pass),
                "failed": self.count(Status::Fail),
                "skipped": self.count(Status::Skip),
                "workload_keyset_digest": self.workload_keyset_digest,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_pass_is_verified() {
        let mut t = Transcript::default();
        t.pass(L2_2, "ok");
        t.pass(L2_3, "ok");
        assert!(t.verified());
        assert_eq!(t.verdict_line(), "VERIFIED (2 pass)");
    }

    #[test]
    fn l2_1_pass_is_fully_verified() {
        // The online path (verify/chat/serve) passes L2.1; the headline, exit
        // code, and JSON must all read VERIFIED — PARTIAL is unreachable here.
        let mut t = Transcript::default();
        t.pass(L2_1, "quote verified to vendor root");
        t.pass(L2_2, "ok");
        assert!(t.verified());
        assert!(t.verdict_line().starts_with("VERIFIED"));
        assert_eq!(t.to_json(false)["verdict"]["verified"], Value::Bool(true));
    }

    #[test]
    fn skip_is_never_treated_as_pass() {
        let mut t = Transcript::default();
        t.pass(L2_2, "ok");
        t.skip(L2_1, "quote collateral offline", "quote collateral offline");
        t.skip(
            L2_5,
            "custody profile not implemented in this CLI yet",
            "custody profile not implemented",
        );
        // L2.1 skipped: no failure, but not fully verified — PARTIAL, and the
        // exit code / JSON `verified` agree by gating on the same predicate.
        assert!(!t.verified());
        assert_eq!(t.count(Status::Pass), 1);
        assert_eq!(
            t.verdict_line(),
            "PARTIAL — hardware root not verified (1 pass, 2 skipped: quote collateral offline, custody profile not implemented)"
        );
    }

    #[test]
    fn any_fail_blocks_the_verdict() {
        let mut t = Transcript::default();
        t.pass(L2_2, "ok");
        t.fail(L2_3, "keyset expired");
        t.skip(L2_1, "quote collateral offline", "quote collateral offline");
        assert!(!t.verified());
        assert_eq!(
            t.verdict_line(),
            "NOT VERIFIED (1 pass, 1 failed, 1 skipped: quote collateral offline)"
        );
    }

    #[test]
    fn empty_transcript_is_not_verified() {
        assert!(!Transcript::default().verified());
    }

    #[test]
    fn info_lines_count_neither_way() {
        let mut t = Transcript::default();
        t.pass(R_1, "ok");
        t.info(R_NOTE, "request.forwarded differs from request.received");
        assert!(t.verified());
        assert_eq!(t.count(Status::Pass), 1);
        assert_eq!(t.verdict_line(), "VERIFIED (1 pass)");
        assert!(t.render_human(false).contains("INFO  R.note"));
        assert_eq!(t.to_json(false)["checks"][1]["status"], "info");
    }

    #[test]
    fn json_shape_carries_checks_and_verdict() {
        let mut t = Transcript {
            workload_keyset_digest: Some("sha256:cd".to_string()),
            ..Default::default()
        };
        t.pass(L2_2, "ok");
        t.explain("input: {}\ncomputed: sha256:ab");
        let v = t.to_json(true);
        assert_eq!(v["checks"][0]["id"], "L2.2");
        assert_eq!(v["checks"][0]["section"], "10.1(2)");
        assert_eq!(v["checks"][0]["status"], "pass");
        assert!(v["checks"][0]["explain"].is_string());
        assert_eq!(v["verdict"]["verified"], true);
        assert_eq!(v["verdict"]["passed"], 1);
        assert_eq!(v["verdict"]["workload_keyset_digest"], "sha256:cd");
        // Without explain requested, the field stays out of the wire shape.
        assert!(t.to_json(false)["checks"][0].get("explain").is_none());
    }
}
