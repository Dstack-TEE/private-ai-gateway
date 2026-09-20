//! Session-log storage-pressure simulator for Chutes-style per-instance
//! attested sessions (PR #213 evaluation harness).
//!
//! Drives the *public* service API only: a real [`JsonlSessionStore`] on
//! disk behind a real [`AciService`], fed synthetic fleet-verification
//! rounds through [`UpstreamSessionSink::record_session`]. Nothing is
//! forwarded — the null upstream is never called — so this measures purely
//! the seal/dedup/append/compact/replay behaviour of the session log.
//!
//! What it answers:
//!
//! * **Growth rate** — log bytes over simulated time, under parameterized
//!   fleet size, evidence-bundle size, round cadence, TTL, and
//!   material-churn rate. Confirms (or refutes) the "one record per
//!   instance per validity window" bound empirically.
//! * **Steady-state footprint** — file size after each compaction.
//! * **Replay cost** — time to reopen the store at the end (the #145
//!   failure mode), plus peak RSS where the OS reports it.
//! * **Sealing CPU** — per-round `record_session` latency, which includes
//!   the per-dedup-hit `is_verifiable_bundle` re-hash of the stored
//!   evidence bundle.
//!
//! Example (24h, 100 instances, auto evidence size ≈ 7 KiB/instance,
//! 60s verification rounds, hourly compaction, 2 instance material
//! changes per hour):
//!
//! ```sh
//! cargo run --release --example session_log_pressure -- \
//!     --instances 100 --hours 24 --churn-per-hour 2 --out /tmp/sim.csv
//! ```
//!
//! CSV columns: sim_time_s, round, file_bytes, file_lines (only sampled at
//! compaction boundaries and at the end; -1 otherwise), round_ms_avg
//! (since the previous sample), compact_ms, compact_kept.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use ed25519_dalek::SigningKey as Ed25519SigningKey;
use serde_json::Value;

use private_ai_gateway::aci::digest;
use private_ai_gateway::aci::e2ee::{
    ed25519_public_key_hex, x25519_public_key_hex, x25519_secret_key_from_bytes,
    E2EE_ALGO_X25519_AESGCM,
};
use private_ai_gateway::aci::keys::{KeyError, KeyProvider, Quote, Quoter, ALGO_ED25519};
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::types::{KeyedPublicKey, TlsSpki};
use private_ai_gateway::aci::upstream::{
    UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, Clock, InMemoryReceiptStore,
};
use private_ai_gateway::aggregator::session_store::JsonlSessionStore;
use private_ai_gateway::aggregator::upstream_config::UpstreamSessionSink;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

struct Args {
    instances: usize,
    hours: u64,
    round_secs: u64,
    ttl_secs: u64,
    compact_secs: u64,
    evidence_kb: usize, // 0 = auto: real measurements body + instances × 31 KiB
    churn_per_hour: usize,
    out: Option<PathBuf>,
    dir: Option<PathBuf>,
    /// Real `/servers/tee/measurements` body embedded in every bundle (the
    /// fixed fleet-wide part of real Chutes evidence).
    measurements_file: Option<PathBuf>,
}

impl Args {
    fn parse() -> Self {
        let mut a = Args {
            instances: 50,
            hours: 24,
            round_secs: 60,
            ttl_secs: 3600,
            compact_secs: 3600,
            evidence_kb: 0,
            churn_per_hour: 0,
            out: None,
            dir: None,
            measurements_file: None,
        };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            let mut val = |name: &str| -> String {
                it.next().unwrap_or_else(|| panic!("{name} needs a value"))
            };
            match arg.as_str() {
                "--instances" => a.instances = val("--instances").parse().unwrap(),
                "--hours" => a.hours = val("--hours").parse().unwrap(),
                "--round-secs" => a.round_secs = val("--round-secs").parse().unwrap(),
                "--ttl" => a.ttl_secs = val("--ttl").parse().unwrap(),
                "--compact-secs" => a.compact_secs = val("--compact-secs").parse().unwrap(),
                "--evidence-kb" => a.evidence_kb = val("--evidence-kb").parse().unwrap(),
                "--churn-per-hour" => a.churn_per_hour = val("--churn-per-hour").parse().unwrap(),
                "--out" => a.out = Some(PathBuf::from(val("--out"))),
                "--dir" => a.dir = Some(PathBuf::from(val("--dir"))),
                "--measurements-file" => {
                    a.measurements_file = Some(PathBuf::from(val("--measurements-file")))
                }
                other => panic!("unknown flag {other}"),
            }
        }
        assert!(a.instances > 0, "--instances must be positive");
        assert!(a.round_secs > 0, "--round-secs must be positive");
        a
    }
}

// ---------------------------------------------------------------------------
// Deterministic PRNG for quote blobs
// ---------------------------------------------------------------------------

struct XorShift(u64);

impl XorShift {
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            out.extend_from_slice(&self.0.to_le_bytes());
        }
        out.truncate(n);
        out
    }
}

// ---------------------------------------------------------------------------
// Minimal public-trait stubs (the upstream is never called)
// ---------------------------------------------------------------------------

struct SimKeys {
    receipt: Ed25519SigningKey,
    e2ee: x25519_dalek::StaticSecret,
}

impl KeyProvider for SimKeys {
    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        vec![KeyedPublicKey {
            key_id: "sim-receipt".to_string(),
            algo: ALGO_ED25519.to_string(),
            public_key_hex: ed25519_public_key_hex(&self.receipt),
        }]
    }

    fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        if key_id != "sim-receipt" {
            return Err(KeyError::UnknownReceiptKeyId(key_id.to_string()));
        }
        use ed25519_dalek::Signer;
        Ok(self.receipt.sign(payload).to_bytes().to_vec())
    }

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        vec![KeyedPublicKey {
            key_id: "sim-e2ee-x25519".to_string(),
            algo: E2EE_ALGO_X25519_AESGCM.to_string(),
            public_key_hex: x25519_public_key_hex(&self.e2ee),
        }]
    }

    fn tls_spkis(&self) -> Vec<TlsSpki> {
        Vec::new()
    }

    fn is_test_only(&self) -> bool {
        true
    }
}

struct SimQuoter;

#[async_trait::async_trait]
impl Quoter for SimQuoter {
    async fn get_quote(&self, report_data: [u8; 32]) -> Result<Quote, KeyError> {
        Ok(Quote {
            raw_quote: b"sim-quote".to_vec(),
            report_data: report_data.to_vec(),
            event_log: Value::Null,
            vm_config: Value::Null,
            app_compose: None,
        })
    }

    async fn get_quote_raw(&self, report_data: [u8; 64]) -> Result<Quote, KeyError> {
        Ok(Quote {
            raw_quote: b"sim-quote".to_vec(),
            report_data: report_data.to_vec(),
            event_log: Value::Null,
            vm_config: Value::Null,
            app_compose: None,
        })
    }
}

struct NullUpstream;

#[async_trait::async_trait]
impl UpstreamBackend for NullUpstream {
    fn name(&self) -> &str {
        "chutes-sim"
    }

    fn url_origin(&self) -> Option<&str> {
        Some("https://sim.upstream")
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Routing(
            "session-pressure simulation never forwards".to_string(),
        ))
    }
}

struct SimClock(Arc<AtomicU64>);

impl Clock for SimClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Fleet event synthesis
// ---------------------------------------------------------------------------

/// A fleet-wide, nonce-bound evidence bundle shaped like the real Chutes
/// verifier output: the public `/servers/tee/measurements` body (~39 KiB,
/// fixed across rounds) plus, per instance, a TDX quote (~7 KiB in JSON)
/// and per-GPU NVIDIA evidence (~3 KiB × 8 GPUs). Multipart framing aside,
/// the bytes land in the session document as one base64 `data:` URI with a
/// `sha256:` digest over the decoded bytes (§8.2).
fn fleet_evidence(
    round: u64,
    instances: usize,
    measurements: &[u8],
    target_bytes: usize,
    rng: &mut XorShift,
) -> Value {
    let mut payload = serde_json::json!({
        "nonce": format!("{round:032x}"),
        "measurements_b64": BASE64.encode(measurements),
        "instances": (0..instances)
            .map(|i| serde_json::json!({
                "instance_id": format!("instance-{i}"),
                "quote": BASE64.encode(rng.bytes(5 * 1024)),
                "gpu_evidence": (0..8)
                    .map(|g| serde_json::json!({
                        "gpu": g,
                        "evidence": BASE64.encode(rng.bytes(2 * 1024)),
                        "certificate": BASE64.encode(rng.bytes(1024)),
                    }))
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });
    let mut bytes = serde_json::to_vec(&payload).unwrap();
    if target_bytes > bytes.len() {
        payload["padding"] = Value::String(BASE64.encode(rng.bytes(target_bytes - bytes.len())));
        bytes = serde_json::to_vec(&payload).unwrap();
    }
    serde_json::json!({
        "digest": digest::sha256_hex(&bytes),
        "data": format!("data:application/json;base64,{}", BASE64.encode(&bytes)),
    })
}

/// One fleet verification round: every instance bound, per-instance claims,
/// a fleet-aggregate GPU flag that alternates every round (must NOT leak
/// into per-instance fingerprints), and TCB flips for churned instances.
fn fleet_event(
    round: u64,
    instances: usize,
    evidence: Value,
    tcb_outdated: &[bool],
) -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        upstream_name: "chutes-sim".to_string(),
        provider_type: Some("chutes".to_string()),
        model_id: "sim-model".to_string(),
        url_origin: Some("https://sim.upstream".to_string()),
        verifier_id: "private-ai-verifier/chutes/v1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: Some(evidence),
        channel_bindings: (0..instances)
            .map(|i| ChannelBinding::E2eePublicKeySha256 {
                provider: "chutes".to_string(),
                key_id: Some(format!("instance-{i}")),
                algorithm: "chutes-ml-kem-768".to_string(),
                public_key_sha256: format!("{:02x}", i % 256).repeat(32),
            })
            .collect(),
        provider_claims: Some(serde_json::json!({
            "trust_boundary": "model_instance",
            "chute_id": "chute-sim",
            "gpu_verified": round % 2 == 0,
            "verified_instance_ids": (0..instances).map(|i| format!("instance-{i}")).collect::<Vec<_>>(),
            "instance_tcb_statuses": (0..instances)
                .map(|i| {
                    (
                        format!("instance-{i}"),
                        Value::String(if tcb_outdated[i] { "OutOfDate" } else { "UpToDate" }.to_string()),
                    )
                })
                .collect::<serde_json::Map<_, _>>(),
            "instance_measurements": (0..instances)
                .map(|i| (format!("instance-{i}"), Value::String("profile-x".to_string())))
                .collect::<serde_json::Map<_, _>>(),
            "instance_gpu": (0..instances)
                .map(|i| (format!("instance-{i}"), serde_json::json!({ "gpu_verified": true, "gpu_arch": "hopper" })))
                .collect::<serde_json::Map<_, _>>(),
        })),
    }
}

// ---------------------------------------------------------------------------
// Measurement helpers
// ---------------------------------------------------------------------------

fn file_bytes(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn file_lines(path: &std::path::Path) -> u64 {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .count() as u64
}

/// Peak RSS in KiB where the OS reports it (Linux /proc); None elsewhere.
fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches(" kB").parse().ok())
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

// ---------------------------------------------------------------------------
// Main simulation
// ---------------------------------------------------------------------------

fn main() {
    let args = Args::parse();
    let dir = args.dir.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("session-sim-{}", std::process::id()))
    });
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("sessions.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(log_path.with_extension("jsonl.lock"));

    let measurements = match &args.measurements_file {
        Some(p) => std::fs::read(p).expect("measurements file must be readable"),
        None => Vec::new(),
    };
    let evidence_bytes = if args.evidence_kb > 0 {
        args.evidence_kb * 1024
    } else {
        // Auto: real measurements body + per-instance quote (7 KiB JSON) and
        // 8 × 3 KiB GPU evidence — the real Chutes bundle shape.
        let auto = measurements.len() + args.instances * 31 * 1024;
        // fleet_evidence pads with ~4/3 base64 overhead; deflate the target
        // so the DECODED bundle lands near `auto`.
        auto
    };
    let rounds = args.hours * 3600 / args.round_secs;

    eprintln!("== session-log pressure simulation ==");
    eprintln!("log file:        {}", log_path.display());
    eprintln!("instances:       {}", args.instances);
    eprintln!(
        "evidence bundle: {} KiB ({:.2} MiB)",
        evidence_bytes / 1024,
        evidence_bytes as f64 / 1024.0 / 1024.0
    );
    eprintln!(
        "rounds:          {rounds} (every {}s over {}h)",
        args.round_secs, args.hours
    );
    eprintln!(
        "ttl:             {}s   compact: {}s   churn: {}/h",
        args.ttl_secs, args.compact_secs, args.churn_per_hour
    );

    let start_secs = 1_700_000_000u64;
    let now = Arc::new(AtomicU64::new(start_secs));
    let store = Arc::new(JsonlSessionStore::open(&log_path, start_secs).unwrap());
    let mut cfg = AciServiceConfig::for_test();
    cfg.receipt_ttl_seconds = args.ttl_secs;
    let svc = AciService::new(
        Arc::new(SimKeys {
            receipt: Ed25519SigningKey::from_bytes(&[0x66; 32]),
            e2ee: x25519_secret_key_from_bytes(&[0x55; 32]).unwrap(),
        }),
        Arc::new(SimQuoter),
        Arc::new(NullUpstream),
        Arc::new(InMemoryReceiptStore::default()),
        cfg,
        Arc::new(SimClock(now.clone())),
    )
    .unwrap()
    .with_session_store(store.clone());

    let mut csv: Box<dyn Write> = match &args.out {
        Some(p) => Box::new(std::fs::File::create(p).unwrap()),
        None => Box::new(std::io::stdout()),
    };
    writeln!(
        csv,
        "sim_time_s,round,file_bytes,file_lines,round_ms_avg,compact_ms,compact_kept"
    )
    .unwrap();

    let mut rng = XorShift(0x9e3779b97f4a7c15);
    let mut tcb_outdated = vec![false; args.instances];
    let mut next_compact = args.compact_secs;
    let mut round_ms_total = 0.0f64;
    let mut round_ms_samples = 0u64;
    let mut round_ms_max = 0.0f64;
    let mut file_peak = 0u64;
    let sim_start = Instant::now();

    for round in 0..rounds {
        let t = start_secs + round * args.round_secs;
        now.store(t, Ordering::Relaxed);

        // Material churn: at each hour boundary flip `churn_per_hour`
        // instances (round-robin), forcing a reseal of exactly those.
        if args.churn_per_hour > 0 && round > 0 && (t - start_secs) % 3600 == 0 {
            let hour = (t - start_secs) / 3600;
            for j in 0..args.churn_per_hour {
                let idx = ((hour as usize * args.churn_per_hour) + j) % args.instances;
                tcb_outdated[idx] = !tcb_outdated[idx];
            }
        }

        let evidence = fleet_evidence(
            round,
            args.instances,
            &measurements,
            evidence_bytes,
            &mut rng,
        );
        let event = fleet_event(round, args.instances, evidence, &tcb_outdated);

        let t0 = Instant::now();
        svc.record_session(&event);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        round_ms_total += ms;
        round_ms_samples += 1;
        round_ms_max = round_ms_max.max(ms);

        let sim_time = t - start_secs;
        let mut compact_ms = -1.0f64;
        let mut compact_kept = -1i64;
        let mut lines = -1i64;
        if args.compact_secs > 0 && sim_time >= next_compact {
            next_compact += args.compact_secs;
            let c0 = Instant::now();
            let kept = store
                .compact(t)
                .unwrap_or_else(|e| panic!("compaction failed at {sim_time}s: {e}"));
            compact_ms = c0.elapsed().as_secs_f64() * 1000.0;
            compact_kept = kept as i64;
            lines = file_lines(&log_path) as i64;
        }

        let bytes = file_bytes(&log_path);
        file_peak = file_peak.max(bytes);

        // One CSV sample per simulated hour (plus compaction rows above).
        let sample_due = (sim_time + args.round_secs) % 3600 < args.round_secs
            || round == rounds - 1
            || compact_ms >= 0.0;
        if sample_due {
            let avg = round_ms_total / round_ms_samples.max(1) as f64;
            writeln!(
                csv,
                "{sim_time},{round},{bytes},{lines},{avg:.3},{compact_ms:.1},{compact_kept}"
            )
            .unwrap();
        }

        if sim_time % 3600 < args.round_secs {
            let avg = round_ms_total / round_ms_samples.max(1) as f64;
            eprintln!(
                "  t+{:>3}h: {:.2} MiB, round avg {:.1} ms (max {:.1})",
                sim_time / 3600,
                mib(bytes),
                avg,
                round_ms_max
            );
        }
        if sample_due {
            round_ms_total = 0.0;
            round_ms_samples = 0;
        }
    }

    // Final state + replay cost (the #145 failure mode): reopen the store.
    let final_bytes = file_bytes(&log_path);
    let final_lines = file_lines(&log_path);
    drop(svc);
    drop(store);
    let r0 = Instant::now();
    let reopened = JsonlSessionStore::open(&log_path, now.load(Ordering::Relaxed)).unwrap();
    let replay_ms = r0.elapsed().as_secs_f64() * 1000.0;
    drop(reopened);

    eprintln!();
    eprintln!("== results ==");
    eprintln!(
        "wall time:        {:.1}s",
        sim_start.elapsed().as_secs_f64()
    );
    eprintln!("records appended: {final_lines}");
    eprintln!("log peak:         {:.2} MiB", mib(file_peak));
    eprintln!("log final:        {:.2} MiB", mib(final_bytes));
    eprintln!("replay (open):    {replay_ms:.1} ms");
    eprintln!("round max:        {round_ms_max:.1} ms");
    if let Some(rss) = peak_rss_kib() {
        eprintln!("peak RSS:         {:.2} MiB", rss as f64 / 1024.0);
    } else {
        eprintln!("peak RSS:         (unavailable on this OS; wrap with /usr/bin/time -l)");
    }
    eprintln!(
        "expected bound:   {} records/window-hour (instances) + churn",
        args.instances
    );
    eprintln!("log kept at:      {}", log_path.display());
}
