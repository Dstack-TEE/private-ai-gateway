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
import socket
import struct
import sys
from threading import Event, Thread
import time
from types import SimpleNamespace
import unittest
from contextlib import contextmanager, nullcontext, redirect_stdout
from unittest.mock import AsyncMock, call, patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import dcap_qvl
import jwt
import requests
from cryptography.hazmat.primitives.asymmetric import ec
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
    binding = hashlib.sha256((nonce + key).encode()).digest()
    quote = quote_fixture(report_data=binding + bytes(32), changes=changes)
    material = tdx_session_material(quote)["tdx_report"]
    report = SimpleNamespace(advisory_ids=["SA-B", "SA-A", "SA-B"], to_json=lambda: json.dumps({"status": "UpToDate", "report": {"TD10": material}}))
    profile = {"name": "profile", "mrtd": material["mr_td"], "runtime_rtmrs": {f"RTMR{i}": material[f"rt_mr{i}"] for i in range(4)}}
    with patch.object(dcap_qvl, "get_collateral_and_verify", new=AsyncMock(return_value=report)) as verify:
        result, _ = asyncio.run(chutes.chutes_verify_instance(
            {"instance_id": "instance-a", "quote": base64.b64encode(quote).decode()}, nonce, key, [profile],
        ))
        verify.assert_awaited_once_with(quote)
        return result


def chutes_output(instances, *, timeout_seconds=60):
    key = base64.b64encode(b"key").decode()
    material = tdx_session_material(quote_fixture())
    report = SimpleNamespace(advisory_ids=["SA-B", "SA-A", "SA-B"], to_json=lambda: json.dumps({
        "status": "UpToDate", "report": {"TD10": material["tdx_report"]},
    }))
    profile = {
        "name": "profile",
        "mrtd": material["tdx_report"]["mr_td"],
        **{f"rtmr{i}": material["tdx_report"][f"rt_mr{i}"] for i in range(4)},
    }

    def fetch(url, params=None, **_kwargs):
        if url.endswith("/servers/tdx/measurements"):
            data = [profile]
        elif url.endswith("/evidence"):
            binding = hashlib.sha256((params["nonce"] + key).encode()).digest()
            quote = base64.b64encode(quote_fixture(report_data=binding + bytes(32))).decode()
            data = {"evidence": [dict(instance, quote=quote) for instance in instances]}
        elif "/e2e/instances/" in url:
            data = {"instances": [
                {"instance_id": instance["instance_id"], "e2e_pubkey": key, "nonces": ["discovery"]}
                for instance in instances
            ]}
        else:
            raise AssertionError(f"unexpected network boundary: {url}")
        return SimpleNamespace(status_code=200, content=json.dumps(data).encode(), headers={}, raise_for_status=lambda: None)

    with (
        patch("requests.get", side_effect=fetch),
        patch.object(dcap_qvl, "get_collateral_and_verify", new=AsyncMock(return_value=report)) as verify,
    ):
        output = io.StringIO()
        with redirect_stdout(output):
            asyncio.run(chutes.verify_chutes({
                "model_id": "00000000-0000-0000-0000-000000000000",
                "timeout_seconds": timeout_seconds,
                "provider_options": {
                    "chutes_api_key": "fixture", "chutes_e2ee_discovery_rounds": "1",
                },
            }))
        return json.loads(output.getvalue()), verify.await_count


def nras_jwk(key):
    jwk = jwt.algorithms.ECAlgorithm.to_jwk(key.public_key(), as_dict=True)
    jwk["kid"] = "nvidia-key"
    return jwk


@contextmanager
def silent_http_endpoint():
    accepted = Event()
    release = Event()
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        listener.settimeout(2)

        def hold_connection():
            try:
                connection, _ = listener.accept()
            except TimeoutError:
                return
            with connection:
                accepted.set()
                release.wait(8)

        server = Thread(target=hold_connection)
        server.start()
        try:
            yield f"http://127.0.0.1:{listener.getsockname()[1]}", accepted
        finally:
            release.set()
            server.join()


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

    def assert_chutes_gpu_unknown(self, result, instance_id):
        self.assertEqual(result["result"], "verified")
        self.assertIn(instance_id, [binding["key_id"] for binding in result["channel_bindings"]])
        gpu = result["provider_claims"]["instance_gpu"][instance_id]
        self.assertTrue(gpu["gpu_evidence_present"])
        self.assertFalse(gpu["gpu_verified"])
        self.assertIsNone(gpu["gpu_evidence_nonce_matched"])

    def test_chutes_emits_per_instance_projection_maps(self):
        material = tdx_session_material(quote_fixture())
        gpu = {"gpu_evidence": [{"arch": "HOPPER"}]}
        instances = [
            dict(gpu, tee_type="TDX", instance_id="instance-a"),
            dict(gpu, tee_type="sNp", instance_id="instance-b"),
            dict(gpu, tee_type=None, instance_id="instance-c"),
            {"instance_id": "instance-d", "gpu_evidence": {"unexpected": "shape"}},
        ]
        jwks = {"keys": [nras_jwk(ec.generate_private_key(ec.SECP384R1()))]}
        with (
            patch.object(jwt.PyJWKClient, "fetch_data", return_value=jwks) as fetch,
            patch("requests.post", return_value=SimpleNamespace(status_code=503)) as attest,
        ):
            result, quotes_checked = chutes_output(instances)
        fetch.assert_called_once()
        self.assertEqual(attest.call_count, 2)
        self.assertEqual(quotes_checked, 3)
        self.assert_chutes_gpu_unknown(result, "instance-a")
        self.assert_chutes_gpu_unknown(result, "instance-c")
        self.assert_chutes_gpu_unknown(result, "instance-d")
        accepted = ["instance-a", "instance-c", "instance-d"]
        self.assertEqual([binding["key_id"] for binding in result["channel_bindings"]], accepted)
        claims = result["provider_claims"]
        self.assertEqual(claims["session_material_version"], "chutes/1")
        for instance_id in accepted:
            self.assertEqual(claims["instance_tdx_reports"][instance_id], material["tdx_report"])
            self.assertEqual(claims["instance_tdx_report_sha256"][instance_id], material["tdx_report_sha256"])
            self.assertEqual(claims["instance_advisory_ids"][instance_id], ["SA-A", "SA-B"])

    def test_chutes_keeps_cpu_binding_when_jwks_fetch_fails(self):
        instances = [{"tee_type": "tdx", "instance_id": "instance-a", "gpu_evidence": [{"arch": "HOPPER"}]}]
        with (
            patch.object(jwt.PyJWKClient, "fetch_data", side_effect=jwt.PyJWKClientConnectionError("offline")) as fetch,
            patch("requests.post") as attest,
        ):
            result, _ = chutes_output(instances)
        fetch.assert_called_once()
        attest.assert_not_called()
        self.assert_chutes_gpu_unknown(result, "instance-a")

    def test_chutes_skips_gpu_checks_without_process_deadline_headroom(self):
        instances = [{"tee_type": "tdx", "instance_id": "instance-a", "gpu_evidence": [{"arch": "HOPPER"}]}]
        with patch.object(jwt.PyJWKClient, "fetch_data") as fetch:
            result, _ = chutes_output(instances, timeout_seconds=2)
        fetch.assert_not_called()
        self.assert_chutes_gpu_unknown(result, "instance-a")

    def test_chutes_gpu_budget_does_not_wait_for_a_silent_socket(self):
        instances = [{"tee_type": "tdx", "instance_id": "instance-a", "gpu_evidence": [{"arch": "HOPPER"}]}]
        jwks = {"keys": [nras_jwk(ec.generate_private_key(ec.SECP384R1()))]}
        for endpoint in ("NRAS_JWKS_URL", "NRAS_ATTEST_URL"):
            with self.subTest(endpoint=endpoint), silent_http_endpoint() as (url, accepted):
                fetch = (
                    patch.object(jwt.PyJWKClient, "fetch_data", return_value=jwks)
                    if endpoint == "NRAS_ATTEST_URL" else nullcontext()
                )
                with (
                    fetch,
                    patch.object(chutes, endpoint, url),
                    patch.object(chutes, "NRAS_GPU_BUDGET_SECONDS", 0.5),
                ):
                    started_at = time.monotonic()
                    result, _ = chutes_output(instances)
                    elapsed = time.monotonic() - started_at
                self.assertTrue(accepted.is_set())
                self.assertLess(elapsed, 2)
                self.assert_chutes_gpu_unknown(result, "instance-a")

    def test_chutes_measurements_fallback_only_when_tdx_endpoint_is_unavailable(self):
        base = "https://chutes.example"
        legacy = requests.Response()
        legacy.status_code = 200
        modern = requests.Response()
        modern.status_code = 404
        with patch("requests.get", side_effect=[modern, legacy]) as get:
            self.assertEqual(chutes.chutes_fetch_measurements(base, 10), (f"{base}/servers/tee/measurements", legacy))
            self.assertEqual(get.call_args_list, [
                call(f"{base}/servers/tdx/measurements", timeout=10),
                call(f"{base}/servers/tee/measurements", timeout=10),
            ])

        modern.status_code = 503
        with patch("requests.get", return_value=modern) as get:
            with self.assertRaises(requests.HTTPError):
                chutes.chutes_fetch_measurements(base, 10)
            get.assert_called_once_with(f"{base}/servers/tdx/measurements", timeout=10)

    def test_chutes_flat_profile_rejects_missing_or_mismatched_rtmr(self):
        report = tdx_session_material(quote_fixture())["tdx_report"]
        profile = {"mrtd": report["mr_td"], **{f"rtmr{i}": report[f"rt_mr{i}"] for i in range(4)}}
        for rtmr3 in (None, "ff" * 48):
            with self.subTest(rtmr3=rtmr3):
                self.assertIsNone(chutes.chutes_measurement_name(
                    {"report": {"TD10": report}}, [dict(profile, rtmr3=rtmr3)],
                ))

    def test_chutes_gpu_requires_a_signed_fresh_nras_platform_verdict(self):
        trusted = ec.generate_private_key(ec.SECP384R1())
        untrusted = ec.generate_private_key(ec.SECP384R1())
        jwk = nras_jwk(trusted)
        signing_keys = {jwk["kid"]: jwt.PyJWK.from_dict(jwk)}
        nonce = "aa" * 32
        claims = {
            "iss": "https://nras.attestation.nvidia.com",
            "sub": "NVIDIA-PLATFORM-ATTESTATION",
            "exp": int(time.time()) + 600,
            "eat_nonce": nonce,
            "x-nvidia-overall-att-result": True,
        }

        def token(key=trusted, kid="nvidia-key", algorithm="ES384", **overrides):
            return jwt.encode(claims | overrides, key, algorithm=algorithm, headers={"kid": kid})

        cases = [
            ("verified", token(), True, True),
            ("wrong nonce", token(eat_nonce="bb" * 32), False, False),
            ("negative verdict", token(**{"x-nvidia-overall-att-result": False}), False, True),
            ("unsigned", token(None, algorithm="none"), False, None),
            ("bad signature", token(untrusted), False, None),
            ("unknown signing key", token(kid="unknown"), False, None),
            ("wrong issuer", token(iss="https://other.example"), False, None),
            ("wrong subject", token(sub="GPU"), False, None),
            ("expired", token(exp=0), False, None),
        ]
        for name, platform_jwt, verified, nonce_matched in cases:
            with self.subTest(name=name):
                response = SimpleNamespace(status_code=200, json=lambda: [["JWT", platform_jwt]])
                with patch("requests.post", return_value=response):
                    result = chutes.chutes_verify_gpu([{"arch": "HOPPER"}], nonce, signing_keys)
                self.assertIs(result["gpu_verified"], verified)
                self.assertIs(result["gpu_evidence_nonce_matched"], nonce_matched)

    def test_cross_language_fixtures_match_real_producers(self):
        self.assertEqual(profile_fixtures(), json.loads((ROOT / "tests/fixtures/session_material.json").read_text()))


if __name__ == "__main__":
    if "--emit-fixtures" in sys.argv:
        print(json.dumps(profile_fixtures(), indent=2, sort_keys=True))
    else:
        unittest.main()
