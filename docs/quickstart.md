# ACI Quickstart

Verify a live ACI deployment yourself. The commands below run against
`https://api.redpill.ai`, a live deployment of the reference implementation;
point `ACI_URL` at any ACI service to verify that instead.

You need a Rust toolchain plus `curl`, `jq`, and `openssl`. The `aci` CLI
lives in this repository:

```bash
git clone https://github.com/Dstack-TEE/private-ai-gateway.git
cd private-ai-gateway
export ACI_URL=https://api.redpill.ai
```

## 1. Verify the service with one command

```bash
cargo run --bin aci -- verify "$ACI_URL"
```

The CLI fetches `GET /v1/aci/attestation` with a fresh 32-byte random nonce
and runs the six checks of [aci.md](../spec/aci.md) §9.1. Abridged output:

```text
PASS  id-1         hardware quote verifies to TEE vendor root and binds report_data [9.1(1)] — tdx quote verified (TCB status UpToDate) and binds report_data; collateral from https://pccs.phala.network
PASS  id-2         binding chain: keyset JCS -> digest -> statement for our nonce -> report_data [9.1(2)] — keyset digest sha256:a1b4…c5c6; statement digest for nonce "9b2c…" matches report_data
PASS  id-3         keyset not expired (now < not_after) [9.1(3)] — now 1783899770 < not_after 1786491770
PASS  id-4         source provenance connects workload to public code [9.1(4)] — booted compose measured into RTMR3: compose-hash=7c1e…40db; repo=https://github.com/Dstack-TEE/private-ai-gateway.git commit=58b027d… (published, not independently rebuilt)
SKIP  id-5         private-key custody and subject per policy [9.1(5)] — custody policy not implemented in this CLI yet (see src/aci/verifier/dstack.rs); subject: null (no policy constraints applied)
PASS  id-6         the channel actually used is bound to the attested keyset (TLS SPKI or E2EE key) [9.1(6)] — observed SPKI 6ff3…9d21 for api.redpill.ai is in the attested keyset

VERIFIED (5 pass, 1 skipped: custody policy not implemented)
```

Each line is the status marker, the check id, its title and spec citation,
then a `—` detail. Statuses are `pass`, `fail`, `skip`, or `info`. A skip is
never counted as a pass: the verdict line names each skip and its reason.
Here id-5 is a `skip` — this CLI has no custody policy, so it does not claim
to have checked private-key custody. The exit code is `0` only on `VERIFIED`.

`--nonce` supplies your own nonce; `--json` emits the transcript as
structured data.

What these checks prove and how they compose is [aci.md](../spec/aci.md) §1 (the
trust model) and the §3 trust-chain diagram.

id-4 verifies that the compose the service booted is the one measured into the
quote, and prints the hash. It does not decide whether that compose is one you
want: that is your verifier policy ([aci.md](../spec/aci.md) §1.3). Pin the
hashes you accept with `--accept-compose`, repeatable and available on
`verify`, `send`, `serve` and `audit`:

```bash
cargo run --bin aci -- serve "$ACI_URL" --accept-compose 7c1e...40db
```

For a production deployment, first run a dstack verifier over the report's
quote, event log, and VM configuration. Require it to reproduce the boot
measurements (MRTD and RTMR0-2), establish `os_image_hash`, and return
`is_valid: true`. The [Phala direct verification path](providers/phala-direct/verification.md#how-the-os-image-is-classified)
implements this check. Then appraise that hash with the ACI client's production
allowlist:

```bash
cargo run --bin aci -- verify "$ACI_URL" --require-production-os
```

The ACI client verifies the DCAP quote and replays RTMR3, but does not perform
the dstack boot-measurement reconstruction. `policy-os: pass` therefore means
the RTMR3 `os-image-hash` is allowlisted. Treat it as production-OS evidence
only when the dstack verifier independently bound the same hash to MRTD and
RTMR0-2. Development, missing, and unknown hashes fail the allowlist; accepting
a new image requires a verifier update.

The compose hash is the value to pin because it is the one measured into
RTMR3. `repo_url` and `repo_commit` ride along in the report unpinned: they
are not bound into the quote, so they are a label to read, not evidence.

## 2. Look at the evidence yourself

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

# Which public code is it running?
jq '.attestation.source_provenance' report.json

# Which TLS keys is it pinned to, per hostname?
jq '.tls_public_keys' keyset.json
```

The provenance names the exact source to review:

```json
{
  "repo_url": "https://github.com/Dstack-TEE/private-ai-gateway.git",
  "repo_commit": "58b027d17b582de6b7b2e5c60a04393901d9b31d",
  "image_digest": null,
  "image_provenance": null
}
```

The commit changes when the deployment updates. The E2EE v2 extension suite is
`x25519-aes-256-gcm-hkdf-sha256`; keyset entries with any other `algo` are
ignored ([ACI §3.1](../spec/aci.md#31-workload-keyset),
[E2EE v2 §4](../spec/e2ee-v2.md#4-algorithms)).

To recompute any digest by hand, add `--explain` to `aci verify`: each check
prints the exact material it computed — the decoded keyset bytes, the §3.2
statement bytes, the digests, and the expected values.
[test-vectors.md](../spec/test-vectors.md) pins the same constructions byte for
byte. To re-run the checks against saved artifacts:

```bash
cargo run --bin aci -- audit --report report.json --nonce "$NONCE"
```

## 3. Use it as a local endpoint

```bash
cargo run --bin aci -- serve "$ACI_URL"
```

`aci serve` verifies the service first, prints the transcript, and refuses
to start unless the verdict is `VERIFIED`. It then listens on plain HTTP at
`127.0.0.1:4180` — like a local Ollama — so any OpenAI-compatible client
can use an unencrypted local API. Send plaintext request bodies without E2EE
headers. The proxy rejects E2EE v2 and legacy E2EE request headers with HTTP
400 instead of forwarding them:

```bash
export API_KEY=<your api key>
MODEL=$(curl -sS http://127.0.0.1:4180/v1/models \
  -H "Authorization: Bearer $API_KEY" | jq -r '.data[0].id')

curl -sS http://127.0.0.1:4180/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "content-type: application/json" \
  -d '{"model": "'"$MODEL"'", "messages": [{"role": "user", "content": "Say hi"}]}'
```

What the proxy does:

- Forwards every method and path to the same path on the service, so each
  API surface works unchanged: OpenAI chat completions, completions and
  embeddings, Anthropic `/v1/messages`, OpenAI `/v1/responses`, model
  listings, and `GET /v1/aci/*`. Headers travel in both directions except
  the connection-scoped ones a proxy re-derives. E2EE request headers are
  rejected at the local boundary. The proxy stores nothing and never logs
  bodies.
- Every inference demands verified serving: the proxy sets
  `provider.aci_verified` in the body ([aci.md](../spec/aci.md) §5.3), so an
  aggregator refuses rather than serve you through an unverified upstream.
  `--allow-unverified` drops the demand.
- Every upstream connection enforces the attested TLS SPKI pin for the
  hostname and fails closed on a mismatch.
- Responses stream through byte-exact while the proxy digests the raw wire
  bytes — bodies are never buffered or stored. Each POST response's receipt
  id and body digests are recorded (the last 256 exchanges), and a 2xx
  inference response with no receipt header is flagged immediately
  ([aci.md](../spec/aci.md) §5.2).
- Verification runs on demand from the control endpoint on
  `127.0.0.1:4181`, not per request:

  ```bash
  curl -sS http://127.0.0.1:4181/receipts        # recent exchanges
  curl -sS -X POST http://127.0.0.1:4181/receipts/<receipt-id>/verify \
    -H "Authorization: Bearer $API_KEY"          # if the receipt fetch needs it
  ```

  This fetches the receipt and its cited session from the service and runs
  the full [aci.md](../spec/aci.md) §9.3 + §9.2 checks against the recorded
  digests — signature, keyset binding, body hashes, session audit. The
  verdict returns as JSON and prints on the proxy console, loud on failure.
- If a response carries a different `X-ACI-Keyset-Digest` than the verified
  one, forwarding blocks until a fresh verify passes.

To go from trusting the service's own gating to pinning the exact sessions
you accept, first audit the current attested sessions:

```bash
cargo run --bin aci -- sessions "$ACI_URL" --require-claim tee_attested=hardware_proven
```

Each current session record is fetched and audited
([aci.md](../spec/aci.md) §9.2), and the ids that pass the audit and the
claims policy print as `ACCEPTED`. Then pin, either way:

```bash
# Fixed accepted set: requests use its intersection with their own pins, or
# this set when they supply none. A disjoint request fails locally.
cargo run --bin aci -- serve "$ACI_URL" --session <session-id>

# Policy pins: derive the set from the required claims. Refuses to start if
# nothing qualifies, and refreshes the set when the service refuses a
# superseded pin (HTTP 412) before retrying the request once.
cargo run --bin aci -- serve "$ACI_URL" --require-claim tee_attested=hardware_proven
```

A request that already carries `provider.aci_session_ids` is narrowed to its
intersection with the local accepted set. On-demand receipt verification also checks the cited session
against the pins (§9.3(6)) and the required claims (§9.2(3)).

## 4. Verify one inference end to end

```bash
export ACI_API_KEY=<your api key>
cargo run --bin aci -- send "$ACI_URL" --prompt "What are you running on?"
```

`aci send` verifies the service (fail closed), sends one chat completion
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

## 5. Verify from a browser or any web app

The [`@phala/aci-verifier`](../clients/verifier-ts) library verifies a service
from a browser tab or any web project in one call:

```ts
import { verifyService } from '@phala/aci-verifier';
const { verdict, lines } = await verifyService('https://api.redpill.ai');
console.log(verdict.line); // VERIFIED / PARTIAL / NOT VERIFIED
```

It fetches the report with a fresh nonce and verifies the hardware quote
(via [`@phala/dcap-qvl`](https://www.npmjs.com/package/@phala/dcap-qvl) against
the Phala PCCS), the binding chain, and the compose measurement — the same
§9.1 checks the CLI runs, except key custody (check 5) and the TLS-certificate
pin (check 6), which a plain browser cannot reach. A prebuilt ESM bundle
(`npm run build:bundle`) drops into a `<script type="module">` with no build
step.

## 6. Going deeper

[README.md](../spec/README.md) routes the rest by task. [aci.md](../spec/aci.md) §9 is the
procedure this walkthrough exercised.
