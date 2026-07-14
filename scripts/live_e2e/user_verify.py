#!/usr/bin/env python3
"""Fetch ACI artifacts for a received response and run `aci audit` on them."""
from __future__ import annotations

import argparse
import base64
import json
import secrets
import subprocess
import sys
import tempfile
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from live_e2e.common import ROOT, request_json, write_bytes  # noqa: E402


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Verify an aggregator report and receipt for a received response."
    )
    parser.add_argument("--base-url")
    parser.add_argument("--chat-id", help="receipt id or response chat id")
    parser.add_argument("--bearer-token")
    parser.add_argument("--report-file", type=Path)
    parser.add_argument("--receipt-file", type=Path)
    parser.add_argument("--session-file", type=Path)
    parser.add_argument("--nonce")
    parser.add_argument("--request-body", type=Path)
    parser.add_argument("--response-body", type=Path)
    parser.add_argument("--skip-expiry", action="store_true")
    args = parser.parse_args()

    if args.report_file and args.receipt_file:
        raise SystemExit(
            run_audit(args, args.report_file, args.receipt_file, args.session_file, args.nonce)
        )

    if not args.base_url or not args.chat_id:
        raise SystemExit(
            "either --report-file/--receipt-file or --base-url/--chat-id is required"
        )
    base_url = args.base_url.rstrip("/")
    nonce = args.nonce or secrets.token_hex(16)
    headers = {}
    if args.bearer_token:
        headers["Authorization"] = f"Bearer {args.bearer_token}"

    with tempfile.TemporaryDirectory(prefix="private-ai-gateway-user-verify-") as tmp:
        tmp_dir = Path(tmp)
        report_status, _, report_body, _ = request_json(
            "GET", f"{base_url}/v1/aci/attestation?nonce={nonce}", timeout=120
        )
        if report_status != 200:
            raise SystemExit(f"report fetch failed with HTTP {report_status}")
        receipt_status, _, receipt_body, receipt_json = request_json(
            "GET", f"{base_url}/v1/aci/receipts/{args.chat_id}", headers=headers, timeout=120
        )
        if receipt_status != 200:
            raise SystemExit(f"receipt fetch failed with HTTP {receipt_status}")
        report_path = tmp_dir / "report.json"
        receipt_path = tmp_dir / "receipt.json"
        write_bytes(report_path, report_body)
        write_bytes(receipt_path, receipt_body)
        session_path = fetch_cited_session(base_url, receipt_json, tmp_dir)
        raise SystemExit(run_audit(args, report_path, receipt_path, session_path, nonce))


def fetch_cited_session(
    base_url: str, receipt_json: object, tmp_dir: Path
) -> Path | None:
    """Fetch the session the receipt's upstream.verified event cites, if any."""
    try:
        payload = json.loads(base64.b64decode(receipt_json["payload_b64"]))
        session_id = next(
            event["session_id"]
            for event in payload["event_log"]
            if event.get("type") == "upstream.verified" and "session_id" in event
        )
    except (KeyError, TypeError, ValueError, StopIteration):
        return None
    status, _, body, _ = request_json(
        "GET", f"{base_url}/v1/aci/sessions/{session_id}", timeout=120
    )
    if status != 200:
        return None
    session_path = tmp_dir / "session.json"
    write_bytes(session_path, body)
    return session_path


def run_audit(
    args: argparse.Namespace,
    report_path: Path,
    receipt_path: Path,
    session_path: Path | None,
    nonce: str | None,
) -> int:
    cmd = [
        "cargo", "run", "--quiet", "--bin", "aci", "--",
        "audit", "--report", str(report_path), "--receipt", str(receipt_path), "--json",
    ]
    if nonce:
        cmd.extend(["--nonce", nonce])
    if session_path:
        cmd.extend(["--session", str(session_path)])
    if args.request_body:
        cmd.extend(["--request-body", str(args.request_body)])
    if args.response_body:
        cmd.extend(["--response-body", str(args.response_body)])
    if args.skip_expiry:
        cmd.append("--skip-expiry")
    # `aci audit --json` prints the transcript on stdout and exits non-zero
    # when the verdict is NOT VERIFIED; pass both through.
    result = subprocess.run(cmd, cwd=ROOT, check=False)
    return result.returncode


if __name__ == "__main__":
    main()
