# ACI Session Storage Measurement Study

Data source: production `api.redpill.ai` — a snapshot of `/v1/aci/sessions`,
11 consecutive full session records for one phala upstream, 10 for near-ai,
and one full record per upstream. Scripts: `scripts/aci-storage-study/`
(corpus not committed).

## 1. Production shape (snapshot, measured)

- 17 upstreams, **342 live sessions** within a one-hour window — the
  abbreviated list response alone is already 614KB.
- Session creation rate (driven by `verifier_cache_seconds`, ~3.6min in
  production):

| verifier | upstreams | sessions/h | sessions/h per upstream |
|---|---|---|---|
| phala-direct | 13 | 217 | 16.7 |
| chutes (per-instance) | 2 | 117 | 58 |
| near-ai-gateway | 1 | 18.6 | 18.6 |
| tinfoil | 1 | 1 | 1 |

- Full document sizes: phala **269KB**, near-ai **116KB**, chutes 1.6KB,
  tinfoil 2.7KB.
- 90-day session volume: phala 469k + near 40k + chutes 253k ≈ **760k records**.

## 2. What changes between sessions (core finding)

Document level: across rounds only `established_at` / `expires_at` /
`evidence.digest` / `evidence.data` change.

Evidence level (phala, 200KB raw bytes):

- **~50% internal duplication**: `all_attestations[0]` is a byte-for-byte
  copy of the top-level fields; `quote == intel_quote`.
- **Stable across 11 rounds, ~68KB ×2 copies**: `info` (62KB, incl.
  `tcb_info` 42KB, `app_cert` 19KB) + `event_log` (5.4KB).
- **Changes every round**: `quote` (10KB hex) + `nvidia_payload` (12KB) +
  nonce. But inside the changing fields, only a few hundred bytes of
  nonce/signature are genuinely fresh entropy.
- near-ai: the instance changes every round, yet large fields such as
  `app_compose` (40KB) remain stable across rounds.

## 3. Storage layouts compared (per-session marginal cost, measured)

| Layout | Marginal bytes | vs raw | Random read |
|---|---|---|---|
| L0 current JSONL (inline base64 evidence) | 268,957 | 1× | ✓ |
| L1 whole-evidence CAS by digest | ~201,000 | 1.3× | ✓ (dedup ≈ 0: the digest is fresh every round) |
| L0z zstd -19 per document | 84,442 | 3.2× | ✓ |
| zstd stream compression over the log | ~8,900 | 30× | ✗ |
| **L4 zstd --patch-from against a per-upstream base** | **1,570** | **171×** | ✓ (base+patch) |

- Patch roundtrips verified by sha256; evidence-level patches are 500B/round,
  near-ai 1.15KB/round.
- Whole-document patching (1.57KB) beats evidence patch + skeleton (2.7KB)
  and is the simplest delta option — but see §6 for why we do not delta the
  small volatile fields at all.
- Verified: JCS = sorted compact JSON on every real document (session id
  recomputation succeeds), so the storage layer may re-encode freely as long
  as reconstruction is byte-exact.

## 4. 90-day retention projection (at production churn)

| Layout | 90-day total |
|---|---|
| L0 current | **131 GB** |
| L1 blob CAS | 99 GB |
| zstd per document | 42 GB |
| zstd stream | 4.4 GB |
| patch-from | **1.2 GB** |
| patch-from + verifier cache 3.6min→1h | **75 MB** |

## 5. Conclusions (first round)

1. Naive whole-evidence CAS dedup is nearly useless — the evidence digest is
   fresh every round (fresh nonce).
2. Byte-level delta (zstd patch-from against a per-upstream base document)
   beats structural/columnar splitting: 1.57KB/session, 171×, keeps random
   reads, zero schema coupling, byte-exact reconstruction.
3. Integrity is backed by the hash chain: after reconstruction, recomputing
   `evidence.digest` + `session_id` exposes any storage corruption or
   tampering.
4. Base evolution policy: when an upstream redeploys, patches grow; promote
   the current document to a new base above a threshold (e.g. 30KB).
5. Churn itself is governed by `verifier_cache_seconds` — an independent
   policy knob; multiplied by retention it decides the total.
6. Side findings: the abbreviated list response is already 614KB and grows
   with churn; session validity currently equals the receipt TTL (1h) —
   three separate lifetimes conflated.

## 6. Churn at L2/L3: the irreducible increment

Method: decode the changing fields to binary/sub-structure and diff byte
ranges across rounds (`deep_diff.py`).

### phala-direct (same instance, 11 rounds)

| Field | Encoded size | Bytes changed per round | What changes |
|---|---|---|---|
| `intel_quote` (hex→bin) | 5,006 B | **164 B** | `[600..632)` report_data tail (nonce-derived hash) + `[636..700)` ECDSA signature (randomized re-signing) + `[1154..1218)` QE report signature |
| `nvidia_payload.evidence` (b64→bin) | 4,129 B | **160 B** | `[4..36)` nonce + `[3565..3597)` hash echo + `[4033..4129)` trailing signature |
| `nvidia_payload.certificate` | 6.4 KB | 0 | GPU certificate chain stable across all 11 rounds |
| `info` / `event_log` / the rest | ~136 KB | 0 | Fully stable (incl. the internal duplicate copy) |

**Per-round genuine novelty ≈ 324 changed bytes, of which random entropy ≈
32B nonce + signature randomness.** The quote's mrtd/rtmr0-3 and the PCK
certificate chain (~3.8KB certification data) are all stable.

### near-ai (instance change every round, 2-round comparison)

| Field | Per-round change |
|---|---|
| `intel_quote` (5,006 B) | **148 B**: rtmr3 (48B, instance measurement) + report_data tail + signatures |
| `event_log` (29 events) | **only event[23]** (IMR3 digest, 96 hex chars) |
| `info.app_cert` (3-cert chain) | root unchanged; intermediate ~80B (serial/validity/signature); leaf (12.6KB DER) ~620B spread over serial/key/extensions/signature |
| Others | `mr_aggregated` 48B, `instance_id`, `vpc_hostname` ~20B |

**Instance-rotation marginal increment ≈ 1–1.3KB** (mostly derived changes
from certificate re-signing; true random entropy is still a few hundred
bytes).

### Conclusion

- The irreducible per-round increment is a few hundred bytes to ~1.3KB,
  matching the measured zstd patch-from sizes (0.5KB same-instance / 1.15KB
  instance-change) — delta storage is already at the information-theoretic
  floor.
- This also disproves the ceiling of any field-level/columnar scheme: all
  such schemes can only eliminate repeated storage of stable fields — the
  increment itself (quote diff regions, signatures, re-signed cert regions)
  is this round's new fact and cannot be schema'd away.
- Of the 200KB evidence, genuinely new information per round is <0.7%; the
  other 99.3% is stable measurements, certificate chains, and serialization
  duplication.

## 7. Cross-upstream, per-field measurement (13 phala upstreams × full records)

Scripts: `cross_diff.py`, `deep_cross.py`.

### 7.1 Production topology findings

- `phala-225` and `phala-87` are **the same dstack app** (app_id /
  instance_id / app_cert / app_compose all identical) behind two endpoints
  serving two models — cross-upstream dedup exists in production today.
- The qwen3-8-27b six-node cluster (212–217): same model, same image, six
  deployments.
- `phala-204` (glm-5-3) runs on a different platform generation:
  mrtd/rtmr0-2/os_image_hash/vm_config/nvidia.arch all differ from the
  other 12 nodes; its document is 499KB.

### 7.2 Field lifecycle taxonomy (measured unique values / 13 nodes)

| Field | Lifecycle | uniq/13 | Size | Evidence |
|---|---|---|---|---|
| `vm_config`, `mrtd`, `rtmr0-2`, `os_image_hash`, `device_id`, `key_provider_info`, `nvidia.arch` | **platform generation** (changes only on image/firmware upgrades) | 1–2 | 0.4–2.3KB | 12 nodes share one value |
| `app_cert` root (cert[2]) | **cluster CA** | 3 | 615B | qwen cluster shares one root |
| `app_compose` (minus `docker_compose_file`) | **model deployment template** | shared per model | 18.5KB | only 1 of 22 leaf fields differs across six qwen nodes |
| `docker_compose_file` | **per-app** (but only ~400B/9.2KB differs within a model) | 12 | 9.8KB | line diff across six nodes: 6 of 263 lines (domain/cert-path/QOS string) |
| `app_cert` leaf+intermediate, `app_id`, `instance_id`, `compose_hash`, `mr_aggregated`, `rtmr3` | **per-app / redeploy** | 12 | leaf cert ~17KB | unique per app; stable across 11 rounds within one app |
| `event_log` | **per-boot** (but 27/30 events identical across same-platform nodes; only 3 IMR3 events are per-app) | 12 | 5.4–7.2KB | cross-node event-level diff |
| `nvidia.certificate` | **per-GPU** | 12 | 6.4KB | one per GPU, stable across rounds |
| `signing_address`, `tls_cert_fingerprint` | **per-channel** | 13 | <100B | unique per endpoint |
| `intel_quote`, `nvidia evidence`, `request_nonce` | **per-round** (every verification) | ∞ | 5KB + 4.1KB | 164B/160B byte-level change, but the whole field must be retained |
| `all_attestations[0]`, `quote == intel_quote` | **intra-document duplicate** | — | ~100KB | byte-identical |

### 7.3 Consequence for the storage design

~~"One bucket of large stable fields"~~ is wrong. The right model: **per-field
CAS, where every chunk carries its own lifecycle**, GC by reference count —
platform fields are rewritten only on platform upgrades, app fields on
redeploys, GPU certificates on hardware changes, per-round fields every
session. Cross-upstream/cross-cluster dedup (one app behind two endpoints,
same-model multi-deployments, same-platform nodes) falls out of content
addressing for free, with no schema knowledge of which field belongs to
which class. The only rules needed are generic:

1. Large strings become chunks;
2. Strings that parse as JSON (`nvidia_payload`, `app_compose`) recurse;
3. Identical content automatically shares a chunk.

### 7.4 Revised 90-day projection (production churn unchanged)

| Item | Volume |
|---|---|
| per-round (phala) | quote 10KB + nvidia evidence 5.5KB + skeleton 2.2KB ≈ **18KB/session** × 469k ≈ 8.4GB |
| per-round (near-ai; instance change also rewrites event_log/app_cert) | ~36KB × 40k ≈ 1.4GB |
| chutes / tinfoil | ≈ 0.4GB |
| per-app / per-platform / per-GPU (low frequency) | ~1MB total |
| **Total** | **~10GB** (~7GB after hex/base64 binary normalization) |

Compare: current 131GB; whole-evidence CAS 99GB; byte-level patch-from
1.2GB (rejected — delta-encoding the small volatile fields is pointless,
see §6).

Known exception: near-ai's per-round instance change makes `app_cert`
(18.5KB) a fresh chunk every round (+~700MB over 90 days). Acceptable as-is;
if near-ai-class upstreams multiply, add one generic rule that splits PEM
chains into certificate blocks (the root is shared cluster-wide, the
intermediate nearly stable, only the leaf is new each round).

## 8. near-ai time-series correction (10 rounds, 1840s span)

Script: `near_series.py`.

### 8.1 Key fact: instances rotate; they do not multiply

Only **2 distinct `instance_id` values across 10 rounds**, alternating
(except the last three rounds on one instance):

| Field | uniq/10 | Meaning |
|---|---|---|
| `instance_id` / `rtmr3` / `mr_aggregated` / `vpc_hostname` | 2 | two fixed instances, round-robin |
| `app_cert` | **2** | each instance's certificate chain is fixed |
| `event_log` | **2** | one per instance, reused across rounds |
| `app_cert` root cert[2] | **1** | both instances share the root |
| `intel_quote` | 10 | the only genuinely per-round large field |

Adjacent-round event_log diff: on instance switch, only event[23] (IMR3
digest) differs; on same instance, **zero difference**.

### 8.2 The §7.4 "near-ai exception" does not exist

The earlier estimate assumed a brand-new instance every round →
`app_cert` (18.5KB) + `event_log` (7.2KB) becoming fresh chunks each round.
In reality the instance set is small and stable, and **field-level CAS
content addressing dedups automatically** — each instance's certificate
chain and event_log is stored once and merely re-referenced every round.
near-ai's per-round marginal is isomorphic to phala's:

```
quote (hex 10KB) + nonce + skeleton ≈ 12–13KB/session
```

New chunks appear only when a **new instance** shows up (scale-out,
redeploy) — deployment-event-driven, not verification-rhythm-driven.

### 8.3 Delta also works (for reference)

zstd --patch-from between adjacent rounds: app_cert ~723B, event_log ~116B,
intel_quote ~233B, whole evidence ~981B (adjacent same-instance rounds only
171–249B). near-ai is indeed delta-storable, but there is no need once
field-CAS brings the marginal to ~13KB — consistent with the §6 conclusion
that delta-encoding small fields is pointless.

### 8.4 Corrected 90-day projection

| Item | Volume |
|---|---|
| phala (469k sessions × ~18KB) | 8.4GB |
| near-ai (40k × ~13KB) | 0.5GB (was 1.4GB) |
| chutes / tinfoil | 0.4GB |
| low-frequency fields (per-app/per-instance/per-platform/per-GPU) | ~1MB |
| **Total** | **~9.3GB** (~6.5GB after hex/base64 normalization) |

## 9. 1-day retention: the two stores compared (end-to-end simulation, byte-verified)

Script: `store_sim3.py`. Method: both storage engines are **fully simulated**
over the real corpus (not estimated); every document asserts that the
rebuilt bytes equal the original byte-for-byte.

### 9.1 Simulated CAS rules (all generic, zero provider schema)

- R0 `evidence.data` data URIs are decoded; a compact-JSON payload is
  recursed into, anything else becomes a binary chunk;
- R1 strings that are compact JSON (either ASCII-escaped or UTF-8 flavor,
  accepted only when re-encoding matches byte-for-byte) are recursed into;
- R2 hex strings → binary chunks; R3 base64 → binary chunks; R4 other large
  strings → verbatim chunks;
- R6 **merkle-ized subtrees**: after processing children, any dict/list whose
  canonical form exceeds 1KB becomes a chunk (reference structures are always
  serialized in insertion order — sorting them would corrupt nested
  order-sensitive embedded contexts);
- Identical content automatically shares a chunk (intra-document duplicates,
  cross-round stability, cross-upstream sharing, instance rotation — all
  dedup naturally).

### 9.2 Per-session marginal cost (real-corpus simulation)

| Corpus | Current L0 | CAS marginal | Ratio | Roundtrip |
|---|---|---|---|---|
| phala-direct ×11 (same instance) | 268,957 B | **13,142 B** | 20.5× | 11/11 byte-exact |
| near-ai ×10 (instance rotation) | 116,054 B | **10,505 B** | 11.0× | 10/10 |
| cross-upstream phala ×13 (shared store) | 281,911 B | 67,635 B¹ | 4.2× | 13/13 |
| chutes / tinfoil | ~1.5–2.7KB | ≈1:1 (no evidence) | — | all exact |

¹ Each upstream's first-seen per-app chunks (certificates, event_log, …)
are counted in the marginal here; amortized to ~0 in steady state.

phala's 13.1KB = quote binary 5,006B + nvidia evidence binary 4,129B +
per-round fresh skeleton/path nodes ~4KB.

### 9.3 1-day retention comparison (production churn: 8,489 sessions/day)

| Upstream class | sessions/day | Current/day | CAS/day |
|---|---|---|---|
| phala ×13 | 5,213 | 1,402.0 MB | 68.5 MB |
| near-ai | 446 | 51.8 MB | 4.7 MB |
| chutes ×2 | 2,806 | 4.5 MB | 4.5 MB |
| tinfoil | 24 | 0.1 MB | 0.1 MB |
| **Total** | **8,489** | **1.458 GB** | **77.8 MB** |

**Steady-state store at 1-day retention: 1.46 GB current vs 78 MB CAS
(+~1MB low-frequency chunks) — 18.8×.**

At a 1-day horizon, per-round fields dominate completely (all low-frequency
fields together are ~1MB). The savings, in decreasing order of contribution:
intra-document duplicate elimination (aa[0], quote==intel_quote) > cross-round
stable-field dedup > hex/base64 binary normalization. Cross-upstream dedup
barely matters in a 1-day window; its value shows up at 30/90-day retention
(§7.4).

### 9.4 Known edges

- near-ai's `tcb_info.app_compose` (38KB) is pretty-printed JSON; R1 rejects
  it, so it is stored as a verbatim chunk (one per instance, dedups fine). A
  future "lenient parse + deterministic re-encode" rule could capture it
  without touching anything else.
- Chunks are not zstd-compressed. Per-round chunks are random bytes
  (signatures/quotes) that do not compress; the skeleton is already down to
  references. Compression is negligible here and intentionally skipped.
