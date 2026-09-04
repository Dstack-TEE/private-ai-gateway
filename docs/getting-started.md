# Local development

This tutorial is for contributors who want to compile the gateway, exercise its
attestation surface, and run the local integration suite. The production binary
always uses dstack KMS keys and dstack TDX quotes, so even a local process needs
a reachable dstack SDK endpoint.

## Prerequisites

Install:

- Rust stable with `rustfmt` and `clippy`
- Python 3.12
- [uv](https://docs.astral.sh/uv/)
- `curl`, `jq`, and `openssl`
- Node.js 24, npm, and Bun 1.4 when changing the TypeScript clients
- the Phala CLI and SSH access to a dev OS CVM when running outside dstack

You also need a dstack SDK endpoint. A dstack CVM exposes the SDK on
`/var/run/dstack.sock`. For local work, forward that socket to a local Unix
socket and set `dstack_endpoint` to the forwarded path.

The default paths used by the test scripts are:

```text
dstack endpoint: unix:/tmp/aci-dstack-sock-dev.dstack.sock
dstack verifier: http://localhost:18080
live artifacts:  /tmp/private-ai-gateway-live-e2e
```

The external dstack verifier is needed only by provider adapters that declare it
in their provider configuration, such as NEAR AI and PhalaDirect. The gateway's
own identity path talks to the dstack SDK socket directly.

## Connect to the dstack SDK

Inside a dstack CVM, use the socket at `/var/run/dstack.sock`. For local
development, forward that socket from a dev OS CVM over SSH. Set a CVM name you
can access, then keep this command running in its own terminal:

```bash
DSTACK_DEV_CVM=your-dev-cvm-name
DSTACK_LOCAL_SOCKET=/tmp/aci-dstack-sock-dev.dstack.sock

phala status
phala cvms get "$DSTACK_DEV_CVM" --json
phala ssh "$DSTACK_DEV_CVM" -- \
  -N -L "$DSTACK_LOCAL_SOCKET:/var/run/dstack.sock"
```

The local socket path must not already belong to another process or stale
tunnel. In a second terminal, verify the forwarded API before starting the
gateway:

```bash
DSTACK_LOCAL_SOCKET=/tmp/aci-dstack-sock-dev.dstack.sock

test -S "$DSTACK_LOCAL_SOCKET"
curl --fail --silent --show-error \
  --unix-socket "$DSTACK_LOCAL_SOCKET" \
  http://dstack/Info \
  | jq
```

The quote, key derivation, application identity, KMS behavior, and event log
come from the remote CVM. A local process using this socket is suitable for API
and binding tests, but its measurements are the remote dev CVM's measurements,
not those of the intended production deployment.

## Install dependencies

From the repository root:

```bash
uv sync --locked
cargo build --all-targets
```

`uv sync --locked` installs the Python packages used by the provider-verifier
bridge. The bridge uses the vendored `scripts/confidential_verifier` package by
default. Set `PRIVATE_AI_VERIFIER_DIR` only when testing an intentional external
verifier checkout.

## Run the repository checks

Run the same checks as the main CI workflow:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
python3 -m compileall scripts
```

The TypeScript verifier, shared provider, and Pi and OpenCode adapters use one
workspace. Run these commands from the repository root after changing anything
under `clients/` or a shared ACI construction:

```bash
npm --prefix clients ci
npm --prefix clients run build
npm --prefix clients run check
npm --prefix clients test
npm --prefix clients run test:bun
npm --prefix clients run lint
npm --prefix clients run format:check
npm --prefix clients run lint:packages
bash clients/package-smoke.sh
```

## Start the gateway without inference routes

Create a writable state directory and a static config:

```bash
mkdir -p /tmp/private-ai-gateway-state

cat >/tmp/private-ai-gateway.config.json <<'JSON'
{
  "bind": "127.0.0.1:8086",
  "state_dir": "/tmp/private-ai-gateway-state",
  "dstack_endpoint": "unix:/tmp/aci-dstack-sock-dev.dstack.sock"
}
JSON
```

Start the binary:

```bash
PRIVATE_AI_GATEWAY_CONFIG_PATH=/tmp/private-ai-gateway.config.json \
  RUST_LOG=info \
  cargo run --bin private-ai-gateway
```

Startup performs these operations before binding the HTTP listener:

1. Requests receipt-signing and E2EE keys from dstack KMS.
2. Opens the gateway state files and takes the session-log writer lock.
3. Loads `<state_dir>/upstreams.json`. A missing or empty file means no routes.
4. Seals a workload keyset with the configured `not_after` lifetime.
5. Compacts the session log.

The process exits on a KMS, config, state, or session-log error.

## Inspect the attested keyset surface

In a second terminal:

```bash
curl --fail --silent --show-error http://127.0.0.1:8086/health

NONCE="$(openssl rand -hex 32)"
curl --fail --silent --show-error \
  "http://127.0.0.1:8086/v1/aci/attestation?nonce=$NONCE" \
  | jq '{api_version, workload_keyset_digest,
         subject: .attestation.workload_keyset.subject}'
```

Expected health response:

```json
{"status":"ok"}
```

The attestation response should report `api_version` as `aci/1`. This check
only confirms that the endpoint returned a report. A relying party still needs
to verify the report as described in the
[verification guide](attested-confidential-inference.md).

## Add an upstream route

The active route file is `<state_dir>/upstreams.json`. For a first direct-mode
test, stop the gateway and create a route that matches an endpoint you control:

```json
[
  {
    "name": "local-openai",
    "provider": "openai-compatible",
    "base_url": "http://127.0.0.1:9000",
    "models": {
      "local-model": "upstream-model"
    }
  }
]
```

Restart the gateway, then inspect the public model catalog:

```bash
curl --fail --silent --show-error http://127.0.0.1:8086/v1/models | jq
```

`openai-compatible` routes have no attestation verifier and cannot satisfy
`provider.aci_verified: true`. Use them for protocol and transformation tests,
not confidential-inference claims.

For live providers, start from [`deploy/upstreams.example.json`](../deploy/upstreams.example.json)
and follow the relevant [provider verification guide](providers/README.md).

## Run the local multi-upstream smoke test

The local smoke test builds containers for two mock ACI services and one router
gateway. All three use the forwarded dstack socket.

Install these additional tools:

- Docker with the Compose plugin
- `awk` and `sha256sum`

Run:

```bash
DSTACK_SOCK=/tmp/aci-dstack-sock-dev.dstack.sock \
  scripts/local_multi_upstream_smoke.sh
```

The script checks model routing, TLS-bound upstream verification, chat and
embedding receipts, attested sessions, dynamic config, and metrics. It removes
its Compose stack when it finishes unless `KEEP_STACK=1` is set.

See the [testing guide](live-e2e-test-suite.md) for live-provider profiles and
artifact locations.

## Common startup failures

| Error | Cause | Fix |
| --- | --- | --- |
| `PRIVATE_AI_GATEWAY_CONFIG_PATH must point to...` | The required config-path variable is missing or empty. | Set it to a readable JSON file. |
| dstack KMS or quote request failed | The dstack endpoint is missing, stale, or not reachable by the process. | Check the forwarded socket and the `dstack_endpoint` value. |
| `invalid upstream config` | `upstreams.json` or the configured seed violates the schema. | Check the field and provider constraints in the [configuration reference](configuration-reference.md#upstream-fields). |
| another gateway holds the session log lock | Two processes use the same `state_dir`. | Give each local process a separate state directory or stop the other writer. |
| `address already in use` | Another process owns the configured listener. | Change `bind` or stop the existing process. |
