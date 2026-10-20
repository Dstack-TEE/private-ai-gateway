# 0G Private Computer Verification

The `0g-private-computer` adapter reaches 0G's private (TeeML) tier through the
0G router. It implements 0G's E2EE protocol natively: every candidate provider
is verified before anything is sealed to it, the prompt is sealed to the key the
provider's TDX quote binds, and a response is released only after the
provider's signature over it recovers to the verified on-chain signer.

| Property | Value |
| --- | --- |
| Attestation scope | Per provider (each verified broker is its own session) |
| Verifier | Native Rust, `src/aci/verifier/zero_g.rs` |
| Verifier ID | `0g-private-computer/native-e2ee/v1` |
| Enforced binding | `e2ee_public_key_sha256` (`key_id` = provider address) |
| Transport | 0G E2EE v1: HPKE base mode, DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305; field-level sealing through the router (`src/aci/upstream/zero_g/`) |
| Protocol reference | `0gfoundation/0g-pc-e2ee` `protocol/SPEC.md` at `release-2026.10.19.2` (`6d823311`) |
| Audit | Not yet audited; assessment in [PR #439](https://github.com/Dstack-TEE/private-ai-gateway/pull/439) |

## Configuration

```json
[{
  "name": "zero-g",
  "provider": "0g-private-computer",
  "base_url": "https://router-api.0g.ai",
  "bearer_token": "<0G API key>",
  "zg_chain_rpc_url": "https://evmrpc.0g.ai",
  "zg_serving_contract": "0x47340d900bdFec2BD393c626E12ea0656F938d84",
  "models": {"glm-5.3": "glm-5.3"}
}]
```

`models` is the model allowlist: only listed provider model IDs are routed, and
each is verified separately. The router origin, API key, chain RPC and contract
are operator configuration; the private trust-tier header and the
`TargetSeparated` rule are code and cannot be configured away. The chain RPC is
the independent source for provider records, so it should be operated by, or
trusted independently of, 0G's router. See
[Upstream fields](../../configuration-reference.md#upstream-fields).

## Verification algorithm

Verification runs per configured model and is cached for
`verifier_cache_seconds` like other providers; only a `verified` result is
cached. The router's answers are untrusted inputs.

1. **Route preview.** `POST <base_url>/v1/routing/preview` with
   `{"model", "service_type": "chatbot"}`, the API key, and
   `X-0G-Provider-Trust-Mode: private` set by code. Candidates are deduplicated
   by address and capped at eight. A candidate whose `canonical_id` or
   `model_id` differs from the requested model is skipped.
2. **On-chain record.** `eth_call getService(address)` on the InferenceServing
   contract through `zg_chain_rpc_url`, decoded fail-closed. The candidate is
   skipped unless `teeSignerAcknowledged` is true, the record's `model` equals
   the requested model, and its `additionalInfo` JSON declares
   `"TargetSeparated": false`. A separated target means the broker forwards
   plaintext beyond its own TEE (on 2026-10-20, compute-network-41 and the
   Baidu/Tencent relays declared it); an absent or unreadable declaration is
   also refused.
3. **Quote.** `GET <record url>/v1/quote?legacy=false` directly from the
   provider broker over HTTPS, without redirects or an ambient proxy. The
   record's URL is used, not one from the router.
4. **DCAP.** The TDX quote is verified to Intel's root with PAG's `dcap-qvl`
   path (`pccs_url`, default Phala PCCS). dcap-qvl refuses invalid TCB
   statuses and debug TDs; the returned status is recorded.
5. **Boot chain.** MRTD, RTMR1 and RTMR2 must equal an allowlisted dstack
   image (below).
6. **report_data.** Parsed per SPEC §4.2: `enc_pub(32) ‖ signer(20) ‖ version
   = 1 ‖ zero(8)`.
7. **Signer.** The quote's signer must equal the on-chain `teeSignerAddress`.

A candidate that fails any step is skipped and logged under target `zero_g`.
With no surviving candidate the result is `failed` and no prompt is sent.
Each surviving provider yields one `e2ee_public_key_sha256` binding over
`SHA256(enc_pub)` and one entry in `provider_claims.instances`.

### Boot-chain allowlist

| Image | Source |
| --- | --- |
| `dstack-nvidia-0.5.9` | `Dstack-TEE/meta-dstack` v0.5.9; `os_image_hash` `806a352e…`, the production image PAG already lists in `scripts/dstack_os_image.py` |
| `dstack-nvidia-0.5.4.1` | `nearai/private-ml-sdk` v0.5.4.1; `os_image_hash` `86b18137…` |

The values are copied from 0G's provider allowlist,
`client/evidence/brokerimages.json` at the pinned release, which documents a
dstack-mr derivation of each entry from the published release artifact. PAG has
not recomputed them; on 2026-10-20 compute-network-19's quote matched the
0.5.4.1 entry. RTMR0 (VM shape) and RTMR3 (application) are excluded, as in
0G's own check.

## Sealing and forwarding

The backend uses only providers whose recorded key hashes to a binding the
current event carries, in preview order, and sends to the first. It never
falls back to plaintext.

- **Sealed** (encrypted to the provider, SPEC §5–§6) whenever present:
  `messages` (required), `tools`, `tool_choice`, `response_format`, `user`,
  `metadata`.
- **Cleartext, authenticated**: `model`, `stream`, `stream_options`, sampling
  and length controls (`temperature`, `top_p`, `top_k`, `min_p`, `max_tokens`,
  `max_completion_tokens`, `stop`, `n`, `presence_penalty`,
  `frequency_penalty`, `repetition_penalty`, `seed`, `logprobs`,
  `top_logprobs`, `logit_bias`, `parallel_tool_calls`, `reasoning_effort`).
  Every cleartext field, `model` included, is bound in the AAD; nothing is
  declared unbound, so a router rewrite fails the provider's `Open`.
- **Refused**: any other field. SPEC §5.1 would send unknown fields in the
  clear; this adapter rejects the request instead. Only
  `/v1/chat/completions` is served.

The request goes to `<base_url>/v1/chat/completions` with exactly these
headers: the API key, `X-0G-Provider-Trust-Mode: private`,
`X-0G-Provider-Address` (and `X-0G-Provider-Identity` when the preview gave
one) pinning the verified provider, and `X-0G-Provider-Allow-Fallbacks: false`.
Caller and route headers are never forwarded, so no `X-0G-*` control can be
injected. `_e2ee.signer_addr` pins the verified signer; the broker refuses an
envelope addressed to another signer.

If the router ignores the pin, another provider cannot open the envelope, and
any response it produced would fail the signature check below.

## Responses

Responses are opened per SPEC §7 with the per-request client key. Every frame
must be sealed, must seal `choices`, must not seal `usage` or `model`, must not
unbind `usage`, and must decrypt to exactly its declared fields. Frames are
opened in order under one HPKE context, so a reordered, dropped, replayed or
tampered frame fails. A frame after the final one is refused.

The §8 signature is fetched directly from the verified broker,
`<record url>/v1/proxy/signature/<ZG-Res-Key>` (not via the router), with up to
three attempts for 404, 5xx and transport errors. It must carry the
`zg-sig-v1/e2ee-ct` (buffered) or `zg-sig-v1/e2ee-ct-stream` (streamed) text
computed locally over the sealed request and every received frame, and its
EIP-191 signature must recover to the verified signer, never the
self-reported `signing_address`.

- **Buffered**: the response must be one final frame. It is returned only
  after the signature verifies. A missing `ZG-Res-Key`, a fetch failure
  (`0G response signature unavailable`) and a wrong or forged signature
  (`0G response signature invalid`) all fail the request; the two signature
  outcomes are logged with distinct `kind` values so their frequency can be
  measured.
- **Streamed**: opened frames are delivered as they arrive. The provider's own
  `[DONE]` is withheld; after the final frame the signature is fetched and
  verified, and only then does the gateway emit `[DONE]`. A truncated stream
  (no final frame), a frame failure or a signature failure ends the stream with
  an error instead, so the gateway appends its protocol error tail and never a
  success terminator. Frames before the failure have already reached the
  client: they are provisional until `[DONE]`.

### Receipts

A receipt's `upstream.verified` event cites the session of the provider that
served (`served_instance_id` = provider address). That session records the
provider address, its signer (as the session identity's `signing_address`),
the enforced key binding, and the claims below. A buffered receipt exists only
for a signature-verified response. On the direct streaming path the receipt is
signed over the delivered bytes, which end in `[DONE]` only when the signature
verified and otherwise in the error tail; on the middleware path a stream that
ends in an upstream error yields no receipt (the gateway's existing broken-stream
behavior). No per-response receipt field records the signature outcome.

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted as hardware-proven: verified TDX quote whose key is the enforced binding. |
| `tcb_up_to_date` | Hardware-proven from the provider's TCB status: asserted for `UpToDate`, refuted otherwise. |
| `os_known_good` | Unknown. The boot chain is gated, but PAG does not classify the image itself. |
| `gpu_attested` | Unknown. GPU evidence is not checked. |
| `serving_software_known_good` | Unknown. |
| `model_weights_provenance` | Unknown. |

`claims.extra` carries the provider entry (address, record URL, model, signer,
encryption key, `os_image`, `compose_hash` from a v1 `mr_config_id`,
`onchain_verifiability`) and shared facts including
`plaintext_destination_verified: false`. The on-chain `verifiability` string is
recorded but not trusted: on 2026-10-20 the relays reported `TeeML` too.

## Limitations

- **Plaintext destination after the broker is not proven.** The broker
  forwards the opened request to its configured target. For
  compute-network-19 the measured compose pins a local target, but per-model
  targets come from unmeasured broker config, bare-label targets are a shape
  check only, plaintext forwarding follows redirects, and the controller holds
  `docker.sock`. Confinement to the provider TEE is not claimed.
- **Tier selection.** The private tier is requested by header and enforced by
  the router; PAG's own gates are the on-chain `TargetSeparated: false`,
  acknowledged signer, model and quote checks. A provider that misdeclares
  `TargetSeparated` passes them.
- **No quote freshness challenge.** 0G's quote endpoint takes no client nonce;
  the binding is to the provider's long-lived keys, re-checked every
  verification period.
- **GPU and weights are unverified.** 0G brokers sign their quote reply
  (`ZG-Quote-Signature`) binding NVIDIA evidence to the report_data signer;
  PAG does not check it. Weights are not attested.
- **Streamed content is provisional** until `[DONE]` (above).
- **Router metadata** (`trust_mode`, `verifiability`, provider names) is not
  trusted for any decision.
- **Floating-point JSON.** The AAD is RFC 8785 JCS over the frame as parsed;
  serde_json's default float parsing is not guaranteed correctly rounded, so a
  response cleartext float could fail to open (fail-closed). Responses carry
  no floats in their cleartext fields today.

## RedPill routing-loop rule

compute-network-41 (`0x6446fE52…`) forwards glm-5.3 to RedPill's gateway
(`TargetSeparated: true`, `TargetTeeAddress` = the signing address of
`api.redpill.ai`). This adapter refuses it before sending. Independently, a
deployment that exposes this adapter must never let its own glm-5.3 routes or
fallbacks select a 0G route that can reach RedPill, and RedPill's 0G-backed
routes must not be offered back to 0G, or a request can loop PAG → 0G → RedPill
→ PAG. This is an operator routing rule; this change does not alter production
routing.

## Tests and interop fixtures

- `src/aci/upstream/zero_g/protocol.rs`: request sealing, frame opening, §8
  binding and EIP-191 recovery against
  `tests/fixtures/0g-private-computer/interop.json`, which 0G's own Go library
  produced; the broker go-ethereum signature KAT; tamper, reorder, truncation,
  unsealed-frame and forged-signature negatives.
- `src/aci/upstream/zero_g/tests.rs`: a mock router and broker running the
  enclave side; header pinning and stripping, buffered and streamed success,
  wrong signer, missing signature, redirected signature fetch, unsealed
  response, truncated, reordered and tampered streams, unknown fields and a
  binding mismatch.
- `src/aci/verifier/tests/zero_g.rs`: preview, chain and quote mocks using the
  real compute-network-19 quote with patched report_data (DCAP-to-root
  stubbed); TargetSeparated, unacknowledged, model mismatch, signer mismatch,
  boot chain, redirect and root-check rejections.
- `tests/0g_private_computer_live.rs`: ignored live glm-5.3 test; see
  [Live E2E test suite](../../live-e2e-test-suite.md).

Regenerate the interop fixture with the Go toolchain the generator declares:

```sh
cd tests/fixtures/0g-private-computer/generator
go run . > ../interop.json
```

`go.mod` pins `github.com/0gfoundation/0g-pc-e2ee/protocol` to the release
commit. HPKE draws fresh randomness, so each run produces different
ciphertexts with the same structure.
