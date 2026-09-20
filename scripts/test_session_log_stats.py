#!/usr/bin/env python3
"""Regression tests for session_log_stats.analyze (inline + externalized).

Run: python3 scripts/test_session_log_stats.py
"""

import base64
import json
import os
import tempfile

from session_log_stats import analyze


def b64(data: bytes) -> str:
    return base64.b64encode(data).decode()


def session_line(ts: int, fp: str, doc: dict, prefix: str | None = None) -> str:
    rec = {
        "seq": 0,
        "ts": ts,
        "type": "session",
        "fingerprint": fp,
        "retention_until": ts + 3600,
        "payload_b64": b64(json.dumps(doc).encode()),
    }
    if prefix is not None:
        rec["evidence_data_prefix"] = prefix
    return json.dumps(rec)


def main() -> None:
    bundle = b"x" * (1024 * 1024)  # 1 MiB shared bundle
    hour = 3600
    doc_inline = {
        "established_at": 1000,
        "evidence": {
            "digest": "sha256:aa",
            "data": f"data:application/json;base64,{b64(b'inline-bytes')}",
        },
    }
    doc_stripped = {"established_at": 1100, "evidence": {"digest": "sha256:aa"}}
    lines = [
        json.dumps({
            "seq": 0, "ts": hour, "type": "evidence",
            "digest": "sha256:aa", "retention_until": hour + 3600,
            "payload_b64": b64(bundle),
        }),
        session_line(hour, "fp-stripped", doc_stripped, "data:application/json;base64,"),
        session_line(hour, "fp-inline", doc_inline),
    ]
    path = os.path.join(tempfile.mkdtemp(), "sessions.jsonl")
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")
    # The analyzer counts stripped line content, not newline bytes.
    total = sum(len(line) for line in lines)

    stats = analyze(path)

    # Hourly accounting covers ALL record types (the bug: evidence records
    # were skipped, underreporting exactly where the bytes live).
    assert stats["per_hour_records"][hour] == 3, stats["per_hour_records"]
    assert stats["per_hour_bytes"][hour] == total, (
        stats["per_hour_bytes"][hour],
        total,
    )
    # Bundle size read from the shared evidence record AND from inline data.
    assert 1024 * 1024 in stats["evidence_sizes"], stats["evidence_sizes"]
    assert len(b"inline-bytes") in stats["evidence_sizes"], stats["evidence_sizes"]
    # Fingerprints count session records only.
    assert dict(stats["per_fingerprint"]) == {"fp-stripped": 1, "fp-inline": 1}
    assert stats["records"] == 3 and stats["bytes"] == total
    print("ok: session_log_stats analyze handles inline + externalized records")


if __name__ == "__main__":
    main()
