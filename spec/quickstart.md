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
and runs the six checks of [aci.md](aci.md) §10.1. Abridged output:

```text
PASS  L2.1   hardware quote verifies to TEE vendor root and binds report_data [10.1(1)] — tdx quote verified (TCB status UpToDate) and binds report_data; collateral from https://pccs.phala.network
PASS  L2.2   binding chain: keyset bytes -> digest -> statement for our nonce -> report_data [10.1(2)] — keyset digest sha256:a1b4…c5c6; statement digest for nonce "9b2c…" matches report_data
PASS  L2.3   keyset not expired (now < not_after) [10.1(3)] — now 1783899770 < not_after 1786491770
PASS  L2.4   source provenance connects workload to public code [10.1(4)] — presence check only: repo_url=https://github.com/Dstack-TEE/private-ai-gateway.git repo_commit=58b027d… (published, not independently verified against a build)
SKIP  L2.5   private-key custody and subject per profile [10.1(5)] — custody profile not implemented in this CLI yet (see src/aci/verifier/dstack.rs); subject: null (no profile constraints applied)
PASS  L2.6   the channel actually used is bound to the attested keyset (TLS SPKI or E2EE key) [10.1(6)] — observed SPKI 6ff3…9d21 for api.redpill.ai is in the attested keyset

VERIFIED (5 pass, 1 skipped: custody profile not implemented)
```

Each line is the uppercase status marker, the check id, its title and spec
citation, then a `—` detail. Statuses are `pass`, `fail`, `skip`, or `info`.
A skipped check is never counted as a pass: the verdict line names each skip
and its reason. Here L2.5 is a `skip` — this CLI has no custody profile, so
it does not claim to have checked private-key custody. The exit code is `0`
only on `VERIFIED`. `--nonce` supplies your own nonce; `--json` emits the
transcript as structured data.

What these checks prove and how they compose is [aci.md](aci.md) §1 (the
trust model) and the §4 trust-chain diagram.

## 2. Look at the evidence yourself

The report is plain JSON; nothing stops you from checking it by hand. The
keyset travels as `workload_keyset_b64` — the base64 of the exact keyset
bytes, because the digest bound into the quote is over those bytes
([aci.md](aci.md) §4.1).

```bash
NONCE=$(openssl rand -hex 32)
curl -sS "$ACI_URL/v1/aci/attestation?nonce=$NONCE" -o report.json

# Decode the attested keyset once.
jq -r '.attestation.workload_keyset_b64' report.json | base64 -d > keyset.json

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

The commit changes when the deployment updates. The ACI E2EE suite is
`x25519-aes-256-gcm-hkdf-sha256`; keyset entries with any other `algo` are
ignored ([aci.md](aci.md) §4.1, §7.1).

To recompute any digest by hand, add `--explain` to `aci verify`: each check
prints the exact material it computed — the decoded keyset bytes, the §4.2
statement bytes, the digests, and the expected values.
[test-vectors.md](test-vectors.md) pins the same constructions byte for
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
works unchanged:

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

- Forwards `POST /v1/chat/completions`, `/v1/completions`, `/v1/embeddings`
  and `GET /v1/models`, and passes `GET /v1/aci/*` through. Your API key is
  forwarded unchanged; the proxy stores nothing and never logs bodies.
- Every upstream connection enforces the attested TLS SPKI pin for the
  hostname and fails closed on a mismatch.
- Streaming responses pass through byte-exact while the proxy tees the raw
  wire bytes for hashing.
- After each inference response it fetches the receipt using that request's
  own `Authorization`, verifies it (signature, keyset binding, body hashes,
  plus the `upstream.verified` shallow audit), and prints a one-line
  verdict. A failure at this point is detect-and-alert: logged loudly,
  serving continues.
- If a response carries a different `X-ACI-Keyset-Digest` than the verified
  one, forwarding blocks until a fresh verify passes.

## 4. Verify one inference end to end

```bash
export ACI_API_KEY=<your api key>
cargo run --bin aci -- chat "$ACI_URL" --prompt "What are you running on?"
```

`aci chat` verifies the service (fail closed), sends one chat completion
over an SPKI-pinned connection while capturing the exact wire bytes, then
fetches and verifies the receipt. This step needs an API key because
receipts are bound to the credential that made the request
([aci.md](aci.md) §8.6). After the response text, the receipt transcript:

```text
PASS  R.1    envelope signature over payload bytes under attested receipt key [10.2(1)]
PASS  R.2    payload workload_keyset_digest matches established digest [10.2(2)]
PASS  R.3    request.received body_hash matches sent bytes [10.2(3)]
PASS  R.4    response.returned body_hash matches received wire bytes [10.2(4)]
PASS  U.1    upstream.verified result is verified and cites a session [10.3(1)]
PASS  U.2    session deep audit: served bytes hash to cited id, served_at in window, evidence digest [10.3(2-5)]
```

If the service rewrote the request before inference, an `INFO R.note` line
reports the differing `request.forwarded` hash; whether a rewrite is
acceptable is your policy ([aci.md](aci.md) §10.2). `--model` selects a
model (default: the first entry of `/v1/models`), `--no-stream` requests a
buffered response, and `--json` emits everything as structured data.

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
§10.1 checks the CLI runs, except key custody (check 5) and the TLS-certificate
pin (check 6), which a plain browser cannot reach. A prebuilt ESM bundle
(`npm run build:bundle`) drops into a `<script type="module">` with no build
step.

## 6. Going deeper

[README.md](README.md) routes the rest by task. [aci.md](aci.md) §10 is the
procedure this walkthrough exercised.
