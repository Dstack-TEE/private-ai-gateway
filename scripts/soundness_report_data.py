#!/usr/bin/env python3
"""Hermetic soundness checks for TDX report_data binding (NEAR AI / dstack path).

Regression guard for the fix that makes the NEAR AI gateway verifier actually
enforce the report_data binding. Before the fix, a wrong request nonce or a swapped
TLS fingerprint still verified, because the report_data check was skipped whenever
the dstack verifier did not surface report_data. These checks pin:

  - verify_report_data() rejects a wrong nonce and a swapped TLS fingerprint, in both
    the address format and the TLS-fingerprint format.
  - _tdx_report_data_hex() parses report_data from the canonical TDX v4 offset and
    fails closed (returns None) on malformed input.

No network, no keys. Run: uv run python scripts/soundness_report_data.py
"""

from __future__ import annotations

import hashlib
import os
import sys

sys.path.append(os.path.dirname(os.path.abspath(__file__)))

from confidential_verifier.verifiers.dstack import verify_report_data  # noqa: E402
from confidential_verifier.verifiers.nearai import _tdx_report_data_hex  # noqa: E402

ADDR = "11" * 20  # 20-byte ethereum-style signing address
NONCE = "22" * 32
FP = "33" * 32


def build_report_data(addr_hex: str, nonce_hex: str, fp_hex: str | None = None) -> str:
    if fp_hex:
        first = hashlib.sha256(bytes.fromhex(addr_hex) + bytes.fromhex(fp_hex)).digest()
    else:
        first = bytes.fromhex(addr_hex) + b"\x00" * (32 - 20)
    return (first + bytes.fromhex(nonce_hex)).hex()


def check() -> list[str]:
    f: list[str] = []

    # --- address format ---
    rd = build_report_data(ADDR, NONCE)
    if not verify_report_data(rd, ADDR, NONCE)["valid"]:
        f.append("address-format: genuine binding should be valid")
    if verify_report_data(rd, ADDR, "44" * 32)["valid"]:
        f.append("address-format: wrong nonce must be rejected")

    # --- TLS-fingerprint format (the NEAR AI path) ---
    rdf = build_report_data(ADDR, NONCE, FP)
    if not verify_report_data(rdf, ADDR, NONCE, tls_cert_fingerprint=FP)["valid"]:
        f.append("fp-format: genuine binding should be valid")
    if verify_report_data(rdf, ADDR, NONCE, tls_cert_fingerprint="55" * 32)["valid"]:
        f.append("fp-format: swapped TLS fingerprint must be rejected")
    if verify_report_data(rdf, ADDR, "44" * 32, tls_cert_fingerprint=FP)["valid"]:
        f.append("fp-format: wrong nonce must be rejected")

    # --- report_data extraction from a synthetic TDX v4 quote ---
    rd_bytes = bytes.fromhex(rd)
    quote = (b"\x00" * 48 + b"\x11" * 520 + rd_bytes + b"\x99" * 16).hex()
    if _tdx_report_data_hex(quote) != rd:
        f.append("_tdx_report_data_hex: must parse report_data at the canonical offset")
    if _tdx_report_data_hex("zz") is not None:
        f.append("_tdx_report_data_hex: malformed hex must return None (fail closed)")
    if _tdx_report_data_hex(None) is not None:
        f.append("_tdx_report_data_hex: missing quote must return None (fail closed)")
    if _tdx_report_data_hex("00" * 8) is not None:
        f.append("_tdx_report_data_hex: too-short quote must return None (fail closed)")

    return f


def main() -> int:
    failures = check()
    if failures:
        print("REPORT_DATA BINDING SOUNDNESS FAILURES:")
        for x in failures:
            print(f"  - {x}")
        return 1
    print("report_data binding soundness checks OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
