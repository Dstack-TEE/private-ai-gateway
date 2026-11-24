#!/usr/bin/env python3
"""Hermetic reviewed-material regression tests, using the pinned quote parser.

Only network and cryptographic verification boundaries are replaced. Projection,
quote parsing, bindings and compose policy execute their production code.
"""
from __future__ import annotations

import asyncio
import base64
import copy
import gzip
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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
from urllib3.response import HTTPResponse
from confidential_verifier.providers.nearai import NearaiProvider
from confidential_verifier.verifiers.nearai import NearAICloudVerifier
from cryptography.hazmat.primitives.asymmetric import ec
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


def http_response(body, *, status=200, headers=None):
    response = requests.Response()
    response.status_code = status
    response.raw = io.BytesIO(body)
    response.headers.update(headers or {})
    return response


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
    component=None,
):
    gateway = {
        "intel_quote": quote_fixture(report_data=bytes.fromhex(nonce) * 2, changes=changes).hex(),
        "signing_address": signing, "tls_cert_fingerprint": "ab" * 32,
        "info": {"compose_hash": supplied},
    }
    if attestations is None:
        attestations = [{"model_name": "model-a", "intel_quote": "ab"}]
    report = {"gateway_attestation": gateway, "model_attestations": attestations}
    response = http_response(json.dumps(report).encode())
    result = {"is_valid": valid, "details": {"dstack": {"details": {
        "tcb_status": tcb_status, "app_info": {"compose_hash": measured},
    }}}}
    with (
        patch("requests.get", return_value=response),
        patch(
            "confidential_verifier.verifiers.nearai.NearAICloudVerifier.verify_gateway_component",
            new=AsyncMock(return_value=result),
        ),
        patch.object(NearAICloudVerifier, "_verify_component", new=component) if component else nullcontext(),
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


def chutes_output(instances, *, timeout_seconds=60, verify_collateral=None):
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

    def fetch(url, params=None, *, stream, **_kwargs):
        if not stream:
            raise AssertionError(f"response was not streamed: {url}")
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
        return http_response(json.dumps(data).encode())

    with (
        patch("requests.get", side_effect=fetch),
        patch.object(
            dcap_qvl, "get_collateral_and_verify",
            new=AsyncMock(return_value=report, side_effect=verify_collateral),
        ) as verify,
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

    def test_near_missing_model_quote_keeps_only_the_router(self):
        for entry in (
            {"model_name": "model-a", "quote_b64": "qw=="},
            {"model_name": "model-a", "intel_quote": ""},
            {"provider": "chutes", "model": "model-a", "quote_b64": "qw=="},
        ):
            with self.subTest(entry=entry):
                output = near_output(attestations=[entry])
                self.assertEqual(output["result"], "verified")
                self.assertEqual(output["attested_scope"], "router")
                self.assertNotIn("near_model", output["evidence"])
        for entry in (
            {"provider": "chutes", "model": "model-b", "quote_b64": "qw=="},
            {"provider": "chutes", "model": "model-a", "model_name": "model-b"},
            {"provider": "chutes", "model": "model-a", "intel_quote": "ab"},
        ):
            with self.subTest(entry=entry):
                self.assertEqual(near_output(attestations=[entry])["failure"], "binding_mismatch")

    def test_near_requires_all_candidates_to_verify_with_distinct_keys(self):
        signer = "0x" + "11" * 20
        other = "0x" + "22" * 20
        compose = "aa" * 32
        first = {
            "model_name": "model-a", "intel_quote": "ab", "signing_algo": "ecdsa",
            "signing_address": signer, "tls_cert_fingerprint": "aa" * 32,
            "info": {"compose_hash": compose},
        }
        valid = {"is_valid": True, "details": {
            "compose_verified": True, "dstack": {"details": {
                "tcb_status": "UpToDate", "app_info": {"compose_hash": compose},
            }},
        }}
        checked = AsyncMock(return_value=valid)
        output = near_output(attestations=[first], component=checked)
        self.assertEqual(output["evidence"]["near_model"]["verified_candidates"], {signer: 0})
        checked.assert_awaited_once()

        checked = AsyncMock(return_value=valid)
        mixed = near_output(
            attestations=[first, {**first, "intel_quote": "cd"}, {**first, "signing_address": other}],
            component=checked,
        )
        self.assertEqual(mixed["evidence"]["near_model"]["verified_candidates"], {other: 2})
        self.assertEqual(checked.await_count, 3)

        for second, outcomes in (
            ({**first, "intel_quote": "cd"}, []),
            ({**first, "signing_address": other}, [valid, {"is_valid": False}]),
            ({**first, "signing_address": other, "nvidia_payload": {"nonce": "wrong"}}, [valid]),
        ):
            checked = AsyncMock(side_effect=outcomes)
            output = near_output(attestations=[first, second], component=checked)
            self.assertEqual(output["result"], "verified")
            self.assertNotIn("near_model", output["evidence"])
            self.assertEqual(checked.await_count, len(outcomes))

    def test_near_caps_decoded_report_before_parsing(self):
        body = b' ' * 2048 + b'{}'
        compressed = gzip.compress(body)

        def response():
            result = http_response(b"")
            result.raw = HTTPResponse(
                body=io.BytesIO(compressed),
                headers={"Content-Encoding": "gzip", "Content-Length": str(len(compressed))},
                preload_content=False,
            )
            return result

        for limit, expected in [
            (len(body) - 1, "response exceeds"),
            (len(body), "missing model_attestations"),
        ]:
            with (
                self.subTest(limit=limit),
                patch.object(NearaiProvider, "MAX_REPORT_BYTES", limit),
                patch("requests.get", return_value=response()) as get,
            ):
                output = run_near_bridge("model-a")
                self.assertEqual(output["failure"], "evidence_invalid")
                self.assertIn(expected, output["reason"])
                self.assertTrue(get.call_args.kwargs["stream"])

    def test_near_stalled_report_times_out(self):
        release = Event()
        reached = Event()

        class StalledReport(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.send_header("Content-Length", "2")
                self.end_headers()
                reached.set()
                release.wait(20)

        with ThreadingHTTPServer(("127.0.0.1", 0), StalledReport) as server:
            thread = Thread(target=server.serve_forever)
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

    def test_near_stalled_model_verification_keeps_verified_gateway(self):
        reached = Event()
        release = Event()
        finished = Event()

        async def stalled(*_args):
            reached.set()
            release.wait(5)
            finished.set()
            return {"is_valid": False}

        entry = {
            "model_name": "model-a", "intel_quote": "ab", "signing_algo": "ecdsa",
            "signing_address": "0x" + "11" * 20, "tls_cert_fingerprint": "ab" * 32,
        }
        try:
            with patch("provider_verifier.nearai.MODEL_VERIFICATION_BUDGET", 0.1):
                output = near_output(attestations=[entry], component=stalled)
            self.assertTrue(reached.is_set())
            self.assertFalse(finished.is_set())
            self.assertEqual(output["result"], "verified")
            self.assertNotIn("near_model", output["evidence"])
        finally:
            release.set()
            if reached.is_set():
                self.assertTrue(finished.wait(5))

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

    def test_chutes_verifies_a_fleet_concurrently_and_preserves_binding_order(self):
        instances = [{"instance_id": f"instance-{i}"} for i in range(20)]
        active = peak = 0

        async def verify_collateral(quote):
            nonlocal active, peak
            active += 1
            peak = max(peak, active)
            try:
                await asyncio.sleep(0)
                report = tdx_session_material(quote)["tdx_report"]
                return SimpleNamespace(advisory_ids=[], to_json=lambda: json.dumps({
                    "status": "UpToDate", "report": {"TD10": report},
                }))
            finally:
                active -= 1

        result, checked = chutes_output(instances, verify_collateral=verify_collateral)
        self.assertEqual(peak, len(instances))
        self.assertEqual(checked, len(instances))
        self.assertEqual(result["result"], "verified")
        self.assertEqual(
            [binding["key_id"] for binding in result["channel_bindings"]],
            [item["instance_id"] for item in instances],
        )

    def test_chutes_caps_each_response_before_parsing(self):
        documents = [
            ("MAX_MEASUREMENTS_BYTES", "Chutes measurements", b"[]"),
            ("MAX_EVIDENCE_BYTES", "Chutes instance evidence", b"{}"),
            ("MAX_PUBLIC_KEYS_BYTES", "Chutes public keys", b"{}"),
        ]
        for index, (setting, label, _) in enumerate(documents):
            responses = [
                http_response(body if i != index else b"too large")
                for i, (_, _, body) in enumerate(documents[: index + 1])
            ]
            output = io.StringIO()
            with (
                self.subTest(setting=setting),
                patch.object(chutes, setting, 2),
                patch("requests.get", side_effect=responses) as get,
                redirect_stdout(output),
            ):
                asyncio.run(chutes.verify_chutes({
                    "model_id": "00000000-0000-0000-0000-000000000000",
                    "provider_options": {"chutes_api_key": "fixture"},
                }))
                result = json.loads(output.getvalue())
                self.assertEqual(result["failure"], "evidence_invalid")
                self.assertIn(f"{label} response exceeds", result["reason"])
                self.assertEqual(get.call_count, index + 1)
                self.assertTrue(all(request.kwargs["stream"] for request in get.call_args_list))
                self.assertTrue(responses[-1].raw.closed)

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
        legacy = http_response(b"[]", headers={"content-type": "application/json"})
        modern = http_response(b"{}", status=404)
        with patch("requests.get", side_effect=[modern, legacy]) as get:
            self.assertEqual(
                chutes.chutes_fetch_measurements(base, 10),
                (f"{base}/servers/tee/measurements", "application/json", b"[]"),
            )
            self.assertTrue(modern.raw.closed)
            self.assertEqual(get.call_args_list, [
                call(f"{base}/servers/tdx/measurements", timeout=10, stream=True),
                call(f"{base}/servers/tee/measurements", timeout=10, stream=True),
            ])

        modern = http_response(b"{}", status=503)
        with patch("requests.get", return_value=modern) as get:
            with self.assertRaises(requests.HTTPError):
                chutes.chutes_fetch_measurements(base, 10)
            get.assert_called_once_with(f"{base}/servers/tdx/measurements", timeout=10, stream=True)

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
