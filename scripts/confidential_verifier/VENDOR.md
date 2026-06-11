# Vendored `confidential_verifier`

This is a vendored copy of the `confidential_verifier` Python package from
[`Phala-Network/private-ai-verifier`](https://github.com/Phala-Network/private-ai-verifier).

The gateway's provider-verifier bridge (`scripts/private_ai_provider_verifier.py`) used to import
this package from a sibling checkout resolved via `PRIVATE_AI_VERIFIER_DIR`. That coupled the gateway
to a separately-versioned repo — and, worse, to whatever happened to be *uncommitted* in that sibling
checkout. A real example: the bridge calls `NearAICloudVerifier.verify_gateway_component`, which only
ever existed as an uncommitted local edit, so a clean checkout failed with
`'NearAICloudVerifier' object has no attribute 'verify_gateway_component'`. Vendoring removes that
whole class of drift: the verification code now ships with the gateway and is ours to edit.

## Provenance

- **Upstream repo:** `Phala-Network/private-ai-verifier`
- **Baseline commit:** `2d2f5cf2281ec65ed43a3d3f44fcbcf0239952b7` (branch `master`)
- **Vendored:** the pristine committed `confidential_verifier/` tree at that commit.

## Local changes on top of the baseline

Keep this list current so we can diff against upstream and re-sync deliberately.

- `verifiers/nearai.py`: added `NearAICloudVerifier.verify_gateway_component(...)` — the gateway-only
  verification entry point the bridge depends on. (Upstream had this only as an uncommitted edit.)
- `verifiers/dstack.py`: `DstackVerifier.verify` sends `"attestation": None` in the `/verify` body.
  dstack-verifier >= 0.5.6 serializes the request with `serde_human_bytes` and no field default, so the
  optional `attestation` field must be present even when unused. Older verifiers ignore the extra key,
  so this is backward compatible. Required to verify dstack >= 0.5.6 CVMs (e.g. OS image 0.5.6/0.5.8).

## Re-syncing with upstream

To pull upstream fixes, diff this tree against a fresh checkout at a newer commit, re-apply the local
changes above, update the baseline commit here, and run the bridge contract test
(`tests/contract_verifier_bridge.py`) plus `run.py --profile quick`.

## Runtime dependencies

Declared in the gateway's `pyproject.toml` so `uv run` resolves them from the gateway project (no
sibling needed): `cryptography`, `dcap-qvl`, `pydantic`, `pyyaml`, `python-dotenv`, `PyJWT`,
`requests`. The package does **not** import `fastapi`/`uvicorn` (those are only used by the upstream
repo's standalone server, which we do not vendor).
