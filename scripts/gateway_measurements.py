#!/usr/bin/env python3
"""Extract release measurements, render release notes, and check live drift.

This is a measurement consistency tool, not a cryptographic quote verifier.
Run with the repository's locked uv environment (requests, PyYAML, cryptography).
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import struct
import sys
from urllib.parse import urlsplit

import requests
import yaml
from cryptography import x509
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

SCHEMA = "gateway-measurements/v1"
DEFAULT_REPO = "Dstack-TEE/private-ai-gateway"
RELEASE_PATTERN = r"gateway-prod-\d{4}-\d{2}-\d{2}"
FIELDS = {
    "app": ("compose_hash", "app_id", "repo_url", "repo_commit", "launcher_image"),
    "kms": ("root_key",),
    "os": ("image", "image_hash", "mrtd", "rtmr0", "rtmr1", "rtmr2", "vcpus", "memory_bytes"),
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def hex_bytes(value, size=None):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-fA-F]+", value), "expected hexadecimal string")
    result = bytes.fromhex(value)
    require(size is None or len(result) == size, f"expected {size} hex bytes")
    return result


def text(value):
    require(isinstance(value, str) and bool(value.strip()), "expected nonempty string")
    return value


def domain(value):
    text(value)
    require(len(value) <= 253 and all(
        re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
        for label in value.split(".")
    ), f"invalid domain: {value!r}")
    return value


def json_text(value):
    return json.dumps(value, indent=2, ensure_ascii=False) + "\n"


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


class ResolveAdapter(requests.adapters.HTTPAdapter):
    """Use Requests' pool-key hook to change the address while retaining TLS SNI."""

    def __init__(self, hostname, address):
        self.hostname = hostname
        self.address = address
        super().__init__()

    def build_connection_pool_key_attributes(self, request, verify, cert=None):
        host, pool = super().build_connection_pool_key_attributes(request, verify, cert)
        require(host["host"] == self.hostname, "resolve host does not match URL")
        host["host"] = self.address
        pool.update(server_hostname=self.hostname, assert_hostname=self.hostname)
        return host, pool

    def add_headers(self, request, **kwargs):
        super().add_headers(request, **kwargs)
        request.headers["Host"] = urlsplit(request.url).netloc


def peer_spki(response):
    # Read before consuming the streamed response, which releases its connection.
    connection = getattr(response.raw, "connection", None)
    sock = getattr(connection, "sock", None)
    require(sock is not None, "TLS peer certificate unavailable on attestation connection")
    certificate = x509.load_der_x509_certificate(sock.getpeercert(binary_form=True))
    spki = certificate.public_key().public_bytes(Encoding.DER, PublicFormat.SubjectPublicKeyInfo)
    return hashlib.sha256(spki).hexdigest()


def fetch_attestation(url, resolve=None):
    parsed = urlsplit(url)
    require(parsed.scheme == "https" and parsed.hostname and not parsed.username
            and not parsed.password and not parsed.query and not parsed.fragment
            and parsed.path in ("", "/", "/v1", "/v1/"), "expected HTTPS gateway base URL")
    with requests.Session() as session:
        if resolve:
            hostname, separator, address = resolve.partition(":")
            require(separator and hostname == parsed.hostname, "--resolve must be URL-host:ip")
            ipaddress.ip_address(address)
            # An explicit address must bypass environment proxies; TLS remains verified.
            session.trust_env = False
            session.mount("https://", ResolveAdapter(hostname, address))
        with session.get(
            f"https://{parsed.netloc}/v1/aci/attestation",
            params={"nonce": secrets.token_hex(32)}, timeout=(10, 60), allow_redirects=False,
            stream=True,
        ) as response:
            require(response.status_code == 200, f"{parsed.hostname}: attestation HTTP {response.status_code}")
            served_spki = peer_spki(response)
            return response.json(), served_spki


def measured_evidence(report):
    require(isinstance(report, dict), "attestation report must be an object")
    attestation = report["attestation"]
    require(isinstance(attestation, dict), "attestation must be an object")
    evidence = attestation["evidence"]
    require(isinstance(evidence, dict), "evidence must be an object")
    quote = hex_bytes(evidence["quote"])
    require(len(quote) >= 48 + 584 and struct.unpack_from("<H", quote)[0] == 4
            and struct.unpack_from("<I", quote, 4)[0] == 0x81, "expected TDX v4 quote")
    body = quote[48:]
    events = evidence["event_log"]
    if isinstance(events, str):
        events = json.loads(events)
    require(isinstance(events, list), "event_log must be a JSON list")
    registers = [bytes(48) for _ in range(4)]
    boot = {}
    ready = False
    for event in events:
        require(isinstance(event, dict), "event must be an object")
        imr = event["imr"]
        require(type(imr) is int and 0 <= imr < 4, "invalid event IMR")
        digest = hex_bytes(event["digest"], 48)
        registers[imr] = hashlib.sha384(registers[imr] + digest).digest()
        name = event.get("event")
        if imr != 3 or ready:
            continue
        if name == "system-ready":
            ready = True
        if name in ("app-id", "compose-hash", "os-image-hash", "key-provider", "system-ready"):
            require(name not in boot, f"duplicate boot event: {name}")
            payload = bytes.fromhex(event["event_payload"])
            expected = hashlib.sha384(struct.pack("<I", event["event_type"])
                                      + b":" + name.encode() + b":" + payload).digest()
            require(digest == expected, f"event payload digest mismatch: {name}")
            boot[name] = payload
    for i, register in enumerate(registers):
        require(register == body[328 + i * 48:376 + i * 48], f"event log RTMR{i} != quote RTMR{i}")
    require(ready, "event log has no system-ready event")
    require({"app-id", "compose-hash", "os-image-hash", "key-provider"} <= boot.keys(),
            "event log is missing a required boot measurement")
    app_compose = text(evidence["app_compose"])
    compose_hash = hashlib.sha256(app_compose.encode()).digest()
    require(compose_hash == boot["compose-hash"], "sha256(app_compose) != measured compose-hash")
    vm = evidence["vm_config"]
    if isinstance(vm, str):
        vm = json.loads(vm)
    require(isinstance(vm, dict), "vm_config must be an object")
    require(hex_bytes(vm["os_image_hash"], 32) == boot["os-image-hash"], "VM OS image hash != measured OS image hash")
    key_provider = json.loads(boot["key-provider"])
    require(isinstance(key_provider, dict), "key-provider payload must be an object")
    require(key_provider["name"] == "kms", "expected KMS key provider")
    return attestation, app_compose, {
        "compose_hash": compose_hash.hex(),
        "app_id": boot["app-id"].hex(),
        "root_key": text(key_provider["id"]),
        "os": {
            "image": vm["image"], "image_hash": vm["os_image_hash"],
            "mrtd": body[136:184].hex(),
            **{f"rtmr{i}": registers[i].hex() for i in range(3)},
            "vcpus": vm["cpu_count"], "memory_bytes": vm["memory_size"],
        },
    }


def compose_identity(compose):
    document = yaml.safe_load(compose)
    require(isinstance(document, dict) and isinstance(document.get("services"), dict),
            "Compose must contain a services object")
    require(all(isinstance(service, dict) for service in document["services"].values()),
            "Compose services must be objects")
    launchers = [service for service in document["services"].values()
                 if "private-ai-launcher@sha256:" in str(service.get("image", ""))]
    require(len(launchers) == 1, "expected one private-ai-launcher service pinned by digest")
    launcher = launchers[0]
    environment = launcher["environment"]
    if isinstance(environment, list):
        require(all(isinstance(item, str) and "=" in item for item in environment),
                "launcher environment must contain literal KEY=value entries")
        environment = dict(item.split("=", 1) for item in environment)
    require(isinstance(environment, dict), "launcher environment must be a list or object")
    return {
        "repo_url": environment["PRIVATE_AI_GATEWAY_REPO_URL"],
        "repo_commit": environment["PRIVATE_AI_GATEWAY_REPO_COMMIT"],
        "launcher_image": launcher["image"],
    }


def extract(report, release, compose=None):
    attestation, app_compose, measured = measured_evidence(report)
    app = json.loads(app_compose)
    require(isinstance(app, dict), "app_compose must be an object")
    identity = compose_identity(app["docker_compose_file"])
    provenance = attestation["source_provenance"]
    require(isinstance(provenance, dict), "source_provenance must be an object")
    require(all(identity[key] == provenance[key] for key in ("repo_url", "repo_commit")),
            "source_provenance != measured launcher source pin")
    if compose is not None:
        app["docker_compose_file"] = compose
        app_compose = json.dumps(app, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        measured["compose_hash"] = hashlib.sha256(app_compose.encode()).hexdigest()
        identity = compose_identity(compose)
    tls = {}
    keyset = attestation["workload_keyset"]
    require(isinstance(keyset, dict) and isinstance(keyset.get("tls_public_keys"), list),
            "workload_keyset must contain a tls_public_keys list")
    for entry in keyset["tls_public_keys"]:
        require(isinstance(entry, dict), "TLS entry must be an object")
        hostname = domain(entry["domain"])
        require(hostname not in tls, f"duplicate TLS domain: {hostname}")
        tls[hostname] = hex_bytes(entry["spki_sha256"], 32).hex()
    measurements = {
        "schema": SCHEMA, "release": release, "domains": sorted(tls),
        "app": {"compose_hash": measured["compose_hash"], "app_id": measured["app_id"], **identity},
        "kms": {"root_key": measured["root_key"]}, "os": measured["os"],
        "tls": {hostname: tls[hostname] for hostname in sorted(tls)},
    }
    validate(measurements)
    return measurements, app_compose, app["docker_compose_file"]


def validate(measurements):
    require(isinstance(measurements, dict), "measurements must be an object")
    require(set(measurements) == {"schema", "release", "domains", "app", "kms", "os", "tls"},
            "invalid measurements fields")
    require(measurements["schema"] == SCHEMA, f"expected schema {SCHEMA}")
    require(re.fullmatch(RELEASE_PATTERN, text(measurements["release"])), "invalid gateway release tag")
    domains = measurements["domains"]
    require(isinstance(domains, list) and domains and len(set(domains)) == len(domains), "expected unique domains")
    for hostname in domains:
        domain(hostname)
    require(isinstance(measurements["tls"], dict) and set(measurements["tls"]) == set(domains), "TLS domains != domains")
    for value in measurements["tls"].values():
        hex_bytes(value, 32)
    for group, fields in FIELDS.items():
        require(isinstance(measurements[group], dict) and set(measurements[group]) == set(fields), f"invalid {group} fields")
        for field in fields:
            value = measurements[group][field]
            if field in ("vcpus", "memory_bytes"):
                require(type(value) is int and value > 0, f"expected positive integer: {field}")
            else:
                text(value)
    for group, field, size in (("app", "compose_hash", 32), ("app", "app_id", 20),
                               ("os", "image_hash", 32), ("os", "mrtd", 48),
                               *(("os", f"rtmr{i}", 48) for i in range(3))):
        hex_bytes(measurements[group][field], size)
    hex_bytes(measurements["kms"]["root_key"])
    require(re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", measurements["app"]["repo_commit"]), "expected full repo_commit")
    require(re.fullmatch(r"[^\s]+/private-ai-launcher@sha256:[0-9a-f]{64}",
                         measurements["app"]["launcher_image"]), "expected pinned launcher_image")


def render(measurements):
    validate(measurements)
    rows = [("Release", measurements["release"]), ("Domains", ", ".join(measurements["domains"]))]
    rows += [(f"{group}.{field}", measurements[group][field])
             for group, fields in FIELDS.items() for field in fields]
    rows += [(f"tls.{hostname}", measurements["tls"][hostname]) for hostname in sorted(measurements["tls"])]
    def cell(value):
        return str(value).replace("|", "&#124;").replace("\n", " ").replace("`", "&#96;")
    return "| Measurement | Value |\n| --- | --- |\n" + "".join(
        f"| {label} | `{cell(value)}` |\n" for label, value in rows
    )


def release_measurements(repo):
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo), "expected owner/repo")
    headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"}
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    with requests.Session() as session:
        releases = []
        url = f"https://api.github.com/repos/{repo}/releases?per_page=100"
        while url:
            response = session.get(url, headers=headers, timeout=(10, 60))
            response.raise_for_status()
            page = response.json()
            require(isinstance(page, list) and all(isinstance(release, dict) for release in page),
                    "GitHub releases response must be a list of objects")
            releases.extend(release for release in page
                            if not release["draft"] and re.fullmatch(RELEASE_PATTERN, release["tag_name"]))
            url = response.links.get("next", {}).get("url")
        published = [release for release in releases if not release["prerelease"]]
        require(published, "no published gateway-prod release found")
        latest = max(published, key=lambda release: release["published_at"])
        accepted = [latest] + [release for release in releases
                               if release["prerelease"] and release["published_at"] > latest["published_at"]]
        snapshots = []
        for release in accepted:
            assets = [asset for asset in release["assets"] if asset["name"] == "measurements.json"]
            require(len(assets) == 1, f"{release['tag_name']}: missing or duplicate measurements.json release asset")
            # Do not send the API token to release-download hosts.
            asset_response = session.get(assets[0]["browser_download_url"], timeout=(10, 60))
            asset_response.raise_for_status()
            measurements = asset_response.json()
            validate(measurements)
            require(measurements["release"] == release["tag_name"], "asset release != release tag")
            snapshots.append(measurements)
    return snapshots


def stable(measurements):
    return {key: sorted(value) if key == "domains" else value
            for key, value in measurements.items() if key != "release"}


def check(repo):
    snapshots = release_measurements(repo)
    failures = []
    domains = sorted({hostname for snapshot in snapshots for hostname in snapshot["domains"]})
    for hostname in domains:
        candidates = [snapshot for snapshot in snapshots if hostname in snapshot["domains"]]
        try:
            report, served_spki = fetch_attestation(f"https://{hostname}")
            live, _, _ = extract(report, candidates[0]["release"])
            match = next((snapshot for snapshot in candidates
                          if stable(live) == stable(snapshot) and served_spki == snapshot["tls"][hostname]), None)
            if match:
                print(f"{hostname}: matches {match['release']}")
                continue
            expected_spkis = ", ".join(f"{snapshot['release']}={snapshot['tls'][hostname]}" for snapshot in candidates)
            failures.append(f"{hostname}: no accepted release matches\n"
                            f"served TLS SPKI SHA-256={served_spki}; expected {expected_spkis}\n" + "\n".join(
                "".join(difflib.unified_diff(
                    json.dumps(stable(snapshot), sort_keys=True, indent=2).splitlines(keepends=True),
                    json.dumps(stable(live), sort_keys=True, indent=2).splitlines(keepends=True),
                    fromfile=snapshot["release"], tofile=f"live/{hostname}",
                )) for snapshot in candidates
            ))
        except (OSError, ValueError, KeyError, TypeError, struct.error, requests.RequestException, yaml.YAMLError) as exc:
            failures.append(f"{hostname}: {exc}")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    extraction = commands.add_parser("extract", help="extract live measurements or predict a compose update")
    source = extraction.add_mutually_exclusive_group(required=True)
    source.add_argument("--url", help="HTTPS gateway base URL")
    source.add_argument("--attestation", type=Path, help="captured public attestation JSON")
    extraction.add_argument("--resolve", help="host:ip (direct connection, retaining SNI and certificate validation)")
    extraction.add_argument("--release", required=True, help="gateway-prod-YYYY-MM-DD")
    extraction.add_argument("--compose", type=Path, help="predict compose/source/image from new Docker Compose YAML")
    extraction.add_argument("--out", type=Path, default=Path("."))
    rendering = commands.add_parser("render", help="render a release-note Markdown values table")
    rendering.add_argument("measurements", type=Path)
    checking = commands.add_parser("check", help="compare every release domain to live measurements")
    checking.add_argument("--repo", default=DEFAULT_REPO)
    args = parser.parse_args(argv)
    try:
        if args.command == "extract":
            require(not args.resolve or args.url, "--resolve requires --url")
            report = fetch_attestation(args.url, args.resolve)[0] if args.url else read_json(args.attestation)
            compose = args.compose.read_text(encoding="utf-8") if args.compose else None
            measurements, app_compose, docker_compose = extract(report, args.release, compose)
            args.out.mkdir(parents=True, exist_ok=True)
            for filename, content in (("measurements.json", json_text(measurements)),
                                      ("app_compose.json", app_compose), ("docker-compose.yml", docker_compose)):
                (args.out / filename).write_text(content, encoding="utf-8")
            print(json_text(measurements), end="")
            if args.compose:
                print("Prediction: only compose hash, source pin, and launcher image are recomputed; "
                      "other measurements are carried forward and require post-deploy verification.", file=sys.stderr)
            return 0
        if args.command == "render":
            print(render(read_json(args.measurements)), end="")
            return 0
        return check(args.repo)
    except (OSError, ValueError, KeyError, TypeError, struct.error, requests.RequestException, yaml.YAMLError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
