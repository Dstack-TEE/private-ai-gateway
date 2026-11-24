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
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
from pathlib import Path
import struct
import sys
import threading
import time
from types import SimpleNamespace
import unittest
from contextlib import redirect_stdout
from unittest.mock import AsyncMock, patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import dcap_qvl
from confidential_verifier.providers.nearai import NearaiProvider
from confidential_verifier.verifiers.nearai import NearAICloudVerifier
from provider_verifier.common import tdx_session_material
from provider_verifier import chutes
import private_ai_provider_verifier as bridge
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


def run_near_bridge(model_id):
    request = {
        "provider": "near-ai",
        "model_id": model_id,
        "url_origin": "https://near.example",
    }
    output = io.StringIO()
    with patch("sys.stdin", io.StringIO(json.dumps(request))), redirect_stdout(output):
        asyncio.run(bridge.main())
    return json.loads(output.getvalue())


def near_output(
    *,
    measured="aa" * 32,
    supplied="bb" * 32,
    signing="0x" + "11" * 20,
    nonce="33" * 32,
    changes=None,
    valid=True,
    attestations=None,
    tcb_status="UpToDate",
):
    gateway = {
        "intel_quote": quote_fixture(report_data=bytes.fromhex(nonce) * 2, changes=changes).hex(),
        "signing_address": signing, "tls_cert_fingerprint": "ab" * 32,
        "info": {"compose_hash": supplied},
    }
    if attestations is None:
        attestations = [{"model_name": "model-a", "intel_quote": "ab"}]
    report = {"gateway_attestation": gateway, "model_attestations": attestations}
    response = SimpleNamespace(json=lambda: report, raise_for_status=lambda: None)
    result = {"is_valid": valid, "details": {"dstack": {"details": {
        "tcb_status": tcb_status, "app_info": {"compose_hash": measured},
    }}}}
    with (
        patch("requests.get", return_value=response),
        patch(
            "confidential_verifier.verifiers.nearai.NearAICloudVerifier.verify_gateway_component",
            new=AsyncMock(return_value=result),
        ),
    ):
        return run_near_bridge("model-a")


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
        stale = near_output(tcb_status="OutOfDate")
        self.assertEqual(stale["result"], "verified")
        self.assertEqual(stale["provider_claims"]["tcb_status"], "OutOfDate")

    def test_near_requires_every_attested_model_name_to_match(self):
        canonical = {"model_name": "model-a", "intel_quote": "ab"}
        second = {"model_name": "model-a", "intel_quote": "cd"}
        self.assertEqual(near_output(attestations=[canonical, second])["result"], "verified")
        reports = [
            [{**canonical, "model_name": "model-b"}],
            [canonical, {**second, "model_name": "Model-a"}],
            [{"intel_quote": "ab"}],
        ]
        for entries in reports:
            with self.subTest(entries=entries):
                output = near_output(attestations=entries)
                self.assertEqual(output["result"], "failed")
                self.assertEqual(output["failure"], "binding_mismatch")
                self.assertIn("model_name does not match", output["reason"])

    def test_near_requires_a_near_hosted_quote(self):
        for entry in (
            {"model_name": "model-a", "quote_b64": "qw=="},
            {"model_name": "model-a", "intel_quote": ""},
        ):
            with self.subTest(entry=entry):
                output = near_output(attestations=[entry])
                self.assertEqual(output["result"], "failed")
                self.assertIn("no NEAR-hosted attestation (missing intel_quote)", output["reason"])

    def test_near_stalled_report_times_out(self):
        release = threading.Event()
        reached = threading.Event()

        class StalledReport(BaseHTTPRequestHandler):
            def do_GET(self):
                reached.set()
                release.wait(20)

        with ThreadingHTTPServer(("127.0.0.1", 0), StalledReport) as server:
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                provider = NearaiProvider(include_tls_fingerprint=True)
                provider.api_base = f"http://127.0.0.1:{server.server_port}/v1"
                started = time.monotonic()
                with patch(
                    "confidential_verifier.providers.nearai.NearaiProvider",
                    return_value=provider,
                ):
                    output = run_near_bridge("stalled")
                elapsed = time.monotonic() - started
                self.assertTrue(reached.is_set())
                self.assertEqual(output["result"], "failed")
                self.assertEqual(output["failure"], "evidence_unavailable")
                self.assertIn("Read timed out", output["reason"])
                self.assertLess(elapsed, 15)
            finally:
                release.set()
                server.shutdown()
                thread.join()

    def test_near_gateway_dstack_uses_process_deadline(self):
        address = bytes.fromhex("11" * 20)
        fingerprint = bytes.fromhex("ab" * 32)
        nonce = bytes.fromhex("33" * 32)
        report_data = hashlib.sha256(address + fingerprint).digest() + nonce
        report = {
            "gateway_attestation": {
                "intel_quote": quote_fixture(report_data=report_data).hex(),
                "signing_address": "0x" + address.hex(),
                "tls_cert_fingerprint": fingerprint.hex(),
            },
        }
        response = SimpleNamespace(
            raise_for_status=lambda: None, json=lambda: {"is_valid": True}
        )
        with patch("requests.post", return_value=response) as post:
            result = asyncio.run(
                NearAICloudVerifier().verify_gateway_component(report, nonce.hex())
            )
        self.assertTrue(result["is_valid"], result["errors"])
        post.assert_called_once()
        self.assertIsNone(post.call_args.kwargs.get("timeout"))

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
