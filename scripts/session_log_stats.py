#!/usr/bin/env python3
"""Analyze a live gateway attested-session log (JSONL) for storage pressure.

Answers, from a production log file alone:

* how big the real verifier evidence bundles are (per record, decoded);
* how many records each channel fingerprint has accumulated (a count far
  above the number of validity windows since startup means the dedup
  fingerprint is committing to per-round material — the #142 regression);
* append rate over time (bytes and records per hour);
* the largest records.

Usage: python3 scripts/session_log_stats.py /path/to/sessions.jsonl
"""

import base64
import json
import sys
from collections import Counter, defaultdict


def data_uri_bytes(uri: str) -> int:
    """Decoded byte length of a data: URI without decoding it."""
    if ";base64," in uri:
        b64 = uri.split(";base64,", 1)[1]
        return len(b64.rstrip("=")) * 3 // 4
    return len(uri)


def main(path: str) -> None:
    total_lines = 0
    total_bytes = 0
    per_fingerprint = Counter()
    per_hour_records = Counter()
    per_hour_bytes = Counter()
    evidence_sizes = []
    largest = []

    with open(path, "rb") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            total_lines += 1
            total_bytes += len(line)
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            fp = rec.get("fingerprint", "?")
            per_fingerprint[fp] += 1
            hour = rec.get("ts", 0) // 3600 * 3600
            per_hour_records[hour] += 1
            per_hour_bytes[hour] += len(line)
            try:
                doc = json.loads(base64.b64decode(rec["payload_b64"]))
            except Exception:
                continue
            ev = doc.get("evidence") or {}
            ev_bytes = data_uri_bytes(ev.get("data", "")) if ev.get("data") else 0
            evidence_sizes.append(ev_bytes)
            largest.append((len(line), ev_bytes, doc.get("established_at", 0), fp))

    print(f"records:            {total_lines}")
    print(f"file bytes:         {total_bytes / 2**20:.2f} MiB")

    if evidence_sizes:
        evidence_sizes.sort()
        n = len(evidence_sizes)
        print(
            "evidence bundle:    min {:.1f} KiB / median {:.1f} KiB / max {:.1f} KiB".format(
                evidence_sizes[0] / 1024,
                evidence_sizes[n // 2] / 1024,
                evidence_sizes[-1] / 1024,
            )
        )
        embedded = sum(evidence_sizes)
        print(
            f"evidence share:     {embedded / 2**20:.2f} MiB decoded "
            f"({100.0 * embedded / max(total_bytes, 1):.0f}% of file before double-base64)"
        )

    counts = sorted(per_fingerprint.values(), reverse=True)
    print(f"fingerprints:       {len(per_fingerprint)}")
    print(f"records/fingerprint: max {counts[0] if counts else 0}, "
          f"median {counts[len(counts)//2] if counts else 0}")
    worst = [fp for fp, c in per_fingerprint.most_common(5)]
    if worst and per_fingerprint[worst[0]] > 4:
        print("WARNING: fingerprints with suspiciously many records "
              "(dedup may be committing to per-round material):")
        for fp in worst:
            print(f"  {fp[:24]}…  {per_fingerprint[fp]} records")

    print("\nappends per hour (ts buckets):")
    for hour in sorted(per_hour_records):
        print(
            f"  {hour}: {per_hour_records[hour]:>6} records, "
            f"{per_hour_bytes[hour] / 2**20:>8.2f} MiB"
        )

    print("\nlargest records:")
    for size, ev_bytes, established, fp in sorted(largest, reverse=True)[:5]:
        print(
            f"  {size / 2**20:>7.2f} MiB (evidence {ev_bytes / 1024:.0f} KiB) "
            f"established {established} fp {fp[:16]}…"
        )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
