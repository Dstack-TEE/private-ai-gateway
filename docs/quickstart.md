# ACI Quickstart

Verify a live ACI deployment yourself. The commands below run against
`https://tee.redpill.ai`, a live deployment of the reference implementation
that enforces TEE-only routing. Point `ACI_URL` at any ACI service to verify
that instead.

You need `pap`, `curl`, `jq`, and `openssl`. Install Private AI Proxy from npm
and select the service:

```bash
npm install --global private-ai-proxy@beta
pap --help
export ACI_URL=https://tee.redpill.ai
```

This guide uses the 0.2 beta, which adds `pap curl` and the custody flags.
`private-ai-proxy` is the full name of the same command. For Homebrew, native
installers, and install scripts, see the [install guide](private-ai-proxy-install.md).
From a source checkout, replace each `pap` command below with
`cargo run --manifest-path apps/desktop/Cargo.toml --bin private-ai-proxy --`.

## 1. Verify the service with one command

```bash
pap verify "$ACI_URL"
```

The CLI fetches `GET /v1/aci/attestation` with a fresh 32-byte random nonce
and runs the six checks of [aci.md](../spec/aci.md) §9.1. Abridged output:

```text
PASS  id-1         hardware quote verifies to TEE vendor root and binds report_data [9.1(1)] — tdx quote verified (TCB status UpToDate) and binds report_data; collateral from https://pccs.phala.network
PASS  id-2         binding chain: keyset JCS -> digest -> statement for our nonce -> report_data [9.1(2)] — keyset digest sha256:a1b4…c5c6; statement digest for nonce "9b2c…" matches report_data
PASS  id-3         keyset not expired (now < not_after) [9.1(3)] — now 1783899770 < not_after 1786491770
PASS  id-4         source provenance connects workload to public code [9.1(4)] — compose-hash=7c1e…40db measured into RTMR3; repo=https://github.com/Dstack-TEE/private-ai-gateway.git commit=58b027d… (published, not rebuilt)
SKIP  id-5         private-key custody and subject per policy [9.1(5)] — no custody policy configured (--accept-dstack-kms-root-public-key with --accept-subject); subject: null (no policy constraints applied)
PASS  id-6         the channel actually used is bound to the attested keyset (TLS SPKI or E2EE key) [9.1(6)] — observed SPKI 6ff3…9d21 for https://tee.redpill.ai is an attested entry clients pin

VERIFIED (5 pass, 1 skipped: no custody policy configured)
```

Each line shows the status, the check ID, its title and spec section, then
the detail. A status is `PASS`, `FAIL`, `SKIP`, or `INFO`. A skip never counts
as a pass, and the verdict line names each skip. The exit code is `0` only on
`VERIFIED`.

`--nonce` supplies your own nonce, and `--json` prints the transcript as
structured data. [aci.md](../spec/aci.md) §1 explains what the checks prove
together.

### Choose what you accept

A passing run proves that genuine TEE hardware booted the compose with hash
`7c1e…40db`. Whether you accept that compose is your decision;
[The privacy claim](attested-confidential-inference.md#the-privacy-claim)
explains the options. This section shows the commands.

To pin, pass the full compose hash from your run. `--accept-compose` is
repeatable and works with `verify`, `curl`, `send`, `serve`, `sessions`, and
`audit`:

```bash
pap verify "$ACI_URL" --accept-compose <full-compose-hash>
```

The compose hash covers the complete compose file, so it identifies what the
deployment runs. In the [reference deployment](../deploy/README.md), that file
names the gateway commit. To read the commit from the measured compose rather
than from the gateway's own `source_provenance` statement:

```bash
curl -sS "$ACI_URL/v1/aci/attestation?nonce=$(openssl rand -hex 32)" \
  | jq -r '.attestation.evidence.app_compose | fromjson | .docker_compose_file' \
  | grep -E 'REPO_URL|COMMIT_SHA'
```

To check custody of the receipt-signing key (id-5), add the measured app ID
and the dstack KMS root key you trust. The app ID is the `app-id` event in the
report's RTMR3 event log:

```bash
curl -sS "$ACI_URL/v1/aci/attestation?nonce=$(openssl rand -hex 32)" \
  | jq -r '.attestation.evidence.event_log | fromjson
           | map(select(.imr == 3 and .event == "app-id")) | .[0].event_payload'

pap verify "$ACI_URL" \
  --accept-subject app-id:0x<app-id> \
  --accept-dstack-kms-root-public-key <kms-root-public-key>
```

id-5 then requires the receipt key's KMS signature chain to end at that root
for that app. It does not cover the TLS private key. That key stays inside the
TEE only if the compose you reviewed runs the TLS terminator and keeps its key
there.

`--require-production-os` also requires the RTMR3 `os-image-hash` to be on the
CLI's allowlist of reviewed production images. It does not replace a dstack
verifier run; see
[Build a verifier policy](attested-confidential-inference.md#build-a-verifier-policy).

## 2. Send a familiar API request over the verified channel

Replace `YOUR_API_KEY` and `MODEL_ID` with values from your provider:

```bash
pap curl "$ACI_URL/v1/chat/completions" -- \
  --fail-with-body \
  --no-buffer \
  --header "Authorization: Bearer YOUR_API_KEY" \
  --header "content-type: application/json" \
  --data-binary '{
    "model": "MODEL_ID",
    "messages": [{"role": "user", "content": "Say hi"}],
    "stream": true,
    "provider": {"aci_verified": true}
  }'
```

`pap curl` first runs the checks from step 1 under the same policy flags. If
they pass, it starts the installed curl pinned to the TLS key the report
declares. Verification output goes to stderr. The API response goes to stdout
unchanged, including SSE streaming.

The pin covers the connection from you to the gateway. The
`provider.aci_verified: true` field makes the gateway refuse the request
unless the chosen model backend also passes verification. `pap curl` does not
fetch or verify the response receipt. Use `pap send` or `pap serve` for that.

Put curl arguments after `--`. The
[CLI reference](../apps/desktop/docs/cli.md#one-pinned-curl-request) lists the
accepted options and exit codes.

## 3. Look at the evidence yourself

The report is plain JSON, keyset included: `attestation.workload_keyset`
is the keyset object itself, and its digest is over the keyset's JCS form
([aci.md](../spec/aci.md) §3.1).

```bash
NONCE=$(openssl rand -hex 32)
curl -sS "$ACI_URL/v1/aci/attestation?nonce=$NONCE" -o report.json

# The attested keyset, as served.
jq '.attestation.workload_keyset' report.json > keyset.json

# Which keys may this workload use, and until when?
jq '{subject, not_after,
     receipt_keys: [.receipt_signing_keys[] | {key_id, algo}],
     e2ee_suites:  [.e2ee_public_keys[].algo]}' keyset.json

# Which public code does the gateway say it runs?
jq '.attestation.source_provenance' report.json

# Which TLS keys is it pinned to, per hostname?
jq '.tls_public_keys' keyset.json
```

The gateway fills `source_provenance` from its launcher config:

```json
{
  "repo_url": "https://github.com/Dstack-TEE/private-ai-gateway.git",
  "repo_commit": "58b027d17b582de6b7b2e5c60a04393901d9b31d",
  "image_digest": null,
  "image_provenance": null
}
```

This field is the gateway's own statement. Check it against the commit in the
measured compose, as shown in [Choose what you accept](#choose-what-you-accept).
The E2EE v2 extension suite is
`x25519-aes-256-gcm-hkdf-sha256`; keyset entries with any other `algo` are
ignored ([ACI §3.1](../spec/aci.md#31-workload-keyset),
[E2EE v2 §4](../spec/e2ee-v2.md#4-algorithms)).

To recompute any digest by hand, add `--explain` to `pap verify`. Each check
then prints the exact material it computed: the decoded keyset bytes, the §3.2
statement bytes, the digests, and the expected values.
[test-vectors.md](../spec/test-vectors.md) pins the same constructions byte for
byte. To re-run the checks against saved artifacts:

```bash
pap audit --report report.json --nonce "$NONCE"
```

To audit a saved exchange as well, add the receipt, the exact request and
response bytes, and the session the receipt cites:

```bash
pap audit --report report.json --nonce "$NONCE" \
  --receipt receipt.json \
  --request-body request.bin \
  --response-body response.bin \
  --session session.json
```

## 4. Use it as a local endpoint

```bash
pap serve "$ACI_URL"
```

`pap serve` verifies the service first, prints the transcript, and refuses
to start unless the verdict is `VERIFIED`. It then listens on plain HTTP at
`127.0.0.1:4180`, like a local Ollama, so any OpenAI-compatible client can
use it as an unencrypted local API. Send plaintext request bodies without E2EE
headers:

```bash
export API_KEY=<your api key>
MODEL=$(curl -sS http://127.0.0.1:4180/v1/models \
  -H "Authorization: Bearer $API_KEY" | jq -r '.data[0].id')

curl -sS http://127.0.0.1:4180/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "content-type: application/json" \
  -d '{"model": "'"$MODEL"'", "messages": [{"role": "user", "content": "Say hi"}]}'
```

The proxy pins every upstream connection to the attested TLS key, adds
`provider.aci_verified` to each inference request, and records each response's
receipt for audit. Check a recorded exchange through the control endpoint:

```bash
curl -sS http://127.0.0.1:4183/receipts        # recent exchanges
curl -sS -X POST http://127.0.0.1:4183/receipts/<receipt-id>/verify \
  -H "Authorization: Bearer $API_KEY"          # if the receipt fetch needs it
```

[Local verifying proxy](../apps/desktop/docs/cli.md#local-verifying-proxy)
describes everything else it does.

To go from trusting the service's own gating to pinning the exact sessions
you accept, first audit the current attested sessions:

```bash
pap sessions "$ACI_URL" --require-claim tee_attested=hardware_proven
```

Each current session record is fetched and audited
([aci.md](../spec/aci.md) §9.2), and the ids that pass the audit and the
claims policy print as `ACCEPTED`. Then pin, either way:

```bash
# A fixed set of accepted sessions:
pap serve "$ACI_URL" --session <session-id>

# Or the sessions that satisfy the required claims:
pap serve "$ACI_URL" --require-claim tee_attested=hardware_proven
```

[Local verifying proxy](../apps/desktop/docs/cli.md#local-verifying-proxy)
explains how each pin set combines with a request's own pins.

## 5. Verify one inference end to end

```bash
export ACI_API_KEY=<your api key>
pap send "$ACI_URL" --prompt "What are you running on?"
```

`pap send` verifies the service (fail closed), sends one chat completion
over an SPKI-pinned connection while capturing the exact wire bytes, then
fetches and verifies the receipt. This step needs an API key because
receipts are bound to the credential that made the request
([aci.md](../spec/aci.md) §7.6). After the response text, the receipt transcript:

```text
PASS  receipt-1    document signature (JCS, minus signature member) under attested receipt key [9.3(1)]
PASS  receipt-2    document workload_keyset_digest matches established digest [9.3(2)]
PASS  receipt-3    request.received body_hash matches sent bytes [9.3(3)]
PASS  receipt-4    response.returned body_hash matches received wire bytes [9.3(4)]
PASS  upstream-1   upstream.verified result is verified and cites a session [9.3(5)]
PASS  upstream-2   session deep audit: document hashes to cited id, served_at in window, evidence digest [9.2(1-2), 9.3(6)]
```

If the service rewrote the request before inference, an `INFO receipt-note` line
reports the differing `request.forwarded` hash; whether a rewrite is
acceptable is your policy ([aci.md](../spec/aci.md) §9.3).

Flags: `--model` selects a model (default: the first entry of `/v1/models`),
`--no-stream` requests a buffered response, and `--json` emits everything as
structured data. Verified serving is demanded by default (the §5.3
`aci_verified` constraint; `--allow-unverified` drops it). `--session <id>`
(repeatable) pins the exact sessions you accept: the service refuses to
serve through anything else, and the transcript checks that the receipt
cites one of yours.

With the default constraint, a missing `X-Receipt-Id` is a failure. With
`--allow-unverified`, an unconstrained stream may have been committed before an
upstream was selected; in that case `pap send` reads the response `id` and uses
it to fetch the finalized receipt.

## 6. Verify from TypeScript

Install the public ESM package:

```bash
npm install @phala/aci-verifier
```

The browser entry verifies a service in one call:

```ts
import { verifyService } from '@phala/aci-verifier';
const { verdict, lines } = await verifyService('https://tee.redpill.ai');
console.log(verdict.line); // VERIFIED / PARTIAL / NOT VERIFIED
```

It fetches the report with a fresh nonce and verifies the hardware quote
(via [`@phala/dcap-qvl`](https://www.npmjs.com/package/@phala/dcap-qvl) against
the Phala PCCS), the binding chain, and the compose measurement. These are the
§9.1 checks the CLI runs, except key custody (check 5) and the TLS-certificate
pin (check 6), which a plain browser cannot reach.

For a pinned fetch transport in Node or Bun, and for receipt verification, see
the [TypeScript client guide](../clients/verifier-ts/README.md).

## 7. Going deeper

[README.md](../spec/README.md) routes the rest by task. [aci.md](../spec/aci.md) §9 is the
procedure this walkthrough exercised.
