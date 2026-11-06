"""Public captured evidence and offline release/HTTP regression tests.

The attestation fixture was captured from tee.redpill.ai on 2026-11-05.
It retains every replay digest and the payloads needed by this tool, with
unrelated event payloads and workload keys omitted. No inference credentials
or private evidence are used.
"""

import copy
from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime, timedelta, timezone
import hashlib
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import requests
import yaml
from cryptography import x509
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from cryptography.x509.oid import NameOID

from scripts import gateway_measurements as gm

FIXTURES = Path(__file__).parent / "fixtures"
REPORT = gm.read_json(FIXTURES / "gateway-attestation.json")
EXPECTED = gm.read_json(FIXTURES / "gateway-measurements.json")
RELEASE = EXPECTED["release"]


def release(snapshot=EXPECTED, *, prerelease=False, published_at=None):
    tag = snapshot["release"]
    return {
        "tag_name": tag, "draft": False, "prerelease": prerelease,
        "published_at": published_at or ("2026-11-06T00:00:00Z" if prerelease else "2026-11-05T00:58:29Z"),
        "assets": [{"name": "measurements.json", "browser_download_url": f"https://fixture.invalid/{tag}.json"}],
    }


def update_quote_replay(report):
    evidence = report["attestation"]["evidence"]
    quote = bytearray.fromhex(evidence["quote"])
    registers = [bytes(48) for _ in range(4)]
    for event in evidence["event_log"]:
        imr = event["imr"]
        registers[imr] = hashlib.sha384(registers[imr] + bytes.fromhex(event["digest"])).digest()
    for i, register in enumerate(registers):
        quote[48 + 328 + i * 48:48 + 376 + i * 48] = register
    evidence["quote"] = quote.hex()


class MeasurementsTests(unittest.TestCase):
    def test_captured_public_evidence_and_output_files(self):
        actual, app, compose = gm.extract(REPORT, RELEASE)
        self.assertEqual(actual, EXPECTED)
        self.assertEqual(hashlib.sha256(app.encode()).hexdigest(), EXPECTED["app"]["compose_hash"])
        self.assertEqual(json.loads(app)["docker_compose_file"], compose)
        with tempfile.TemporaryDirectory(prefix="extract-") as out:
            with redirect_stdout(io.StringIO()):
                result = gm.main(["extract", "--attestation", str(FIXTURES / "gateway-attestation.json"),
                                  "--release", RELEASE, "--out", out])
            self.assertEqual(result, 0)
            self.assertEqual(gm.read_json(Path(out) / "measurements.json"), EXPECTED)
            self.assertEqual((Path(out) / "app_compose.json").read_text(), app)
            self.assertEqual((Path(out) / "docker-compose.yml").read_text(), compose)

    def test_event_log_and_vm_json_strings(self):
        report = copy.deepcopy(REPORT)
        evidence = report["attestation"]["evidence"]
        evidence["event_log"] = json.dumps(evidence["event_log"])
        evidence["vm_config"] = json.loads(evidence["vm_config"])
        self.assertEqual(gm.extract(report, RELEASE)[0], EXPECTED)

    def test_rejects_replay_mismatch_for_each_register(self):
        for imr in range(4):
            with self.subTest(imr=imr):
                report = copy.deepcopy(REPORT)
                quote = bytearray.fromhex(report["attestation"]["evidence"]["quote"])
                quote[48 + 328 + imr * 48] ^= 1
                report["attestation"]["evidence"]["quote"] = quote.hex()
                with self.assertRaisesRegex(ValueError, f"event log RTMR{imr}"):
                    gm.extract(report, RELEASE)

    def test_rejects_changed_compose_and_event_payload_substitution(self):
        report = copy.deepcopy(REPORT)
        report["attestation"]["evidence"]["app_compose"] += " "
        with self.assertRaisesRegex(ValueError, r"sha256\(app_compose\)"):
            gm.extract(report, RELEASE)
        report = copy.deepcopy(REPORT)
        event = next(event for event in report["attestation"]["evidence"]["event_log"]
                     if event.get("event") == "compose-hash")
        event["event_payload"] = "00" * 32
        with self.assertRaisesRegex(ValueError, "event payload digest mismatch"):
            gm.extract(report, RELEASE)

    def test_rejects_duplicate_boot_event_even_if_replay_matches(self):
        report = copy.deepcopy(REPORT)
        events = report["attestation"]["evidence"]["event_log"]
        events.insert(-1, copy.deepcopy(next(event for event in events if event.get("event") == "compose-hash")))
        update_quote_replay(report)
        with self.assertRaisesRegex(ValueError, "duplicate boot event"):
            gm.extract(report, RELEASE)

    def test_rejects_unmeasured_os_image_and_false_provenance(self):
        report = copy.deepcopy(REPORT)
        vm = json.loads(report["attestation"]["evidence"]["vm_config"])
        vm["os_image_hash"] = "00" * 32
        report["attestation"]["evidence"]["vm_config"] = vm
        with self.assertRaisesRegex(ValueError, "VM OS image hash"):
            gm.extract(report, RELEASE)
        report = copy.deepcopy(REPORT)
        report["attestation"]["source_provenance"]["repo_commit"] = "00" * 20
        with self.assertRaisesRegex(ValueError, "source_provenance"):
            gm.extract(report, RELEASE)

    def test_rejects_unsupported_and_truncated_quotes(self):
        for quote in ("00", "00" * 1000, "zz"):
            report = copy.deepcopy(REPORT)
            report["attestation"]["evidence"]["quote"] = quote
            with self.subTest(quote=quote[:4]), self.assertRaises(ValueError):
                gm.extract(report, RELEASE)

    def test_prediction_hashes_canonical_app_and_updates_source_and_image(self):
        compose = json.loads(REPORT["attestation"]["evidence"]["app_compose"])["docker_compose_file"]
        compose = compose.replace(EXPECTED["app"]["repo_commit"], "ab" * 20)
        compose = compose.replace(EXPECTED["app"]["launcher_image"],
                                  "ghcr.io/redpill-ai/private-ai-launcher@sha256:" + "cd" * 32)
        compose += "\n# Unicode comment: café\n"
        actual, app, rendered_compose = gm.extract(REPORT, "gateway-prod-2026-11-06", compose)
        self.assertEqual(rendered_compose, compose)
        self.assertEqual(app, json.dumps(json.loads(app), sort_keys=True, separators=(",", ":"), ensure_ascii=False))
        self.assertEqual(actual["app"]["compose_hash"], hashlib.sha256(app.encode()).hexdigest())
        self.assertEqual(actual["app"]["repo_commit"], "ab" * 20)
        self.assertEqual(actual["app"]["launcher_image"].split(":")[-1], "cd" * 32)
        for key in ("os", "kms", "tls"):
            self.assertEqual(actual[key], EXPECTED[key])
        # Mapping-style Compose environments are also supported by YAML.
        document = yaml.safe_load(compose)
        launcher = document["services"]["launcher"]
        launcher["environment"] = dict(entry.split("=", 1) for entry in launcher["environment"])
        self.assertEqual(gm.compose_identity(yaml.safe_dump(document))["repo_commit"], "ab" * 20)

    def test_render_contains_all_values_and_cli_reads_fixture(self):
        rendered = gm.render(EXPECTED)
        self.assertTrue(rendered.startswith("| Measurement | Value |\n"))
        for group, fields in gm.FIELDS.items():
            for field in fields:
                self.assertIn(f"| {group}.{field} | `{EXPECTED[group][field]}` |", rendered)
        for hostname, fingerprint in EXPECTED["tls"].items():
            self.assertIn(f"| tls.{hostname} | `{fingerprint}` |", rendered)
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(gm.main(["render", str(FIXTURES / "gateway-measurements.json")]), 0)
        self.assertEqual(output.getvalue(), rendered)

    def test_schema_rejects_missing_fields_and_tls_domain_mismatch(self):
        for group in ("app", "kms", "os", "tls"):
            snapshot = copy.deepcopy(EXPECTED)
            snapshot[group].pop(next(iter(snapshot[group])))
            with self.subTest(group=group), self.assertRaises(ValueError):
                gm.validate(snapshot)

    def test_resolve_retains_original_http_host_tls_sni_and_verification(self):
        adapter = gm.ResolveAdapter("tee.redpill.ai", "192.0.2.1")
        try:
            request = requests.Request("GET", "https://tee.redpill.ai/v1/aci/attestation").prepare()
            host, pool = adapter.build_connection_pool_key_attributes(request, True)
            adapter.add_headers(request)
            self.assertEqual(host["host"], "192.0.2.1")
            self.assertEqual(pool["server_hostname"], "tee.redpill.ai")
            self.assertEqual(pool["assert_hostname"], "tee.redpill.ai")
            self.assertEqual(pool["cert_reqs"], "CERT_REQUIRED")
            self.assertEqual(request.headers["Host"], "tee.redpill.ai")
        finally:
            adapter.close()

    def test_fetch_reads_peer_spki_from_same_connection_before_reading_body(self):
        key = ec.generate_private_key(ec.SECP256R1())
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "tee.redpill.ai")])
        now = datetime.now(timezone.utc)
        certificate = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
                       .public_key(key.public_key()).serial_number(1)
                       .not_valid_before(now - timedelta(minutes=1))
                       .not_valid_after(now + timedelta(minutes=1)).sign(key, hashes.SHA256()))
        der = certificate.public_bytes(Encoding.DER)
        expected = hashlib.sha256(key.public_key().public_bytes(Encoding.DER, PublicFormat.SubjectPublicKeyInfo)).hexdigest()
        observed = []
        def getpeercert(*, binary_form):
            self.assertTrue(binary_form)
            observed.append("peer certificate")
            return der
        def read_body():
            self.assertEqual(observed, ["peer certificate"])
            return REPORT
        response = requests.Response()
        response.status_code = 200
        response._content_consumed = True
        response.raw = SimpleNamespace(connection=SimpleNamespace(sock=SimpleNamespace(getpeercert=getpeercert)),
                                       release_conn=lambda: None)
        with patch.object(requests.Session, "get", return_value=response) as get, patch.object(response, "json", read_body):
            report, actual = gm.fetch_attestation("https://tee.redpill.ai", "tee.redpill.ai:192.0.2.1")
        self.assertIs(report, REPORT)
        self.assertEqual(actual, expected)
        self.assertEqual(get.call_count, 1)
        self.assertTrue(get.call_args.kwargs["stream"])
        with self.assertRaisesRegex(ValueError, "TLS peer certificate unavailable"):
            gm.peer_spki(SimpleNamespace(raw=None))

    def test_malformed_event_struct_is_clean_error_on_extract(self):
        report = copy.deepcopy(REPORT)
        event = next(event for event in report["attestation"]["evidence"]["event_log"] if event.get("event") == "compose-hash")
        event["event_type"] = -1
        with tempfile.TemporaryDirectory() as out:
            attestation = Path(out) / "malformed.json"
            attestation.write_text(json.dumps(report))
            error = io.StringIO()
            with redirect_stderr(error):
                self.assertEqual(gm.main(["extract", "--attestation", str(attestation), "--release", RELEASE, "--out", out]), 1)
            self.assertIn("error:", error.getvalue())
            self.assertNotIn("Traceback", error.getvalue())


class CheckTests(unittest.TestCase):
    def run_check(self, releases=None, assets=None, reports=None, *, paginated=False, served_spkis=None):
        releases = releases if releases is not None else [release()]
        assets = assets if assets is not None else {RELEASE: EXPECTED}
        reports = reports if reports is not None else {}
        calls = []
        def peer_spki(response):
            hostname = response.url.split("/")[2]
            if served_spkis and hostname in served_spkis:
                return served_spkis[hostname]
            report = reports.get(hostname, REPORT)
            if not report:
                return EXPECTED["tls"][hostname]
            return next(key["spki_sha256"] for key in report["attestation"]["workload_keyset"]["tls_public_keys"]
                        if key["domain"] == hostname)
        def get(_session, url, **kwargs):
            calls.append(url)
            response = requests.Response()
            response.status_code = 200
            response.url = url
            if "api.github.com" in url:
                payload = releases if not paginated or "page=2" in url else []
                if paginated and "page=2" not in url:
                    response.headers["Link"] = '<https://api.github.com/repos/fixture/repo/releases?page=2>; rel="next"'
            elif url.startswith("https://fixture.invalid/"):
                payload = assets[url.rsplit("/", 1)[1][:-5]]
                self.assertNotIn("Authorization", kwargs.get("headers", {}))
            else:
                hostname = url.split("/")[2]
                payload = reports.get(hostname, REPORT)
                self.assertRegex(kwargs["params"]["nonce"], r"^[0-9a-f]{64}$")
            response._content = json.dumps(payload).encode()
            response._content_consumed = True
            return response
        output, error = io.StringIO(), io.StringIO()
        with patch.object(requests.Session, "get", get), patch.object(gm, "peer_spki", peer_spki), redirect_stdout(output), redirect_stderr(error):
            result = gm.main(["check", "--repo", "fixture/repo"])
        return result, output.getvalue(), error.getvalue(), calls

    def test_match_checks_every_domain_and_paginates(self):
        result, output, error, calls = self.run_check(paginated=True)
        self.assertEqual((result, error), (0, ""))
        for hostname in EXPECTED["domains"]:
            self.assertIn(f"{hostname}: matches {RELEASE}", output)
            self.assertIn(f"https://{hostname}/v1/aci/attestation", calls)

    def test_mismatch_reports_diff_and_continues_every_domain(self):
        snapshot = copy.deepcopy(EXPECTED)
        snapshot["tls"]["tee.redpill.ai"] = "ff" * 32
        result, _, error, calls = self.run_check(assets={RELEASE: snapshot})
        self.assertEqual(result, 1)
        self.assertIn("--- gateway-prod-2026-11-04", error)
        self.assertIn("+++ live/tee.redpill.ai", error)
        self.assertIn("ff" * 32, error)
        self.assertEqual(sum("/v1/aci/attestation" in url for url in calls), 3)

    def test_rollout_accepts_both_complete_snapshots_on_different_domains(self):
        scheduled = copy.deepcopy(EXPECTED)
        scheduled["release"] = "gateway-prod-2026-11-06"
        scheduled["tls"]["tee.redpill.ai"] = "ee" * 32
        report = copy.deepcopy(REPORT)
        keys = report["attestation"]["workload_keyset"]["tls_public_keys"]
        next(key for key in keys if key["domain"] == "tee.redpill.ai")["spki_sha256"] = "ee" * 32
        result, output, error, _ = self.run_check(
            releases=[release(scheduled, prerelease=True), release()],
            assets={RELEASE: EXPECTED, scheduled["release"]: scheduled},
            reports={"tee.redpill.ai": report},
        )
        self.assertEqual((result, error), (0, ""))
        self.assertIn(f"api.redpill.ai: matches {RELEASE}", output)
        self.assertIn("tee.redpill.ai: matches gateway-prod-2026-11-06", output)

    def test_rollout_rejects_mixing_individual_fields_across_snapshots(self):
        published = copy.deepcopy(EXPECTED)
        published["tls"]["tee.redpill.ai"] = "ff" * 32
        scheduled = copy.deepcopy(EXPECTED)
        scheduled["release"] = "gateway-prod-2026-11-06"
        scheduled["tls"]["api.redpill.ai"] = "ee" * 32
        result, _, error, _ = self.run_check(
            releases=[release(published), release(scheduled, prerelease=True)],
            assets={RELEASE: published, scheduled["release"]: scheduled},
        )
        self.assertEqual(result, 1)
        self.assertIn("no accepted release matches", error)

    def test_stale_prerelease_cannot_authorize_rollback(self):
        published = copy.deepcopy(EXPECTED)
        published["tls"]["tee.redpill.ai"] = "ff" * 32
        old = copy.deepcopy(EXPECTED)
        old["release"] = "gateway-prod-2026-11-03"
        for timestamp in ("2026-11-04T00:00:00Z", release()["published_at"]):
            with self.subTest(published_at=timestamp):
                result, _, error, calls = self.run_check(
                    releases=[release(old, prerelease=True, published_at=timestamp), release(published)],
                    assets={RELEASE: published, old["release"]: old},
                )
                self.assertEqual(result, 1)
                self.assertIn("no accepted release matches", error)
                self.assertNotIn(f"https://fixture.invalid/{old['release']}.json", calls)

    def test_served_tls_key_must_match_same_release_as_attestation(self):
        result, output, error, _ = self.run_check(served_spkis={"tee.redpill.ai": "ff" * 32})
        self.assertEqual(result, 1)
        self.assertIn("served TLS SPKI SHA-256=" + "ff" * 32, error)
        self.assertIn(EXPECTED["tls"]["tee.redpill.ai"], error)
        self.assertIn(f"api.redpill.ai: matches {RELEASE}", output)
        scheduled = copy.deepcopy(EXPECTED)
        scheduled["release"] = "gateway-prod-2026-11-06"
        scheduled["tls"]["tee.redpill.ai"] = "ff" * 32
        result, _, error, _ = self.run_check(
            releases=[release(), release(scheduled, prerelease=True)],
            assets={RELEASE: EXPECTED, scheduled["release"]: scheduled},
            served_spkis={"tee.redpill.ai": "ff" * 32},
        )
        self.assertEqual(result, 1)
        self.assertIn("tee.redpill.ai: no accepted release matches", error)

    def test_malformed_struct_fails_domain_cleanly_and_continues(self):
        report = copy.deepcopy(REPORT)
        event = next(event for event in report["attestation"]["evidence"]["event_log"] if event.get("event") == "compose-hash")
        event["event_type"] = -1
        result, output, error, _ = self.run_check(reports={"api.redpill.ai": report})
        self.assertEqual(result, 1)
        self.assertIn("api.redpill.ai:", error)
        self.assertNotIn("Traceback", error)
        self.assertIn("tee.redpill.ai: matches", output)

    def test_latest_selection_ignores_other_release_families_and_drafts(self):
        old = copy.deepcopy(EXPECTED)
        old["release"] = "gateway-prod-2026-11-03"
        draft = {**release(), "draft": True, "published_at": None, "assets": []}
        desktop = {**release(), "tag_name": "desktop-v9", "assets": []}
        result, _, error, _ = self.run_check(releases=[release(old, published_at="2026-11-04T00:00:00Z"),
                                                     draft, desktop, release()])
        self.assertEqual((result, error), (0, ""))

    def test_missing_release_asset_is_clear_error_before_live_requests(self):
        result, _, error, calls = self.run_check(releases=[{**release(), "assets": []}])
        self.assertEqual(result, 1)
        self.assertIn(f"{RELEASE}: missing or duplicate measurements.json", error)
        self.assertEqual(len(calls), 1)

    def test_prerelease_missing_asset_and_wrong_asset_tag_fail_closed(self):
        scheduled = copy.deepcopy(EXPECTED)
        scheduled["release"] = "gateway-prod-2026-11-06"
        result, _, error, _ = self.run_check(
            releases=[release(), {**release(scheduled, prerelease=True), "assets": []}],
        )
        self.assertEqual(result, 1)
        self.assertIn("gateway-prod-2026-11-06: missing or duplicate measurements.json", error)
        result, _, error, _ = self.run_check(assets={RELEASE: scheduled})
        self.assertEqual(result, 1)
        self.assertIn("asset release != release tag", error)

    def test_network_errors_return_nonzero_without_traceback(self):
        error = io.StringIO()
        with patch.object(requests.Session, "get", side_effect=requests.Timeout("fixture timeout")), redirect_stderr(error):
            self.assertEqual(gm.main(["check"]), 1)
        self.assertIn("error: fixture timeout", error.getvalue())
        self.assertNotIn("Traceback", error.getvalue())

    def test_unavailable_live_domain_does_not_skip_other_domains(self):
        result, output, error, _ = self.run_check(reports={"api.redpill.ai": {}})
        self.assertEqual(result, 1)
        self.assertIn("api.redpill.ai:", error)
        self.assertIn("tee.redpill.ai: matches", output)


if __name__ == "__main__":
    unittest.main()
