#!/usr/bin/env python3
"""Hermetic reviewed-material regression tests, using the pinned quote parser.

Only network and cryptographic verification boundaries are replaced. Projection,
quote parsing, bindings and compose policy execute their production code.
"""
from __future__ import annotations

import asyncio
import base64
import copy
import hashlib
import io
import json
from pathlib import Path
import struct
import sys
from types import SimpleNamespace
import unittest
from contextlib import redirect_stdout
from unittest.mock import AsyncMock, patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import dcap_qvl
from provider_verifier.common import tdx_session_material
from provider_verifier import nearai, chutes
import soundness_phala_direct as phala

# Serialized fixture layout from the pinned dcap-qvl TD report definitions.
FIELDS = [
    ("tee_tcb_svn", 16), ("mr_seam", 48), ("mr_signer_seam", 48),
    ("seam_attributes", 8), ("td_attributes", 8), ("xfam", 8),
    ("mr_td", 48), ("mr_config_id", 48), ("mr_owner", 48),
    ("mr_owner_config", 48), ("rt_mr0", 48), ("rt_mr1", 48),
    ("rt_mr2", 48), ("rt_mr3", 48),
]


def quote_fixture(*, td15=False, report_data=bytes(64), changes=None):
    fields = FIELDS + [("report_data", 64)]
    if td15:
        fields += [("tee_tcb_svn2", 16), ("mr_service_td", 48)]
    values = {name: bytes([0x22]) * size for name, size in fields}
    values["td_attributes"] = bytes(8)  # debug gate stays real
    values["report_data"] = report_data
    values.update(changes or {})
    body = b"".join(values[name] for name, _ in fields)
    header = struct.pack("<HHIHH16s20s", 5 if td15 else 4, 2, 0x81, 0, 0, bytes(16), bytes(20))
    # Minimal parseable ECDSA authentication data; the crypto boundary is mocked.
    qe = bytes(384 + 64) + struct.pack("<HHI", 0, 5, 0)
    auth = bytes(128) + struct.pack("<HI", 6, len(qe)) + qe
    return header + (struct.pack("<HI", 3, len(body)) if td15 else b"") + body + struct.pack("<I", len(auth)) + auth


def near_output(*, measured="aa" * 32, supplied="bb" * 32, signing="0x" + "11" * 20, nonce="33" * 32, changes=None, valid=True):
    gateway = {
        "intel_quote": quote_fixture(report_data=bytes.fromhex(nonce) * 2, changes=changes).hex(),
        "signing_address": signing, "tls_cert_fingerprint": "ab" * 32,
        "info": {"compose_hash": supplied},
    }
    report = SimpleNamespace(raw={"gateway_attestation": gateway}, request_nonce=nonce, model_dump=lambda **_: {})
    result = {"is_valid": valid, "details": {"dstack": {"details": {
        "tcb_status": "UpToDate", "app_info": {"compose_hash": measured},
    }}}}
    with patch("confidential_verifier.providers.nearai.NearaiProvider.fetch_report", return_value=report), patch("confidential_verifier.verifiers.nearai.NearAICloudVerifier.verify_gateway_component", new=AsyncMock(return_value=result)):
        output = io.StringIO()
        with redirect_stdout(output):
            asyncio.run(nearai.verify_nearai({"model_id": "model-a", "url_origin": "https://near.example"}))
        return json.loads(output.getvalue())


def phala_output(*, changes=None, compose=phala.APP_COMPOSE):
    def build(nonce):
        report = phala._report(nonce, app_compose=compose)
        rd = hashlib.sha256(bytes.fromhex(phala.ADDR + phala.FP)).digest() + bytes.fromhex(nonce)
        report["intel_quote"] = quote_fixture(report_data=rd, changes=changes).hex()
        return report
    return phala._run(report_builder=build, compose_hash=hashlib.sha256(compose.encode()).hexdigest())


def chutes_instance(*, nonce="nonce-a", changes=None):
    key = base64.b64encode(b"key").decode()
    rd = hashlib.sha256((nonce + key).encode()).digest() + bytes(32)
    quote = quote_fixture(report_data=rd, changes=changes)
    material = tdx_session_material(quote)["tdx_report"]
    report = SimpleNamespace(advisory_ids=["SA-B", "SA-A", "SA-B"], to_json=lambda: json.dumps({"status": "UpToDate", "report": {"TD10": material}}))
    profile = {"name": "profile", "mrtd": material["mr_td"], "runtime_rtmrs": {f"RTMR{i}": material[f"rt_mr{i}"] for i in range(4)}}
    with patch.object(dcap_qvl, "get_collateral_and_verify", new=AsyncMock(return_value=report)) as verify:
        result = asyncio.run(chutes.chutes_verify_instance({"instance_id": "instance-a", "quote": base64.b64encode(quote).decode()}, nonce, key, [profile], 5))
        verify.assert_awaited_once_with(quote)
        return result


def profile_fixtures():
    instance = chutes_instance()
    chutes_claims = {
        "session_material_version": "chutes/1", "trust_boundary": "model_instance",
        "evidence_scope": "model_instance", "chute_id": "chute", "canonical_model_id": "model",
        "instance_id": instance["instance_id"], "measurement": instance["measurement"],
        "tcb_status": instance["tcb_status"], "advisory_ids": instance["advisory_ids"],
        **instance["gpu"], "tdx_report": instance["tdx_report"], "tdx_report_sha256": instance["tdx_report_sha256"],
    }
    return {"near-ai": near_output()["provider_claims"], "phala-direct": phala_output()["provider_claims"],
            "chutes": chutes_claims,
            "privatemode": {"session_material_version": "privatemode/1", "trust_boundary": "attested-compose-privatemode-proxy", "attestation_scope": "contrast-attested-e2ee-secret", "request_encryption": "privatemode-oae", "success_response_authentication": "privatemode-oae", "inference_secret_policy": "latest-per-attempt-fail-closed", "manifest_mode": "dynamic", "observed_manifest_sha256": "aa" * 32, "manifest_observed_at": "2026-10-16T12:00:00Z", "manifest_observation": "latest-proxy-fetch-log", "manifest_bound_to_active_secret": False}}


class SessionMaterialTests(unittest.TestCase):
    def test_each_tdx_security_field_and_freshness(self):
        for td15 in (False, True):
            base = tdx_session_material(quote_fixture(td15=td15))
            self.assertEqual(base, tdx_session_material(quote_fixture(td15=td15, report_data=b"f" * 64)))
            fields = FIELDS + ([("tee_tcb_svn2", 16), ("mr_service_td", 48)] if td15 else [])
            for name, size in fields:
                with self.subTest(format=td15, field=name):
                    changed = tdx_session_material(quote_fixture(td15=td15, changes={name: b"c" * size}))
                    self.assertNotEqual(base["tdx_report_sha256"], changed["tdx_report_sha256"])
                    self.assertEqual(changed["tdx_report"][name], "63" * size)
            self.assertEqual(base["tdx_report"]["report_format"], "td15" if td15 else "td10")
        self.assertNotEqual(tdx_session_material(quote_fixture()), tdx_session_material(quote_fixture(td15=True)))

    def test_missing_malformed_unsupported_quotes(self):
        for quote in (None, b"", b"x" * 100, "zz", quote_fixture()[:100]):
            self.assertEqual(tdx_session_material(quote), {})
        # A parseable SGX report has no reviewed TDX projection.
        header = struct.pack("<HHIHH16s20s", 3, 2, 0, 0, 0, bytes(16), bytes(20))
        auth = bytes(64 + 64 + 384 + 64) + struct.pack("<HHI", 0, 5, 0)
        sgx = header + bytes(384) + struct.pack("<I", len(auth)) + auth
        self.assertIsInstance(dcap_qvl.Quote.parse(sgx).report, dcap_qvl.SgxEnclaveReport)
        self.assertEqual(tdx_session_material(sgx), {})

    def test_near_authenticated_compose_and_identity(self):
        base = near_output()["provider_claims"]
        self.assertEqual(base["compose_hash"], "aa" * 32)
        self.assertEqual(base, near_output(supplied="cc" * 32, nonce="44" * 32)["provider_claims"])
        for kwargs in ({"measured": "dd" * 32}, {"signing": "0x" + "22" * 20}, {"changes": {"mr_td": b"z" * 48}}):
            self.assertNotEqual(base, near_output(**kwargs)["provider_claims"])
        self.assertNotIn("session_material_version", near_output(measured=None)["provider_claims"])
        self.assertEqual(near_output(valid=False)["result"], "failed")
        self.assertNotIn("canonical_model_id", base)

    def test_phala_compose_and_quote(self):
        base = phala_output()["provider_claims"]
        self.assertEqual(base["session_material_version"], "phala-direct/1")
        self.assertEqual(base, phala_output()["provider_claims"])  # fresh random nonce
        self.assertNotEqual(base, phala_output(compose="services: [changed]")["provider_claims"])
        self.assertNotEqual(base, phala_output(changes={"mr_owner": b"z" * 48})["provider_claims"])

    def test_chutes_real_measurements_and_advisories(self):
        base = chutes_instance()
        self.assertEqual(base, chutes_instance(nonce="nonce-b"))
        self.assertEqual(base["advisory_ids"], ["SA-A", "SA-B"])
        self.assertNotEqual(base["tdx_report_sha256"], chutes_instance(changes={"rt_mr3": b"z" * 48})["tdx_report_sha256"])
        self.assertIn("gpu_evidence_nonce_matched", base["gpu"])

    def test_chutes_emits_per_instance_projection_maps(self):
        key = base64.b64encode(b"key").decode()
        material = tdx_session_material(quote_fixture())
        report = SimpleNamespace(advisory_ids=["SA-B", "SA-A", "SA-B"], to_json=lambda: json.dumps({
            "status": "UpToDate", "report": {"TD10": material["tdx_report"]},
        }))
        profile = {"name": "profile", "mrtd": material["tdx_report"]["mr_td"],
                   "runtime_rtmrs": {f"RTMR{i}": material["tdx_report"][f"rt_mr{i}"] for i in range(4)}}
        def fetch(url, params=None, **_kwargs):
            if url.endswith("/measurements"):
                data = [profile]
            elif url.endswith("/evidence"):
                nonce = params["nonce"]
                rd = hashlib.sha256((nonce + key).encode()).digest() + bytes(32)
                data = {"evidence": [{"instance_id": "instance-a", "quote": base64.b64encode(quote_fixture(report_data=rd)).decode()}]}
            elif "/e2e/instances/" in url:
                data = {"instances": [{"instance_id": "instance-a", "e2e_pubkey": key, "nonces": ["discovery"]}]}
            else:
                raise AssertionError(f"unexpected network boundary: {url}")
            return SimpleNamespace(content=json.dumps(data).encode(), headers={}, raise_for_status=lambda: None)
        with patch("requests.get", side_effect=fetch), patch.object(dcap_qvl, "get_collateral_and_verify", new=AsyncMock(return_value=report)) as verify:
            output = io.StringIO()
            with redirect_stdout(output):
                asyncio.run(chutes.verify_chutes({"model_id": "00000000-0000-0000-0000-000000000000", "provider_options": {
                    "chutes_api_key": "fixture", "chutes_e2ee_discovery_rounds": "1",
                }}))
            result = json.loads(output.getvalue())
            verify.assert_awaited_once()
        self.assertEqual(result["result"], "verified")
        claims = result["provider_claims"]
        self.assertEqual(claims["session_material_version"], "chutes/1")
        self.assertEqual(claims["instance_tdx_reports"], {"instance-a": material["tdx_report"]})
        self.assertEqual(claims["instance_tdx_report_sha256"], {"instance-a": material["tdx_report_sha256"]})
        self.assertEqual(claims["instance_advisory_ids"], {"instance-a": ["SA-A", "SA-B"]})
        self.assertIn("gpu_evidence_nonce_matched", claims["instance_gpu"]["instance-a"])

    def test_cross_language_fixtures_match_real_producers(self):
        self.assertEqual(profile_fixtures(), json.loads((ROOT / "tests/fixtures/session_material.json").read_text()))


if __name__ == "__main__":
    if "--emit-fixtures" in sys.argv:
        print(json.dumps(profile_fixtures(), indent=2, sort_keys=True))
    else:
        unittest.main()
