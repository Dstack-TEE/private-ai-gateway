#!/usr/bin/env python3
"""ACI session storage measurement: what changes per session, and what each
storage layout costs. All numbers from production api.redpill.ai corpus."""
import json, base64, hashlib, glob, io, tarfile, os, sys, zlib

CORPUS = os.path.expanduser("~/workshop/aci-storage-study/corpus")

def sha(b): return hashlib.sha256(b).hexdigest()

def load_doc(p): return json.load(open(p, "rb"))

def evidence_bytes(doc):
    uri = doc["evidence"]["data"]
    return base64.b64decode(uri.split(";base64,")[1])

def doc_served_bytes(doc):
    return len(json.dumps(doc, separators=(",", ":"), sort_keys=True).encode())

# ---------- 1. production shape from the list ----------
lst = json.load(open(f"{CORPUS}/sessions-list.json"))["sessions"]
from collections import defaultdict, Counter
by_up = defaultdict(list)
for s in lst: by_up[s["upstream_name"]].append(s)
span = max(s["established_at"] for s in lst) - min(s["established_at"] for s in lst)
hours = span / 3600
verifier_of = {n: xs[0]["verifier_id"] for n, xs in by_up.items()}
by_ver = defaultdict(list)
for n, xs in by_up.items(): by_ver[verifier_of[n]].extend(xs)

print("=" * 70)
print("1. PRODUCTION SHAPE (snapshot of /v1/aci/sessions)")
print("=" * 70)
print(f"upstreams: {len(by_up)}   live sessions: {len(lst)}   span: {span}s ({hours:.2f}h)")
print(f"list response size (abbreviated, no evidence.data): {os.path.getsize(CORPUS + chr(47) + chr(115) + chr(101) + chr(115) + chr(115) + chr(105) + chr(111) + chr(110) + chr(115) + chr(45) + chr(108) + chr(105) + chr(115) + chr(116) + chr(46) + chr(106) + chr(115) + chr(111) + chr(110)):,} B")
for v, xs in sorted(by_ver.items(), key=lambda kv: -len(kv[1])):
    ups = [n for n in by_up if verifier_of[n] == v]
    rate = len(xs) / hours
    print(f"  {v:42s} upstreams={len(ups):3d} sessions={len(xs):4d} rate={rate:7.1f}/h  per-upstream={rate/len(ups):5.2f}/h")

# ---------- 2. time series: what changes per round ----------
print()
print("=" * 70)
print("2. PER-ROUND INCREMENT (11 consecutive phala-225 sessions)")
print("=" * 70)
series = sorted(glob.glob(f"{CORPUS}/series-phala225/*.json"),
                key=lambda p: load_doc(p)["established_at"])
docs = [load_doc(p) for p in series]
print(f"series length: {len(docs)}  served bytes each: {[doc_served_bytes(d) for d in docs][:3]}...")

# doc-level diff (excluding evidence.data/timestamps)
base = docs[0]
def flat(d, pfx=""):
    out = {}
    for k, v in d.items():
        key = f"{pfx}.{k}" if pfx else k
        if isinstance(v, dict): out.update(flat(v, key))
        else: out[key] = v
    return out
changing = Counter()
for d in docs[1:]:
    fa, fb = flat(base), flat(d)
    for k in set(fa) | set(fb):
        if fa.get(k) != fb.get(k): changing[k] += 1
print("doc-level fields that ever change across rounds:")
for k, c in changing.most_common():
    print(f"  {k:24s} changed in {c}/{len(docs)-1} rounds")

# evidence-level: static vs varying across the whole series
evs = [json.loads(evidence_bytes(d)) for d in docs]
print()
print("evidence leaf stability across 11 rounds (top level + all_attestations[0]):")
first = evs[0]
for scope, get in [("top", lambda e: e), ("aa[0]", lambda e: e["all_attestations"][0])]:
    for k in sorted(get(first)):
        vals = [json.dumps(get(e).get(k), sort_keys=True) for e in evs]
        stable = len(set(vals)) == 1
        print(f"  {scope}.{k:22s} size={len(vals[0]):7d} stable={stable}")

# ---------- 3. inside nvidia_payload (schema-aware potential) ----------
print()
print("=" * 70)
print("3. INSIDE nvidia_payload (12KB, changes every round — but what inside?)")
print("=" * 70)
nvs = [json.loads(e["nvidia_payload"]) for e in evs]
nv0 = nvs[0]
def walk(d, pfx=""):
    out = {}
    if isinstance(d, dict):
        for k, v in d.items(): out.update(walk(v, f"{pfx}.{k}" if pfx else k))
    elif isinstance(d, list):
        for i, v in enumerate(d): out.update(walk(v, f"{pfx}[{i}]"))
    else: out[pfx] = d
    return out
wnv = [walk(n) for n in nvs]
for k in sorted(wnv[0]):
    vals = [json.dumps(w.get(k)) for w in wnv]
    stable = len(set(vals)) == 1
    if len(vals[0]) > 100 or not stable:
        print(f"  {k:44s} size={len(vals[0]):7d} stable={stable}")
