# Gateway release measurements

Production deployment releases use `gateway-prod-YYYY-MM-DD` tags (with a `-N` suffix for a second
release on the same date) in
[Dstack-TEE/private-ai-gateway](https://github.com/Dstack-TEE/private-ai-gateway/releases).
Each release carries `measurements.json`, `app_compose.json`, and
`docker-compose.yml` assets. The JSON asset is the source of release measurement
values for release notes, documentation, and the live drift monitor. Older
releases may lack it; the monitor fails clearly when a required asset is missing.

## Extract and render

Use the repository's locked Python environment:

```sh
uv sync --locked
uv run --locked python scripts/gateway_measurements.py extract \
  --url https://tee.redpill.ai --release gateway-prod-YYYY-MM-DD \
  --out /tmp/gateway-release
uv run --locked python scripts/gateway_measurements.py render \
  /tmp/gateway-release/measurements.json
```

Replace the release date with the deployment's release tag. `extract` writes
the three assets to `--out` (default: the current directory) and prints the
measurements JSON. `render` prints a Markdown values table. A maintainer reviews
the assets and attaches them to the matching release; the script does not upload
or publish releases.

For captured public evidence, replace `--url` with `--attestation report.json`.
For a specific replica, add `--resolve tee.redpill.ai:IP_ADDRESS` to `--url`.
This connects directly, bypassing environment proxies, while retaining the
hostname for HTTP, TLS SNI, and certificate validation.

`extract` replays every event digest into RTMR0–3 and checks the result against
the TDX v4 quote. It checks the selected boot-event payload digests, requires
`sha256(app_compose)` to equal the pre-`system-ready` compose-hash event, and
matches the OS image hash to its boot event. It reads the source pin from the
launcher's measured Compose environment and checks the report's
`source_provenance` against it. The launcher image must be pinned by digest.

This tool checks measurement consistency over HTTPS. It does not verify the
quote signature, collateral, TCB status, ACI nonce binding, or TLS key custody.
Approving a release still requires the
[deployment verification policy](../deploy/README.md#verify-the-deployment).
VM image labels and CPU and memory sizes are taken from the report; this tool
does not independently authenticate their hardware binding. The live check
compares the peer certificate's SPKI on the attestation HTTPS connection with
the matching release snapshot's TLS fingerprint. This establishes the served
key's identity; key custody remains part of deployment review.

## Asset schema

`schema` is `gateway-measurements/v1`; `release` identifies the release tag;
`domains` lists every monitored hostname. Hashes and public keys use hex.

| Object | Fields |
| --- | --- |
| `app` | `compose_hash` (SHA-256), `app_id`, `repo_url`, `repo_commit`, `launcher_image` |
| `kms` | `root_key` (the public `id` in the measured `key-provider` event) |
| `os` | `image`, `image_hash` (SHA-256), `mrtd`, `rtmr0`, `rtmr1`, `rtmr2` (SHA-384 measurements), `vcpus`, `memory_bytes` |
| `tls` | Map of domain to leaf certificate SPKI SHA-256 |

RTMR3 is replayed during extraction but is omitted from the stable schema:
the asset records its relevant application, OS image, and KMS root identities.

To prepare a compose change before rollout:

```sh
uv run --locked python scripts/gateway_measurements.py extract \
  --attestation report.json --compose new-docker-compose.yml \
  --release gateway-prod-YYYY-MM-DD --out /tmp/gateway-scheduled
```

The tool first validates the captured report, replaces `docker_compose_file`,
then serializes the app compose as compact, key-sorted JSON with Unicode
preserved. It recomputes the compose hash and reads the new launcher image and
source pin. All other measurements are carried forward. Review that assumption
before using a prediction; OS, app identity, KMS, or TLS changes require their
own reviewed values and post-deploy verification. Re-extract after rollout.

## Live drift monitor

```sh
uv run --locked python scripts/gateway_measurements.py check
# Optional: --repo owner/repo (defaults to Dstack-TEE/private-ai-gateway)
```

The check selects the newest published production release by `published_at`
and open production prereleases published after that release. Stale prereleases
cannot extend acceptance of an earlier deployment. It loads each `measurements.json`, then
fetches public `/v1/aci/attestation` evidence from every domain in those assets.
Each domain must match one complete accepted snapshot, comparing every schema
field except `release`; domain ordering is ignored. The SPKI observed in the TLS
handshake on that same connection must also match that snapshot's value for
the domain. Missing peer-certificate evidence fails closed. A rollout can serve the old
snapshot on one domain and a prerelease snapshot on another. Individual fields
from different releases cannot be combined to pass. Publishing the new release
automatically ends the acceptance window for older prereleases.

A mismatch prints a unified JSON diff and exits 1. Missing assets, malformed
evidence, and network failures also fail the check. `GITHUB_TOKEN` is optional
locally and supplies authenticated GitHub API quota in CI.

The [Gateway measurements workflow](../.github/workflows/gateway-measurements.yml)
runs hourly and on manual dispatch. It opens or updates one
`measurement-drift` issue on failure and closes it on recovery. It uses only
`contents: read` and `issues: write` permissions. Scheduled checks start once
the workflow is on the default branch; neither the workflow nor the script
deploys the gateway or changes release assets.
