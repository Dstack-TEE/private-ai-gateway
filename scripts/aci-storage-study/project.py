#!/usr/bin/env python3
"""Final projection: production shape x 90-day retention x storage layout.
Per-session marginal costs are MEASURED from the api.redpill.ai corpus."""

# measured marginals (bytes)
PHALA_DOC, PHALA_PATCH = 268_957, 1_570      # doc-level zstd --patch-from, avg of 6 rounds
NEAR_DOC,  NEAR_PATCH  = 116_054, 2_000      # evidence patch measured 1151B; doc-level est 2KB
CHUTES_DOC, CHUTES_PATCH = 1_600, 1_600      # per-instance docs; store raw (patch base is per-instance anyway)
TINFOIL_DOC = 2_717
PHALA_ZSTD_DOC, NEAR_ZSTD_DOC = 84_442, 40_000
PHALA_STREAM, NEAR_STREAM = 8_900, 3_000     # zstd stream marginal (no random access)
BASE_COST = 17 * 100_000                      # one zstd-compressed base doc per upstream

# measured production rates (sessions/hour)
PHALA_R, NEAR_R, CHUTES_R, TINFOIL_R = 217.2, 18.6, 116.9, 1.0
HOURS = 90 * 24

N_PHALA, N_NEAR, N_CHUTES, N_TINFOIL = (int(r * HOURS) for r in (PHALA_R, NEAR_R, CHUTES_R, TINFOIL_R))
print(f"90-day session counts: phala={N_PHALA:,} near={N_NEAR:,} chutes={N_CHUTES:,} tinfoil={TINFOIL_R*HOURS:,.0f}")
print()

def gb(b): return f"{b/1e9:8.2f} GB" if b >= 1e9 else f"{b/1e6:8.1f} MB"

rows = [
    ("L0 current JSONL (inline base64 evidence)",
     N_PHALA*PHALA_DOC + N_NEAR*NEAR_DOC + N_CHUTES*CHUTES_DOC + TINFOIL_R*HOURS*TINFOIL_DOC),
    ("L1 whole-evidence CAS by digest (dedup ~= 0, digest fresh每轮)",
     N_PHALA*(PHALA_DOC-67_000) + N_NEAR*(NEAR_DOC-29_000) + N_CHUTES*CHUTES_DOC),  # minus b64 overhead only
    ("L0z zstd -19 per document",
     N_PHALA*PHALA_ZSTD_DOC + N_NEAR*NEAR_ZSTD_DOC + N_CHUTES*CHUTES_DOC),
    ("L0z-stream zstd 流式压缩 (无随机读)",
     N_PHALA*PHALA_STREAM + N_NEAR*NEAR_STREAM + N_CHUTES*400),
    ("L4 doc patch-from base (zstd 差分, 可随机读)",
     N_PHALA*PHALA_PATCH + N_NEAR*NEAR_PATCH + N_CHUTES*CHUTES_PATCH + BASE_COST),
]
print("layout".ljust(60), "90d total".rjust(12))
per = [("raw", PHALA_DOC), ("blob-CAS", 201_000), ("zstd/doc", PHALA_ZSTD_DOC),
       ("zstd stream", PHALA_STREAM), ("patch-from", PHALA_PATCH)]
for name, total in rows:
    print(f"{name:60s} {gb(total):>12s}")

print()
print("phala 单 session 边际成本 (实测):")
for name, b in per:
    print(f"  {name:15s} {b:>10,} B   ({268957/b:6.1f}x vs raw)")

print()
print("== 组合策略: patch-from + verifier_cache_seconds 3.6min -> 1h (churn /16.7) ==")
n2 = int(N_PHALA/16.7)
total = n2*PHALA_PATCH + int(N_NEAR/16.7)*NEAR_PATCH + int(N_CHUTES/16.7)*CHUTES_PATCH + BASE_COST
print(f"90d total: {gb(total)}")
