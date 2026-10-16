#!/usr/bin/env python3
"""Cross-upstream per-field uniqueness: which evidence fields are shared across
phala upstreams (same deployment artifacts) vs per-node vs per-instance."""
import json, base64, glob, os, hashlib
from collections import defaultdict

CORPUS = os.path.expanduser("~/workshop/aci-storage-study/corpus")

def h(x):
    return hashlib.sha256(json.dumps(x, sort_keys=True).encode()).hexdigest()[:12]

rows = []
for p in sorted(glob.glob(f"{CORPUS}/cross/phala-*.json")):
    name = os.path.basename(p)[:-5]
    doc = json.load(open(p))
    ev = json.loads(base64.b64decode(doc["evidence"]["data"].split(";base64,")[1]))
    nv = json.loads(ev["nvidia_payload"]) if isinstance(ev.get("nvidia_payload"), str) else {}
    info = ev.get("info", {})
    tcb = info.get("tcb_info", {})
    aa = ev.get("all_attestations", [])
    rows.append({
        "name": name,
        "n_gpu_attestations": len(aa),
        "endpoint": doc.get("endpoint"),
        "signing_address": ev.get("signing_address"),
        "tls_cert_fingerprint": ev.get("tls_cert_fingerprint"),
        "info.app_cert": info.get("app_cert"),
        "info.app_id": info.get("app_id"),
        "info.instance_id": info.get("instance_id"),
        "info.device_id": info.get("device_id"),
        "info.compose_hash": info.get("compose_hash"),
        "info.os_image_hash": info.get("os_image_hash"),
        "info.mr_aggregated": info.get("mr_aggregated"),
        "info.key_provider_info": info.get("key_provider_info"),
        "tcb.app_compose": tcb.get("app_compose"),
        "tcb.mrtd": tcb.get("mrtd"),
        "tcb.rtmr0": tcb.get("rtmr0"), "tcb.rtmr1": tcb.get("rtmr1"),
        "tcb.rtmr2": tcb.get("rtmr2"), "tcb.rtmr3": tcb.get("rtmr3"),
        "tcb.event_log": tcb.get("event_log"),
        "event_log(top)": ev.get("event_log"),
        "vm_config(top)": ev.get("vm_config"),
        "info.vm_config": info.get("vm_config"),
        "nvidia.certificate": (nv.get("evidence_list") or [{}])[0].get("certificate"),
        "nvidia.arch": (nv.get("evidence_list") or [{}])[0].get("arch"),
    })

fields = [k for k in rows[0] if k not in ("name", "endpoint")]
print(f"{len(rows)} phala upstreams\n")
print("field".ljust(26), "uniq".rjust(4), "sizes".rjust(22), "class")
for f in fields:
    vals = defaultdict(list)
    for r in rows:
        vals[h(r[f])].append(r["name"])
    sizes = sorted({len(json.dumps(r[f])) for r in rows})[:4]
    marker = {1: "SHARED-ALL", len(rows): "PER-NODE"}.get(len(vals), f"{len(vals)} GROUPS")
    print(f.ljust(26), str(len(vals)).rjust(4), str(sizes).rjust(22), marker)
    if 1 < len(vals) < len(rows):
        for hh, names in sorted(vals.items(), key=lambda kv: -len(kv[1])):
            grp = ",".join(sorted(n.replace("phala-", "") for n in names))
            print("   ", hh, ":", grp)
print()
for r in rows:
    print(r["name"].ljust(12), "gpus=" + str(r["n_gpu_attestations"]),
          "app_id=" + str(r["info.app_id"])[:12],
          "instance=" + str(r["info.instance_id"])[:12],
          "endpoint=" + str(r["endpoint"]))
