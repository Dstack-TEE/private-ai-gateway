# Vendored `confidential_verifier`

This is a vendored copy of the `confidential_verifier` Python package from
[`Phala-Network/private-ai-verifier`](https://github.com/Phala-Network/private-ai-verifier).

The gateway's provider-verifier bridge (`scripts/private_ai_provider_verifier.py`) imports this copy
rather than a sibling checkout of the upstream repo. The verification code ships with the gateway and
is ours to edit, so a clean checkout always has the methods the bridge calls. A sibling checkout ties
the gateway to whatever is uncommitted there: `NearAICloudVerifier.verify_gateway_component` once
existed only as an uncommitted edit, and a clean checkout failed with
`'NearAICloudVerifier' object has no attribute 'verify_gateway_component'`.

## Provenance

- **Upstream repo:** `Phala-Network/private-ai-verifier`
- **Baseline commit:** `51c2b5a83d6d753b9a29288e0ed522ab2d65bac4` (branch `master`)
- **Vendored:** the `confidential_verifier/` tree at that commit with the local changes below. The
  baseline includes the `attestation: None` compatibility fix for dstack-verifier >= 0.5.6.

## Local changes on top of the baseline

Keep this list current so we can diff against upstream and re-sync deliberately.

- `verifiers/nearai.py`: added `NearAICloudVerifier.verify_gateway_component(...)`, the gateway-only
  verification entry point the bridge depends on. (Upstream had this only as an uncommitted edit.)
- `verifiers/nearai.py`: `_verify_component` parses `report_data` from the quote bytes with
  `_tdx_report_data_hex` and fails when the nonce, signing address, or `report_data` is missing.
  Upstream read `report_data` from the dstack verifier result, which never contains it, so the
  binding check never ran.
- `providers/nearai.py`: `NearaiProvider` takes an optional `api_key` and sends it as a bearer token
  when fetching the attestation report. It raises an explicit error when the
  first model attestation lacks a non-empty `intel_quote` string.
  NEAR AI report, catalog, and NVIDIA requests use connect and read timeouts.
  The shared NVIDIA client accepts an optional timeout.
- `RedpillProvider` is renamed `RedPillProvider`.

## Re-syncing with upstream

To pull upstream fixes, diff this tree against a fresh checkout at a newer commit, re-apply the local
changes above, update the baseline commit here, and run the bridge contract test
(`cargo test --locked --test contract_verifier_bridge`) plus
`uv run python scripts/live_e2e/run.py --profile quick`.

## Runtime dependencies

Declared in the gateway's `pyproject.toml` so `uv run` resolves them from the gateway project (no
sibling needed): `cryptography`, `dcap-qvl`, `pydantic`, `pyyaml`, `python-dotenv`, `PyJWT`,
`requests`. The package does **not** import `fastapi`/`uvicorn` (those are only used by the upstream
repo's standalone server, which we do not vendor).
