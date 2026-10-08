#!/usr/bin/env python3
"""Deep cross-upstream diff: app_compose / app_cert / event_log across phala nodes."""
import json, base64, glob, os, hashlib
from collections import defaultdict

CORPUS = os.path.expanduser("~/workshop/aci-storage-study/corpus")

def load_ev(p):
    doc = json.load(open(p))
    return json.loads(base64.b64decode(doc["evidence"]["data"].split(";base64,")[1]))

def h(s): return hashlib.sha256(s.encode() if isinstance(s, str) else s).hexdigest()[:12]

evs = {}
for p in sorted(glob.glob(f"{CORPUS}/cross/phala-*.json")):
    evs[os.path.basename(p)[:-5]] = load_ev(p)

qwen = [n for n in evs if n in ("phala-212", "phala-213", "phala-214", "phala-215", "phala-216", "phala-217")]

print("=" * 72)
print("A. app_compose across qwen3-8-27b nodes (6 nodes)")
print("=" * 72)
ac = {n: evs[n]["info"]["tcb_info"]["app_compose"] for n in qwen}
n0 = qwen[0]
s0 = ac[n0]
print("type:", type(s0).__name__, "len:", len(s0))
try:
    j0 = json.loads(s0)
    print("parses as JSON, keys:", sorted(j0.keys()))
    js = {n: json.loads(ac[n]) for n in qwen}
    def walk(d, pfx=""):
        out = {}
        if isinstance(d, dict):
            for k, v in d.items(): out.update(walk(v, f"{pfx}.{k}" if pfx else k))
        elif isinstance(d, list):
            for i, v in enumerate(d): out.update(walk(v, f"{pfx}[{i}]"))
        else: out[pfx] = d
        return out
    w0 = walk(j0)
    diff_counts = defaultdict(int)
    for n in qwen[1:]:
        wn = walk(js[n])
        for k in set(w0) | set(wn):
            if w0.get(k) != wn.get(k): diff_counts[k] += 1
    stable_bytes = sum(len(json.dumps(v)) for k, v in w0.items() if k not in diff_counts)
    total_bytes = sum(len(json.dumps(v)) for v in w0.values())
    print(f"leaf fields differing vs {n0}: {len(diff_counts)} of {len(w0)}")
    for k, c in sorted(diff_counts.items(), key=lambda kv: -kv[1]):
        print(f"  {k[:70]:70s} differs in {c}/5 peers  size={len(json.dumps(w0.get(k)))}")
    print(f"stable leaf bytes: {stable_bytes} / {total_bytes} ({100*stable_bytes//total_bytes}%)")
except Exception as e:
    print("not JSON:", e)

print()
print("=" * 72)
print("B. app_cert chain positions across ALL 13 nodes")
print("=" * 72)
def pem_blocks(pem):
    blocks, cur = [], []
    for line in pem.splitlines():
        cur.append(line)
        if "END CERTIFICATE" in line:
            blocks.append("\n".join(cur)); cur = []
    return blocks
chains = {n: pem_blocks(evs[n]["info"]["app_cert"]) for n in evs}
maxlen = max(len(c) for c in chains.values())
for pos in range(maxlen):
    groups = defaultdict(list)
    for n, c in chains.items():
        groups[h(c[pos]) if pos < len(c) else "NONE"].append(n.replace("phala-", ""))
    sizes = [len(c[pos]) for c in chains.values() if pos < len(c)]
    print(f"cert[{pos}] uniq={len(groups)} sizes={sorted(set(sizes))[:4]}")
    for hh, names in sorted(groups.items(), key=lambda kv: -len(kv[1])):
        if len(groups) <= 4:
            print("   ", hh, ":", ",".join(sorted(names)))

print()
print("=" * 72)
print("C. event_log events across nodes (shared platform boot sequence?)")
print("=" * 72)
def events_of(n):
    el = evs[n]["event_log"]
    try:
        return json.loads(el)
    except Exception:
        return None
el0 = events_of("phala-212")
if el0:
    print(f"phala-212: {len(el0)} events")
    per_pos = []
    for i, e0 in enumerate(el0):
        same = 0
        for n in qwen[1:]:
            eln = events_of(n)
            if eln and i < len(eln) and json.dumps(eln[i], sort_keys=True) == json.dumps(e0, sort_keys=True):
                same += 1
        per_pos.append((i, same, e0.get("imr"), e0.get("event_type"), len(json.dumps(e0))))
    shared = sum(s for _, s, *_ in per_pos if s == len(qwen) - 1)
    shared_bytes = sum(sz for _, s, _, _, sz in per_pos if s == len(qwen) - 1)
    total = sum(sz for *_, sz in per_pos)
    print(f"events identical across all 6 qwen nodes: {shared}/{len(el0)}, bytes {shared_bytes}/{total} ({100*shared_bytes//total}%)")
    for i, s, imr, et, sz in per_pos:
        if s != len(qwen) - 1:
            print(f"  event[{i}] imr={imr} type={et} size={sz} shared_by={s}/5")
