#!/usr/bin/env python3
"""Level-2/3 churn analysis: decode varying fields to binary/JSON and diff."""
import json, base64, glob, os, hashlib

CORPUS = os.path.expanduser("~/workshop/aci-storage-study/corpus")

def load_series():
    docs = sorted(glob.glob(f"{CORPUS}/series-phala225/*.json"),
                  key=lambda p: json.load(open(p))["established_at"])
    return [json.loads(base64.b64decode(json.load(open(p))["evidence"]["data"].split(";base64,")[1])) for p in docs]

def byte_ranges(a: bytes, b: bytes):
    """Return list of (start, end) changed ranges between equal-length byte strings."""
    ranges, start = [], None
    n = min(len(a), len(b))
    for i in range(n):
        if a[i] != b[i]:
            if start is None: start = i
        elif start is not None:
            ranges.append((start, i)); start = None
    if start is not None: ranges.append((start, n))
    if len(a) != len(b): ranges.append((n, max(len(a), len(b))))
    return ranges

evs = load_series()
print("=" * 72)
print("A. intel_quote: hex -> binary, byte-range diff across 11 rounds")
print("=" * 72)
quotes = [bytes.fromhex(e["intel_quote"]) if all(c in "0123456789abcdefABCDEF" for c in e["intel_quote"]) else base64.b64decode(e["intel_quote"]) for e in evs]
q0 = quotes[0]
print(f"quote binary len: {len(q0)}  all same len: {len(set(map(len, quotes))) == 1}")
# version/tee from header
ver, ak, tee = int.from_bytes(q0[0:2], "little"), int.from_bytes(q0[2:4], "little"), q0[4:8].hex()
print(f"header: version={ver} att_key_type={ak} tee_type={tee}")
# accumulate which ranges ever change across all rounds
all_ranges = []
for q in quotes[1:]:
    all_ranges.extend(byte_ranges(q0, q))
# merge
all_ranges.sort()
merged = []
for s, e in all_ranges:
    if merged and s <= merged[-1][1] + 8:  # gap<8 merge for readability
        merged[-1] = (merged[-1][0], max(merged[-1][1], e))
    else:
        merged.append((s, e))
total_changed = sum(e - s for s, e in merged)
print(f"changed byte ranges (merged, gap<8): {len(merged)} ranges, {total_changed} bytes of {len(q0)}")
for s, e in merged:
    print(f"  [{s:5d}..{e:5d})  len={e-s}")

print()
print("=" * 72)
print("B. nvidia_payload.evidence: base64 decode -> byte diff across rounds")
print("=" * 72)
nvs = [json.loads(e["nvidia_payload"]) for e in evs]
ne0 = nvs[0]["evidence_list"][0]
print("evidence_list[0] keys:", sorted(ne0.keys()))
nb = [base64.b64decode(n["evidence_list"][0]["evidence"]) for n in nvs]
print(f"evidence binary len: {len(nb[0])}  same len across rounds: {len(set(map(len, nb))) == 1}")
# try to see if it is JSON
try:
    j = json.loads(nb[0])
    print("decodes as JSON, keys:", sorted(j.keys()) if isinstance(j, dict) else type(j).__name__)
except Exception:
    print("not JSON; head hex:", nb[0][:32].hex())
    print("head ascii:", "".join(chr(c) if 32 <= c < 127 else "." for c in nb[0][:64]))
ranges = []
for x in nb[1:]: ranges.extend(byte_ranges(nb[0], x))
ranges.sort()
merged = []
for s, e in ranges:
    if merged and s <= merged[-1][1] + 8: merged[-1] = (merged[-1][0], max(merged[-1][1], e))
    else: merged.append((s, e))
print(f"changed ranges (merged): {len(merged)}, {sum(e-s for s,e in merged)} bytes of {len(nb[0])}")
for s, e in merged[:20]: print(f"  [{s:5d}..{e:5d})  len={e-s}")

print()
print("=" * 72)
print("C. near-ai: instance changes per round — what differs at L2/L3")
print("=" * 72)
def near_ev(p):
    d = json.load(open(f"{CORPUS}/{p}"))
    return json.loads(base64.b64decode(d["evidence"]["data"].split(";base64,")[1]))["gateway_attestation"]
na = near_ev("session-1d21b481f4e4d135f90e7823299d1748b48baa7d4f0a016b11ef40fa2d8d05e7.json")
nb2 = near_ev("session-near-72c3.json")

# quote byte diff
qa, qb = bytes.fromhex(na["intel_quote"]), bytes.fromhex(na["intel_quote"] if False else nb2["intel_quote"])
print(f"intel_quote binary: {len(qa)} vs {len(qb)}")
rs = byte_ranges(qa, qb)
m = []
for s, e in rs:
    if m and s <= m[-1][1] + 8: m[-1] = (m[-1][0], max(m[-1][1], e))
    else: m.append((s, e))
print(f"changed ranges: {len(m)}, {sum(e-s for s,e in m)} bytes")
for s, e in m[:25]: print(f"  [{s:5d}..{e:5d}) len={e-s}")

# event_log: JSON string?
ela, elb = na["event_log"], nb2["event_log"]
print(f"\nevent_log len: {len(ela)} vs {len(elb)}")
try:
    ja, jb = json.loads(ela), json.loads(elb)
    print(f"event_log is JSON: {len(ja)} vs {len(jb)} events")
    for i, (xa, xb) in enumerate(zip(ja, jb)):
        if json.dumps(xa, sort_keys=True) != json.dumps(xb, sort_keys=True):
            print(f"  event[{i}] differs: {json.dumps(xa)[:100]}")
            print(f"               vs : {json.dumps(xb)[:100]}")
    if len(ja) != len(jb): print(f"  count differs: {len(ja)} vs {len(jb)}")
except Exception as ex:
    print("event_log not JSON:", ex)

# app_cert: PEM chain?
aca, acb = na["info"]["app_cert"], nb2["info"]["app_cert"]
print(f"\napp_cert len: {len(aca)} vs {len(acb)}, head: {aca[:40]!r}")

# app_cert PEM block diff
def pem_blocks(pem):
    blocks, cur = [], []
    for line in pem.splitlines():
        cur.append(line)
        if "END CERTIFICATE" in line:
            blocks.append("\n".join(cur)); cur = []
    return blocks
ca, cb = pem_blocks(aca), pem_blocks(acb)
print(f"app_cert chain: {len(ca)} vs {len(cb)} certs")
for i, (xa, xb) in enumerate(zip(ca, cb)):
    print(f"  cert[{i}] len={len(xa)} identical={xa == xb}")
    if xa != xb:
        import base64 as b64
        der_a = b64.b64decode("".join(xa.splitlines()[1:-1]))
        der_b = b64.b64decode("".join(xb.splitlines()[1:-1]))
        rs = byte_ranges(der_a, der_b)
        mm = []
        for s, e in rs:
            if mm and s <= mm[-1][1] + 8: mm[-1] = (mm[-1][0], max(mm[-1][1], e))
            else: mm.append((s, e))
        print(f"    DER {len(der_a)}B, changed ranges: {[(s, e, e-s) for s, e in mm[:12]]}")
