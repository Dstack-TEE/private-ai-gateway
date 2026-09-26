from __future__ import annotations

import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts.live_e2e.common import audit_aci_artifacts


REQUIRED = (
    "receipt-1",
    "receipt-2",
    "receipt-3",
    "receipt-4",
    "upstream-1",
    "upstream-2",
)


class AuditTests(unittest.TestCase):
    def audit(self, stdout: bytes, stderr: bytes = b"") -> dict:
        result = subprocess.CompletedProcess(["pap", "audit"], 1, stdout, stderr)
        with patch("scripts.live_e2e.common.run_cmd", return_value=result):
            return audit_aci_artifacts(
                report=Path("report.json"),
                receipt=Path("receipt.json"),
                session=Path("session.json"),
                nonce="a" * 64,
                request_body=Path("request.json"),
                response_body=Path("response.json"),
            )

    def test_offline_partial_requires_all_receipt_and_session_checks(self) -> None:
        transcript = {
            "verdict": {"verified": False, "failed": 0},
            "checks": [{"id": "id-1", "status": "skip"}]
            + [{"id": check, "status": "pass"} for check in REQUIRED],
        }
        self.assertEqual(self.audit(json.dumps(transcript).encode()), transcript)
        transcript["checks"][-1]["status"] = "skip"
        with self.assertRaisesRegex(RuntimeError, "did not pass"):
            self.audit(json.dumps(transcript).encode())

    def test_invalid_transcript_surfaces_cli_stderr(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "no such file"):
            self.audit(b"", b"receipt.json: no such file")
