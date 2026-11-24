# 0G Private Computer verification

The `0g-private-computer` adapter reaches 0G's private (TeeML) tier through the 0G router. It implements 0G's E2EE protocol natively, verifies every candidate provider before sealing anything to it, and checks that the response signature recovers to the verified on-chain signer before releasing a buffered response or marking a stream complete.

| Property | Value |
| --- | --- |
| Attestation scope | Per provider (each verified broker is its own session) |
| Verifier | `src/aci/verifier/zero_g.rs` (native Rust) |
| Verifier ID | `0g-private-computer/native-e2ee/v1` |
| Enforced binding | `e2ee_public_key_sha256`, `key_id` = provider address |
| Transport | 0G E2EE v1 (X25519 / HKDF-SHA256 / ChaCha20-Poly1305) through the router. Requests use HPKE base mode. Responses use PSK mode when the broker advertises support. |
| Protocol | `0gfoundation/0g-pc-e2ee` `protocol/SPEC.md` at `release-2026.11.20.3` (`37d697f2`). |
| Audit | [Acceptable with conditions](review.md#round-2-2026-11-23-pst), 2026-11-23 PST |

## Configuration

```json
[{
  "name": "zero-g",
  "provider": "0g-private-computer",
  "base_url": "https://router-api-cloud.0g.ai",
  "bearer_token": "<0G API key>",
  "zg_chain_rpc_url": "https://evmrpc.0g.ai",
  "zg_serving_contract": "0x47340d900bdFec2BD393c626E12ea0656F938d84",
  "accepted_subjects": [
    "tdx-boot-chain:sha256:8c79554388166a9b2484a13ea4ccce08a81854a0993dc27b5c3b6adcfacc9d34",
    "tdx-boot-chain:sha256:8dbfd8fe8bdef1ed5aca189e78f8e143e7c74d79bcbc04c3a38003f3cd0ef191"
  ],
  "models": {"glm-5.3": "glm-5.3", "0gm-1.0-35b-a3b": "0GM-1.0-35B-A3B"}
}]
```

`base_url` must be the 0G router itself (`router-api-cloud.0g.ai`), not the public entry `router-api.0g.ai`. The public entry is 0G's hosted gateway: it seals plaintext requests on the caller's behalf, so it would see the prompt, and it rejects an already-sealed request (`seal request: no sealed fields`). Pointing at it therefore fails closed. `models` is the model allowlist: each public name maps to the provider's on-chain model id, exactly as the InferenceServing record spells it. That configured id is the only model identity PAG trusts. `accepted_subjects` pins the provider OS images (see [Boot-chain pins](#boot-chain-pins)); it is required. The chain RPC is the independent source of provider records, so use one you trust independently of 0G's router. The private-tier header and the `TargetSeparated` rule are code, not config.

## Verification algorithm

Per configured model, cached for `verifier_cache_seconds` (verified results only):

1. `POST /v1/routing/preview` with the model, `service_type: chatbot` and `X-0G-Provider-Trust-Mode: private`. Candidates are deduplicated by address and capped at eight. A candidate's `model_id` must equal the configured upstream model exactly. The router's `canonical_id` is catalog metadata and is never used for identity.
2. `getService(address)` on the InferenceServing contract through `zg_chain_rpc_url`. Require `teeSignerAcknowledged`, the record's `model` equal to the configured upstream model exactly, and `additionalInfo` declaring `"TargetSeparated": false`. A separated target means the broker forwards plaintext beyond its TEE; absent or unreadable is refused.
3. `GET <record url>/v1/quote?legacy=false` from the broker directly, HTTPS only, no redirects, no ambient proxy.
4. Verify the TDX quote to Intel's root with `dcap-qvl` (`pccs_url`, default Phala PCCS), which refuses invalid TCB statuses and debug TDs.
5. Require the quote's boot chain to be one of the route's `accepted_subjects`.
6. Require `report_data` = `enc_pub(32) ‖ signer(20) ‖ version 1 ‖ zero(8)` and the signer to equal the on-chain `teeSignerAddress`.
7. Read `e2ee_features` from the broker's direct HTTPS quote reply and enable response binding for that provider when the array contains `"resp-bind"`. The feature list is HTTPS metadata outside the signed quote.

A failing candidate is skipped and logged under target `zero_g`. With none left, the result is `failed`, no prompt is sent, and the request is refused like any other verification failure (503 `upstream_verification_failed` with a refusal receipt), whether or not the caller required verification.

On 2026-11-23 PST, `glm-5.3` (`compute-network-19e`) and `0GM-1.0-35B-A3B` (`compute-network-20c`) passed the on-chain and quote checks with the round-2 pins below. The separated providers from round 1 were absent from the registry. See the [round-2 audit](review.md#round-2-2026-11-23-pst) for the live checks and their limits.

### Boot-chain pins

A `tdx-boot-chain:sha256:<hex>` subject is SHA-256 over the 144 bytes MRTD ‖ RTMR1 ‖ RTMR2 of a dstack OS image: the firmware, the kernel, and the cmdline (which pins the rootfs verity hash) with initrd. It deliberately excludes RTMR0 (VM shape) and RTMR3 (the application), which vary per provider. armet-ai's `tdx-measurement` subject covers all five registers and the launch configuration, so it cannot express this.

| Image | Subject | Audit status |
| --- | --- | --- |
| `dstack-0.6.5` (compute-network-19e, `glm-5.3`) | `tdx-boot-chain:sha256:8c79554388166a9b2484a13ea4ccce08a81854a0993dc27b5c3b6adcfacc9d34` | Observed on 2026-11-23 PST |
| `dstack-nvidia-0.5.15` (compute-network-20c, `0GM-1.0-35B-A3B`) | `tdx-boot-chain:sha256:8dbfd8fe8bdef1ed5aca189e78f8e143e7c74d79bcbc04c3a38003f3cd0ef191` | Observed on 2026-11-23 PST |
| `dstack-nvidia-0.5.4.1` (compute-network-19, `glm-5.3`) | `tdx-boot-chain:sha256:536a92e7c6204087b2c2910e4c17083b7a9f8b3aa455b6bc14c6df3df2f1a7fc` | Round 1 only. Absent from the brokers checked in round 2. |
| `dstack-nvidia-0.5.9` (compute-network-20, `0GM-1.0-35B-A3B`) | `tdx-boot-chain:sha256:bebbc1f26600aa7e415962d919d1919613d7be1d3ce9f4043c19f6785da11c0e` | Round 1 only. Absent from the brokers checked in round 2. |

The round-2 register values match the single-pass entries in [0G's `brokerimages.json` at `release-2026.11.20.3`](https://github.com/0gfoundation/0g-pc-e2ee/blob/37d697f2944f31aaaf10c5d1a1c81376ad364cf5/client/evidence/brokerimages.json). Their `os_image_hash` values are `4771a29e…8623` and `3bb7d262…4ef2`, respectively. 0G derives the registers with dstack-mr from the published OS images. The audit matched the registers to live quotes. Independent derivation from the release artifacts remains unverified. To derive a subject, take MRTD, RTMR1 and RTMR2 from a dstack-mr measurement of the release and confirm them against a verified quote:

```sh
printf '%s' "$MRTD$RTMR1$RTMR2" | xxd -r -p | sha256sum
```

## Adding a model

Adding a model is configuration only: map its public name in the route's `models` to the provider's on-chain model id (for example `"0gm-1.0-35b-a3b": "0GM-1.0-35B-A3B"`), and make sure `accepted_subjects` covers the image its provider runs. It must be a chat-completions model whose private-tier provider has an acknowledged signer and `TargetSeparated: false`. Run the live test with `PRIVATE_AI_GATEWAY_0G_MODEL` set to the model before routing traffic to it.

## Forwarding

The backend uses the first provider, in preview order, whose recorded key hashes to a binding the current event carries.

- Only the fields 0G's router reads stay cleartext, bound in the AAD: `model` (routing and attribution), `stream` (how the router serves the response), `stream_options` (the router forces `include_usage` to bill a stream), and `max_tokens` / `max_completion_tokens` (0G's design lists the output limit as read for routing and billing; the router's source is not public, so they stay readable). The provider is pinned, so the router routes on nothing else. The adapter seals `messages`, `tools` and `tool_choice` by default and uses §5.1 to seal all other fields, including `temperature`, `stop`, `logit_bias`, `response_format`, `reasoning_effort`, `user` and `metadata`. Sealing protects those fields through delivery to the verified broker. The broker's forwarding policy determines which fields reach the engine. As 0G's client does, a streamed request gets `stream_options.include_usage: true` before sealing, so the router need not rewrite it. Nothing is unbound, so a rewritten `model` fails the provider's `Open`. The sealed `model` is the configured on-chain id, which the router forwards unchanged. 0G's client instead seals the `canonical_id` and leaves `model` unbound so the router can rewrite it. Caller-supplied `_e2ee` and `_bind` fields are refused. Only `/v1/chat/completions` is served.
- The adapter does not translate reasoning. As for any route, the middleware writes it in the candidate's `reasoningFormat` ([control-plane contract](../../control-plane-contract.md#reasoning-format)), and a 0G route needs `reasoning_effort`. A managed candidate (no `engine`) does not forward `chat_template_kwargs`, so the `chat_template_*` formats would drop the switch. `thinking_type` writes a `thinking` field, and an unset `reasoningFormat` writes a nested `reasoning` object. The adapter seals both fields. The provider ignored `thinking` in [round 1](#provider-behavior). The nested object remains untested.
- The router receives exactly: the API key, `X-0G-Provider-Trust-Mode: private`, `X-0G-Provider-Address` (and `-Identity` when the preview gave one) and `X-0G-Provider-Allow-Fallbacks: false`. Caller and route headers are not forwarded. `_e2ee.signer_addr` pins the verified signer.
- A 409 with the broker's `e2ee_key_mismatch` error code triggers the existing channel-binding reverification path. A gateway-managed binding can be refreshed from a new verified quote before resealing the request. Other non-200 responses keep their status. Error bodies are withheld.

Middleware injects a per-user `cache_salt` and the adapter seals it. Source reading of broker `v0.6.4` and `v0.6.5` found that the top-level allowlist for self-hosted engines omits `cache_salt`, so the broker may drop it after opening the request. Forwarding and cache isolation remain unverified live for these versions. See the [earlier prefix-cache test](../../prefix-cache-isolation.md#observed-enforcement).

## Responses

Every frame must be sealed, seal `choices`, not seal `usage` or `model`, not unbind `usage`, and decrypt to exactly its declared fields. Frames open in order under one HPKE context, so reordering, dropping or tampering fails.

For a provider that advertises `resp-bind`, the gateway derives `_bind` from the response ephemeral private key, seals it in the request and requires §7.6 HPKE PSK mode on the response. The first frame must authenticate under that key before any chunk is released. Other providers use HPKE base mode.

If the first PSK frame fails authentication, the gateway invalidates the selected broker's cached verification and can retry a gateway-managed request once through the channel-binding reverification path, before the gateway starts the client response. An early SSE heartbeat also precludes retry. It looks up that broker on-chain again and verifies a fresh quote directly. The retry uses base mode only if the fresh quote reply omits the advertisement. Sibling brokers keep their cached evidence and expiry. A later frame failure ends the stream without retrying.

Only the decrypted fields and the bound cleartext reach the caller. A field named in a frame's `unbound_fields` is outside the AAD and the §8 binding, so an intermediary can set it undetected (§5.2; §8.2: "a router-injected `x_0g_trace` is unauthenticated by construction"). Every unbound field is therefore dropped when the frame opens, after its AEAD and binding checks, and the §8 signature still covers the frames as received. In round 1, live frames declared `["model", "x_0g_trace"]` unbound. `x_0g_trace` carried the router's billing amounts and provider address. 0G unbinds `model` so the router can restore its alias (§5.2), so the caller gets the `model` this request bound, which is the configured on-chain id. A streamed frame left with no value to deliver is not emitted. In round 1, the last live frame carried only `x_0g_trace` and an empty sealed `choices`.

The §8 signature is fetched from `<record url>/v1/proxy/signature/<ZG-Res-Key>` on the broker, not the router, with three attempts on 404, 5xx and transport errors. Its text must equal the binding computed locally over the sealed request and every frame (`zg-sig-v1/e2ee-ct` or `zg-sig-v1/e2ee-ct-stream`), and it must recover (EIP-191) to the verified signer.

- **Buffered:** returned only after the signature verifies. A fetch failure (`signature_unavailable`) or a bad signature (`signature_invalid`) fails the request; both are logged under target `zero_g`.
- **Streamed:** frames are delivered as they open, but the provider's `[DONE]` is withheld. The gateway emits `[DONE]` only after the final frame and a verified signature. Truncation, a bad frame or a bad signature ends the stream with an error, so the gateway appends its error tail, never a success terminator. In base mode, chunks already delivered are provisional until `[DONE]`. With response binding, chunks authenticate on arrival. The final signature still determines whether the stream completed successfully.

The outer SSE envelope uses the shared `sse-stream` decoder with a 4 MiB block
cap. It accepts LF, CRLF and bare CR framing, compares `[DONE]` exactly, discards
incomplete blocks at EOF and rejects invalid UTF-8. Opened application values and
the verified terminal use the library encoder. Application decoding happens once
after this provider boundary; it cannot release the terminal before the signature
check above.

A receipt cites the session of the provider that served it. That session's claims hold the provider address, signer, encryption key and the enforced binding. A buffered receipt exists only for a verified response. A streamed receipt covers bytes that end in `[DONE]` only when the signature verified; there is no separate per-response signature field.

## Provider behavior

These behaviors were observed on 2026-10-21 with `glm-5.3` on the round-1 host `compute-network-19` (broker `v0.5.5`). They were not re-observed in round 2.

- Reasoning cannot be turned off. `reasoning_effort` `none` or `minimal`, or `chat_template_kwargs.enable_thinking: false`, stop the provider from returning reasoning separately, but the model still reasons. Its thinking and a closing `</think>` then land in `content`, and a long run can hit the token limit.
- On short prompts, `reasoning_effort` `low` and `medium` produce about one reasoning token, and `high` through `max` only a little more. Without `reasoning_effort`, or with `chat_template_kwargs.enable_thinking: true`, the model reasons by default. Under the `reasoning_effort` format, `reasoning: {"enabled": true}` becomes `medium` and can return no reasoning text.
- All effort levels from `minimal` to `max` are accepted. `response_format` `json_object` and `json_schema` are honoured. A `thinking: {"type": "disabled"}` field is ignored.
- Usage reports `reasoning_tokens` at the top level. The middleware moves it into `completion_tokens_details`; direct mode passes it through unchanged.
- A cold prompt of about 810K tokens got a router 502 after about 20 seconds. The same request succeeded, buffered and streamed, once the provider had cached the prompt.

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Hardware-proven: verified TDX quote whose key is the enforced binding. |
| `tcb_up_to_date` | Hardware-proven from the provider's TCB status. |
| `os_known_good`, `gpu_attested`, `serving_software_known_good`, `model_weights_provenance` | Unknown. |

`claims.extra` records `boot_chain`, `onchain_verifiability` (informational) and `plaintext_destination_verified: false`. With `mr_config_id` v1, the adapter reads `compose_hash` directly from the register. With dstack 0.6's v3 layout, the adapter uses the shared dstack verifier to replay V1 and V2 runtime events to the quote's RTMR3, then reconstructs the launch document from the verified boot events. The hash is recorded only when the document reproduces the quote's `mr_config_id`. Incomplete or inconsistent evidence leaves the claim absent. The `mr_config_id` layout is independent of the `report_data` version.

## Limitations

- The reviewed deployments specify their in-CVM targets in quote-bound Compose, and the broker restricts redirects to the same origin. The adapter does not pin the Compose hash or prove plaintext confinement.
- The private tier is enforced by the router. PAG's own gates are the on-chain record and quote checks above, so a provider that misdeclares `TargetSeparated` passes them.
- There is no quote freshness challenge (0G's quote takes no nonce). GPU evidence (0G's `ZG-Quote-Signature` over NVIDIA evidence) and weights are not checked.
- A send failure does not fall back to another verified provider.
- The stale-key 409 arrives via the router. A misbehaving router can trigger up to two reverifications per request with a gateway-managed binding. The gateway fails closed if reverification fails or the mismatch persists.
- Receipts carry no per-response signature-status field: the outcome is implied by the receipt existing (buffered) or by the stream ending in `[DONE]` (streamed).
- Response cleartext floats can cause a frame to fail closed because `serde_json` float parsing is not guaranteed to round correctly. Round-1 responses carried no cleartext floats.

## Tests

- `src/aci/upstream/zero_g/tests.rs`:
  - base-mode and response-bound interop from 0G's Go protocol library at the pinned release, plus the broker signature known-answer test. See [fixture provenance](../../../tests/fixtures/0g-private-computer/README.md).
  - tampered, reordered, truncated and unsealed frames, and base-mode responses refused when binding was requested. Gateway tests cover the single retry after a fresh quote, persistent mismatch, and a later frame failure without retrying;
  - a mock router and broker covering header pinning, wrong signer, missing signature, router errors, stream truncation, reorder and tamper, sealed non-cleartext fields (`response_format`, reasoning controls, `user`, `metadata`, …) with cleartext fields bound, unbound fields dropped while bound fields and `usage` survive, and stale-key conflict classification.
- `src/aci/verifier/tests/zero_g.rs` uses a dstack 0.6.5 quote fixture. It covers admission failures, Compose claim extraction for both layouts, omission of the v3 claim when evidence fails to match, and recording an advertised response-binding capability.
- `tests/0g_private_computer_live.rs`: an ignored authenticated live test for the reviewed boot chains, including a sealed JSON-mode request with `reasoning_effort`. See [Live E2E test suite](../../live-e2e-test-suite.md).
