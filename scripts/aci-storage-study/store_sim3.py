#!/usr/bin/env python3
"""v3: merkle-ized field-CAS — chunk strings AND subtrees.

Rules (generic, byte-exact verified, fallback verbatim):
  strings: R0 data-URI decode, R1 embedded-JSON recurse (ascii|utf8 compact), R2 hex->bin,
           R3 b64->bin, R4 raw; threshold THRESH
  subtrees: R6 after processing children, if canonical bytes > THRESH -> chunk whole node
           canonical form follows context mode: outer doc = JCS; embedded = original-order compact
Roundtrip assertion on every doc.
"""
import json, base64, glob, os, hashlib, binascii

CORPUS = os.path.expanduser("~/workshop/aci-storage-study/corpus")
THRESH = 1024          # subtree chunking threshold
STR_THRESH = 1024      # string chunking threshold
HEXCHARS = set("0123456789abcdef")

def enc(o, mode):
    if mode == "jcs":
        return json.dumps(o, separators=(",", ":"), sort_keys=True, ensure_ascii=False)
    if mode == "compact_a":
        return json.dumps(o, separators=(",", ":"), sort_keys=False, ensure_ascii=True)
    return json.dumps(o, separators=(",", ":"), sort_keys=False, ensure_ascii=False)

class Store:
    def __init__(self):
        self.chunks = {}
        self.new_bytes = 0
        self.fallbacks = []

    def chunk(self, raw: bytes, tag: str):
        key = tag + ":" + hashlib.sha256(raw).hexdigest()
        if key not in self.chunks:
            self.chunks[key] = raw
            self.new_bytes += len(raw)
        return {"__c__": key}

    def process_str(self, s: str, mode):
        if len(s) <= STR_THRESH:
            return s
        st = s.strip()
        if st.startswith(("{", "[")):
            try:
                inner = json.loads(s)
                if enc(inner, "compact_a") == s:
                    return {"__js_a__": self.process(inner, "compact_a")}
                if enc(inner, "compact") == s:
                    return {"__js__": self.process(inner, "compact")}
                self.fallbacks.append(("json-str", len(s)))
            except Exception:
                pass
        if len(s) % 2 == 0 and all(c in HEXCHARS for c in s):
            return self.chunk(binascii.unhexlify(s), "hex")
        try:
            raw = base64.b64decode(s, validate=True)
            if base64.b64encode(raw).decode() == s:
                return self.chunk(raw, "b64")
        except Exception:
            pass
        return self.chunk(s.encode(), "raw")

    def process(self, node, mode, key_hint=None):
        if isinstance(node, dict):
            out = {k: self.process(v, mode, k) for k, v in node.items()}
        elif isinstance(node, list):
            out = [self.process(v, mode) for v in node]
        elif isinstance(node, str):
            if key_hint == "data" and node.startswith("data:") and ";base64," in node:
                head, b64 = node.split(";base64,", 1)
                ctype = head[5:]
                try:
                    raw = base64.b64decode(b64)
                except Exception:
                    return self.chunk(node.encode(), "raw")
                try:
                    inner = json.loads(raw)
                    if inner and enc(inner, "compact_a").encode() == raw:
                        return {"__du_a__": ctype, "doc": self.process(inner, "compact_a")}
                    if inner and enc(inner, "compact").encode() == raw:
                        return {"__du__": ctype, "doc": self.process(inner, "compact")}
                    self.fallbacks.append(("datauri-json", len(raw)))
                except Exception:
                    pass
                return {"__du_bin__": ctype, "ref": self.chunk(raw, "bin")}
            return self.process_str(node, mode)
        else:
            return node
        # R6: subtree chunking. Ref structures are serialized in INSERTION
        # ORDER (never sorted): nested structures may carry order-sensitive
        # embedded-JSON contexts; a jcs parent re-sorts at encode time anyway.
        canon = json.dumps(out, separators=(",", ":"), sort_keys=False,
                           ensure_ascii=(mode == "compact_a")).encode()
        if len(canon) > THRESH:
            return self.chunk(canon, "sub:" + mode)
        return out

    def rebuild(self, node):
        if isinstance(node, dict):
            if "__c__" in node:
                key = node["__c__"]
                raw = self.chunks[key]
                if key.startswith("sub:"): return self.rebuild(json.loads(raw))
                tag = key.split(":", 1)[0]
                if tag == "hex": return binascii.hexlify(raw).decode()
                if tag == "b64": return base64.b64encode(raw).decode()
                return raw.decode()
            if "__js__" in node: return enc(self.rebuild(node["__js__"]), "compact")
            if "__js_a__" in node: return enc(self.rebuild(node["__js_a__"]), "compact_a")
            if "__du__" in node:
                return "data:" + node["__du__"] + ";base64," + base64.b64encode(enc(self.rebuild(node["doc"]), "compact").encode()).decode()
            if "__du_a__" in node:
                return "data:" + node["__du_a__"] + ";base64," + base64.b64encode(enc(self.rebuild(node["doc"]), "compact_a").encode()).decode()
            if "__du_bin__" in node:
                return "data:" + node["__du_bin__"] + ";base64," + base64.b64encode(self.chunks[node["ref"]["__c__"]]).decode()
            return {k: self.rebuild(v) for k, v in node.items()}
        if isinstance(node, list):
            return [self.rebuild(v) for v in node]
        return node

def run(files, label):
    docs = []
    for p in sorted(files):
        d = json.load(open(p))
        if "evidence" in d: docs.append(d)
    st = Store()
    l0, cas_total, marg = 0, 0, []
    ok = 0
    for i, d in enumerate(docs):
        raw = enc(d, "jcs").encode()
        before = st.new_bytes
        skel = st.process(d, "jcs")
        cost = len(enc(skel, "jcs").encode()) + st.new_bytes - before
        l0 += len(raw); cas_total += cost
        if i > 0: marg.append(cost)
        # Hard invariant: a roundtrip mismatch fails the run. Silently
        # counting a lower match rate would let corrupted rebuilds
        # contribute "savings" to the report.
        assert (
            enc(st.rebuild(skel), "jcs").encode() == raw
        ), f"{label}: rebuilt bytes diverge from the input document"
        ok += 1
    n = len(docs)
    m = sum(marg)/len(marg) if marg else cas_total/n
    print(f"--- {label}: {n} sessions")
    print(f"  L0 {l0/n:>10,.0f}/sess | CAS marginal {m:>10,.0f}/sess | ratio {l0/n/m:6.1f}x | roundtrip {ok}/{n} | fallbacks {len(st.fallbacks)}")
    return {"l0": l0/n, "marg": m}

r = {}
r["phala"] = run(glob.glob(f"{CORPUS}/series-phala225/*.json"), "phala-direct series x11")
r["near"] = run(glob.glob(f"{CORPUS}/series-near/*.json"), "near-ai series x10")
r["cross"] = run(glob.glob(f"{CORPUS}/cross/phala-*.json"), "cross-upstream phala x13 shared store")
r["chutes"] = run(glob.glob(f"{CORPUS}/cross/chutes-*.json"), "chutes-embed x1")
r["tinfoil"] = run(glob.glob(f"{CORPUS}/cross/tinfoil.json"), "tinfoil x1")

print()
print("=" * 72)
print("1-DAY RETENTION, production churn 8,489 sessions/day")
print("=" * 72)
RATE = {"phala": 217.2, "near": 18.6, "chutes": 116.9, "tinfoil": 1.0}
L0S = {"phala": 268_957, "near": 116_054, "chutes": 1_600, "tinfoil": 2_717}
CASM = {"phala": r["phala"]["marg"], "near": r["near"]["marg"], "chutes": 1_600, "tinfoil": 2_717}
l0_day = sum(RATE[k]*24*L0S[k] for k in RATE)
cas_day = sum(RATE[k]*24*CASM[k] for k in RATE)
for k in RATE:
    print(f"{k:10s} {RATE[k]*24:8,.0f} sess/d   L0 {RATE[k]*24*L0S[k]/1e6:9.1f} MB   CAS {RATE[k]*24*CASM[k]/1e6:7.1f} MB")
print(f"TOTAL    {sum(RATE.values())*24:8,.0f}          L0 {l0_day/1e9:8.3f} GB   CAS {cas_day/1e6:7.1f} MB")
print(f"steady-state @1d: L0 {l0_day/1e9:.2f} GB  vs  CAS {cas_day/1e6:.0f} MB (+~1MB low-freq chunks)   ratio {l0_day/cas_day:.1f}x")
