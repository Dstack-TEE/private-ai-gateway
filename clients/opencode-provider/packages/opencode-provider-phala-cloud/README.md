# opencode-provider-phala-cloud

Phala Cloud branded [opencode](https://opencode.ai) provider plugin for
[private-ai-gateway](https://github.com/Dstack-TEE/private-ai-gateway) (ACI):
attested TLS (SPKI) pinning + per-response receipt verification.

Thin skin over [`@phala/opencode-provider-aci`](../opencode-provider-aci) —
all protocol logic lives in the core.

## Install

`opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["opencode-provider-phala-cloud"]
}
```

## Login

Device-flow login (mints a Redpill LLM virtual key; no `phak_` cloud token):

```bash
opencode auth login   # choose Phala Cloud
```

Or set an API key directly:

```bash
export PHALA_LLM_API_KEY=...
```

Then select a `phala/<model-id>` model.

## Configuration

Plugin options:

```json
{
  "plugin": [["opencode-provider-phala-cloud", {
    "isTeeOnly": true,
    "failOpenOnUnpinned": false,
    "pinning": true
  }]]
}
```

Env vars: `PHALA_BASE_URL` (aliases `PHALA_CLOUD_API_PREFIX`,
`PHALA_CLOUD_BASE_URL`), `PHALA_IS_TEE_ONLY`, `PHALA_MODEL_ALLOWLIST`,
`PHALA_AUTO_FETCH_RECEIPT`, `PHALA_REQUIRE_ATTESTATION_MATCH`,
`PHALA_FAIL_OPEN_ON_UNPINNED`, `PHALA_PINNING`.

Status: after each response the verification result
(`verified` / `verified*` / `routed` / `attested` / `mismatch` + TLS pin
state) is written to the opencode log and shown as a TUI toast. The
`phala_verification_status` tool returns the structured status;
`phala_settings` shows and toggles runtime settings (pinning, fail-open,
receipt auto-verify). Set `PHALA_DEBUG=1` to write exchange/status trace
lines to stderr (useful with `opencode run`; in headless mode the process can
exit before the async log/toast publish lands).
