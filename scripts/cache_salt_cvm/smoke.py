#!/usr/bin/env python3
"""Per-user cache-salt smoke test against a disposable gateway deployment.

Sends one long, unique prompt per model through the gateway as two test
tenants and reads `usage.prompt_tokens_details.cached_tokens` to tell whether
each request reused a prefix-cache namespace.

Usage: smoke.py BASE_URL MODEL [MODEL ...]
       smoke.py BASE_URL --replay RESULTS_JSON

Exit status: 0 pass, 1 an isolation check hit another namespace, 2 a warm-up
never produced a hit (inconclusive; rerun), 3 a request error (inconclusive).
A leak outranks request errors elsewhere in the run.

`--replay` repeats tenant A's no-salt prompt from an earlier run, for example
after a restart: a hit shows the gateway derived the same salt again, so the
dstack KMS key is stable.
Environment: TEST_USER_A_KEY, TEST_USER_B_KEY (bearer keys the mock control
plane maps to userId 1001 and 1002).
"""

import json
import os
import secrets
import sys
import time
import urllib.error
import urllib.request

HIT_RETRIES = 12
TRIALS = 3


def prompt(nonce: str) -> str:
    facts = " ".join(
        f"Fact {i}: the quick brown fox number {i} jumps over lazy dog {i * 7}."
        for i in range(400)
    )
    return f"[{nonce}] {facts}"


def call(base, model, key, nonce, caller_salt=None, stream=False):
    body = {
        "model": model,
        "max_tokens": 8,
        "messages": [
            {"role": "system", "content": prompt(nonce)},
            {"role": "user", "content": "Reply OK."},
        ],
    }
    if caller_salt is not None:
        body["cache_salt"] = caller_salt
    if stream:
        body["stream"] = True
        body["stream_options"] = {"include_usage": True}
    headers = {"content-type": "application/json"}
    if key:
        headers["authorization"] = f"Bearer {key}"
    req = urllib.request.Request(
        f"{base}/v1/chat/completions", json.dumps(body).encode(), headers
    )
    try:
        with urllib.request.urlopen(req, timeout=180) as resp:
            raw = resp.read().decode()
            if resp.status != 200:
                return {"status": resp.status, "error": raw[:300]}
    except urllib.error.HTTPError as err:
        return {"status": err.code, "error": err.read().decode()[:300]}
    except (urllib.error.URLError, TimeoutError) as err:
        return {"status": 0, "error": str(err)[:300]}
    usage = None
    if stream:
        for line in raw.splitlines():
            if line.startswith("data: ") and line != "data: [DONE]":
                chunk = json.loads(line[6:])
                usage = chunk.get("usage") or usage
    else:
        usage = json.loads(raw).get("usage")
    details = (usage or {}).get("prompt_tokens_details") or {}
    return {
        "status": 200,
        "prompt_tokens": (usage or {}).get("prompt_tokens"),
        "cached": details.get("cached_tokens") or 0,
    }


def is_hit(result):
    """A hit reuses most of the prompt. A few cached tokens are the shared chat
    template the same tenant sent earlier, not this prompt."""
    return result.get("cached", 0) * 2 >= (result.get("prompt_tokens") or 1)


def expect_hit(fn):
    """Providers spread requests over replicas, and a cold replica misses:
    retry until one request lands on the warm replica."""
    for attempt in range(1, HIT_RETRIES + 1):
        result = fn()
        if is_hit(result) or result["status"] != 200:
            break
        time.sleep(2)
    return {**result, "attempts": attempt}


def run_model(base, model, user_a, user_b, stream):
    """Each trial uses a fresh prompt. Another tenant's request for a prompt
    must be that tenant's first, or it would hit its own namespace, so the
    isolation checks send once per trial and rely on several trials instead."""
    steps = []
    nonces = []

    def step(trial, name, expect, result):
        hit = is_hit(result)
        ok = result["status"] == 200 and (hit if expect == "hit" else not hit)
        steps.append(
            {"trial": trial, "step": name, "expect": expect, "ok": ok, **result}
        )

    for trial in range(TRIALS):
        nonce = f"{model}-{'s' if stream else 'b'}-{secrets.token_hex(8)}"
        salt = f"shared-caller-salt-{secrets.token_hex(8)}"
        nonces.append(nonce)

        def send(key, caller_salt=None, nonce=nonce):
            return call(base, model, key, nonce, caller_salt, stream=stream)

        step(trial, "A first request", "miss", send(user_a))
        step(trial, "A repeat", "hit", expect_hit(lambda: send(user_a)))
        step(trial, "B same prompt", "miss", send(user_b))
        step(trial, "A + caller salt", "miss", send(user_a, salt))
        step(
            trial,
            "A + caller salt repeat",
            "hit",
            expect_hit(lambda salt=salt: send(user_a, salt)),
        )
        step(trial, "B + same caller salt", "miss", send(user_b, salt))
        step(trial, "anonymous same prompt", "miss", send(None))
    return {"model": model, "stream": stream, "nonces": nonces, "steps": steps}


def replay(base, path, user_a):
    """Send each of tenant A's earlier prompts exactly once. Only a first-try
    hit counts: a retry could hit an entry the first try cached under a newly
    derived salt. One hit proves the gateway-wide KMS key survived; a provider
    that spreads requests may miss on a cold replica, so misses alone are
    inconclusive."""
    hits = 0
    request_error = False
    with open(path) as results:
        runs = json.load(results)
    for run in runs:
        if run["stream"]:
            continue
        for nonce in run["nonces"]:
            result = call(base, run["model"], user_a, nonce)
            request_error |= result["status"] != 200
            hit = result["status"] == 200 and is_hit(result)
            hits += hit
            print(
                f"{'hit ' if hit else 'miss'} replay {run['model']} {nonce} cached={result.get('cached')}/{result.get('prompt_tokens')} status={result['status']}",
                file=sys.stderr,
            )
    print(
        "INCONCLUSIVE (request error)"
        if request_error
        else "PASS"
        if hits
        else "INCONCLUSIVE",
        file=sys.stderr,
    )
    sys.exit(3 if request_error else 0 if hits else 2)


def main():
    base = sys.argv[1].rstrip("/")
    user_a = os.environ["TEST_USER_A_KEY"]
    if sys.argv[2] == "--replay":
        replay(base, sys.argv[3], user_a)
    models = sys.argv[2:]
    user_b = os.environ["TEST_USER_B_KEY"]
    results = [
        run_model(base, model, user_a, user_b, stream)
        for model in models
        for stream in (False, True)
    ]
    json.dump(results, sys.stdout, indent=2)
    print()
    # Only a successful isolation request reporting a hit proves a leak, and a
    # leak outranks any request error elsewhere. Request errors and warm-ups
    # without a hit make the run inconclusive.
    failed = inconclusive = request_error = False
    for r in results:
        bad = [s for s in r["steps"] if not s["ok"]]
        leaked = any(
            s["status"] == 200 and s["expect"] == "miss" and is_hit(s)
            for s in r["steps"]
        )
        errors = any(s["status"] != 200 for s in r["steps"])
        verdict = (
            "FAIL"
            if leaked
            else "INCONCLUSIVE (request error)"
            if errors
            else "INCONCLUSIVE"
            if bad
            else "PASS"
        )
        failed |= leaked
        request_error |= errors
        inconclusive |= verdict == "INCONCLUSIVE"
        print(f"{verdict} {r['model']} stream={r['stream']}", file=sys.stderr)
        for s in r["steps"]:
            mark = "ok " if s["ok"] else "BAD"
            print(
                f"  {mark} t{s['trial']} {s['step']:<24} expect={s['expect']:<4} cached={s.get('cached')}/{s.get('prompt_tokens')} attempts={s.get('attempts', 1)} status={s['status']}",
                file=sys.stderr,
            )
    sys.exit(1 if failed else 3 if request_error else 2 if inconclusive else 0)


if __name__ == "__main__":
    main()
