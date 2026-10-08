#!/usr/bin/env python3
"""near-ai churn series: instance rotation pattern + per-field delta check."""
import json, base64, glob, os, hashlib, subprocess, tempfile

CORPUS = os.path.expanduser("~/workshop/aci-storage-study/corpus")

def h(x): return hashlib.sha256(x.encode() if isinstance(x, str) else x).hexdigest()[:12]

def pem_blocks(pem):
    blocks, cur = [], []
    for line in pem.splitlines():
        cur.append(line)
        if "END CERTIFICATE" in line:
            blocks.append("\n".join(cur)); cur = []
    return blocks

rows = []
for p in sorted(glob.glob(f"{CORPUS}/series-near/*.json")):
    doc = json.load(open(p))
    ga = json.loads(base64.b64decode(doc["evidence"]["data"].split(";base64,")[1]))["gateway_attestation"]
    rows.append({
        "file": os.path.basename(p)[:2],
        "est": doc["established_at"],
        "instance_id": ga["info"]["instance_id"],
        "app_cert": ga["info"]["app_cert"],
        "event_log": ga["event_log"],
        "intel_quote": ga["intel_quote"],
        "rtmr3": ga["info"]["tcb_info"]["rtmr3"],
        "mr_aggregated": ga["info"]["mr_aggregated"],
        "vpc_hostname": ga.get("vpc", {}).get("vpc_hostname"),
    })

print(f"{len(rows)} rounds, span {rows[-1]['est'] - rows[0]['est']}s\n")

print("== instance rotation ==")
inst = [r["instance_id"] for r in rows]
print("unique instance_id:", len(set(inst)), "of", len(inst))
for r in rows:
    print(f"  [{r['file']}] est={r['est']} instance={r['instance_id'][:12]} rtmr3={r['rtmr3'][:12]} vpc={r['vpc_hostname']}")

print()
print("== per-field uniqueness across rounds ==")
for f in ("app_cert", "event_log", "intel_quote", "rtmr3", "mr_aggregated"):
    vals = [r[f] for r in rows]
    uniq = len({h(v) for v in vals})
    print(f"  {f:16s} uniq={uniq}/{len(rows)} size={len(vals[0])}")

print()
print("== app_cert chain positions across rounds ==")
chains = [pem_blocks(r["app_cert"]) for r in rows]
for pos in range(max(len(c) for c in chains)):
    uniq = len({h(c[pos]) for c in chains if pos < len(c)})
    print(f"  cert[{pos}] uniq={uniq}/{len(rows)}")

print()
print("== event_log event-level delta across consecutive rounds ==")
els = [json.loads(r["event_log"]) for r in rows]
for i in range(1, len(els)):
    a, b = els[i - 1], els[i]
    diffs = [j for j, (x, y) in enumerate(zip(a, b)) if json.dumps(x, sort_keys=True) != json.dumps(y, sort_keys=True)]
    print(f"  round {i-1:02d}->{i:02d}: events {len(a)}->{len(b)}, changed idx={diffs}")

print()
print("== zstd --patch-from per field (consecutive rounds) ==")
def patch_size(base: bytes, target: bytes) -> int:
    with tempfile.NamedTemporaryFile(delete=False) as fb, tempfile.NamedTemporaryFile(delete=False) as ft:
        fb.write(base); ft.write(target)
        fb.close(); ft.close()
        out = subprocess.run(["zstd", "-19", "--patch-from=" + fb.name, "-c", ft.name],
                             capture_output=True).stdout
        os.unlink(fb.name); os.unlink(ft.name)
        return len(out)

for f in ("app_cert", "event_log", "intel_quote"):
    sizes = [patch_size(rows[i - 1][f].encode(), rows[i][f].encode()) for i in range(1, len(rows))]
    print(f"  {f:16s} patches: {sizes}  avg={sum(sizes)//len(sizes)}")

# whole evidence patch (reference, measured before = 1151B)
evs = []
for p in sorted(glob.glob(f"{CORPUS}/series-near/*.json")):
    doc = json.load(open(p))
    evs.append(base64.b64decode(doc["evidence"]["data"].split(";base64,")[1]))
sizes = [patch_size(evs[i - 1], evs[i]) for i in range(1, len(evs))]
print(f"  whole-evidence   patches: {sizes}  avg={sum(sizes)//len(sizes)}")
