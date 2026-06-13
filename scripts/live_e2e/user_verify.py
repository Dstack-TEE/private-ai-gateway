#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import secrets
import sys
import tempfile
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from live_e2e.common import request_json, run_cmd_json, write_bytes  # noqa: E402


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Verify an aggregator report and receipt for a received response."
    )
    parser.add_argument("--base-url")
    parser.add_argument("--chat-id")
    parser.add_argument("--bearer-token")
    parser.add_argument("--report-file", type=Path)
    parser.add_argument("--receipt-file", type=Path)
    parser.add_argument("--nonce")
    parser.add_argument("--request-body", type=Path)
    parser.add_argument("--response-body", type=Path)
    parser.add_argument("--skip-freshness", action="store_true")
    args = parser.parse_args()

    if args.report_file and args.receipt_file:
        nonce = args.nonce
        report_path = args.report_file
        receipt_path = args.receipt_file
        print(json.dumps(run_verifier(args, report_path, receipt_path, nonce), indent=2))
        return

    if not args.base_url or not args.chat_id:
        raise SystemExit(
            "either --report-file/--receipt-file or --base-url/--chat-id is required"
        )
    nonce = args.nonce or secrets.token_hex(16)
    headers = {}
    if args.bearer_token:
        headers["Authorization"] = f"Bearer {args.bearer_token}"

    with tempfile.TemporaryDirectory(prefix="private-ai-gateway-user-verify-") as tmp:
        tmp_dir = Path(tmp)
        report_status, _, report_body, _ = request_json(
            "GET",
            f"{args.base_url.rstrip('/')}/v1/attestation/report?nonce={nonce}",
            timeout=120,
        )
        if report_status != 200:
            raise SystemExit(f"report fetch failed with HTTP {report_status}")
        receipt_status, _, receipt_body, _ = request_json(
            "GET",
            f"{args.base_url.rstrip('/')}/v1/signature/{args.chat_id}",
            headers=headers,
            timeout=120,
        )
        if receipt_status != 200:
            raise SystemExit(f"receipt fetch failed with HTTP {receipt_status}")
        report_path = tmp_dir / "report.json"
        receipt_path = tmp_dir / "receipt.json"
        write_bytes(report_path, report_body)
        write_bytes(receipt_path, receipt_body)
        print(json.dumps(run_verifier(args, report_path, receipt_path, nonce), indent=2))


def run_verifier(
    args: argparse.Namespace,
    report_path: Path,
    receipt_path: Path,
    nonce: str | None,
) -> dict[str, object]:
    cmd = [
        "cargo",
        "run",
        "--quiet",
        "--example",
        "verify_aci_artifacts",
        "--",
        "--report",
        str(report_path),
        "--receipt",
        str(receipt_path),
    ]
    if nonce:
        cmd.extend(["--nonce", nonce])
    if args.request_body:
        cmd.extend(["--request-body", str(args.request_body)])
    if args.response_body:
        cmd.extend(["--response-body", str(args.response_body)])
    if args.skip_freshness:
        cmd.append("--skip-freshness")
    return run_cmd_json(cmd, timeout=240)


if __name__ == "__main__":
    main()
