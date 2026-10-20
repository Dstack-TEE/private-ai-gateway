# 0G Private Computer Verification

The `0g-private-computer` adapter reaches 0G's private (TeeML) tier through the 0G router. It implements 0G's E2EE protocol natively, verifies every candidate provider before sealing anything to it, and releases a response only after the provider's signature recovers to the verified on-chain signer.

| Property | Value |
| --- | --- |
| Attestation scope | Per provider (each verified broker is its own session) |
| Verifier | `src/aci/verifier/zero_g.rs` (native Rust) |
| Verifier ID | `0g-private-computer/native-e2ee/v1` |
| Enforced binding | `e2ee_public_key_sha256`, `key_id` = provider address |
| Transport | 0G E2EE v1 (HPKE base, X25519 / HKDF-SHA256 / ChaCha20-Poly1305) through the router, `src/aci/upstream/zero_g/` |
| Protocol | `0gfoundation/0g-pc-e2ee` `protocol/SPEC.md` at `release-2026.10.19.2` (`6d823311`) |
| Audit | [Acceptable with conditions](review.md), 2026-10-20 |

## Configuration

```json
[{
  "name": "zero-g",
  "provider": "0g-private-computer",
  "base_url": "https://router-api-cloud.0g.ai",
  "bearer_token": "<0G API key>",
  "zg_chain_rpc_url": "https://evmrpc.0g.ai",
  "zg_serving_contract": "0x47340d900bdFec2BD393c626E12ea0656F938d84",
  "models": {"glm-5.3": "glm-5.3"}
}]
```

`base_url` must be the 0G router itself (`router-api-cloud.0g.ai`), not the public entry `router-api.0g.ai`. The public entry is 0G's hosted gateway: it seals plaintext requests on the caller's behalf, so it would see the prompt, and it rejects an already-sealed request (`seal request: no sealed fields`). Pointing at it therefore fails closed. `models` is the model allowlist. The chain RPC is the independent source of provider records, so use one you trust independently of 0G's router. The private-tier header and the `TargetSeparated` rule are code, not config.

## Verification algorithm

Per configured model, cached for `verifier_cache_seconds` (verified results only):

1. `POST /v1/routing/preview` with the model, `service_type: chatbot` and `X-0G-Provider-Trust-Mode: private`. Candidates are deduplicated by address and capped at eight; one whose `canonical_id` differs from the model is skipped.
2. `getService(address)` on the InferenceServing contract through `zg_chain_rpc_url`. Require `teeSignerAcknowledged`, the record's `model` equal to the requested model, and `additionalInfo` declaring `"TargetSeparated": false`. A separated target means the broker forwards plaintext beyond its TEE; absent or unreadable is refused.
3. `GET <record url>/v1/quote?legacy=false` from the broker directly, HTTPS only, no redirects, no ambient proxy.
4. Verify the TDX quote to Intel's root with `dcap-qvl` (`pccs_url`, default Phala PCCS), which refuses invalid TCB statuses and debug TDs.
5. Require MRTD, RTMR1 and RTMR2 to match an allowlisted dstack image: `dstack-nvidia-0.5.9` or `dstack-nvidia-0.5.4.1`. The values come from 0G's `client/evidence/brokerimages.json` at the pinned release, which documents a dstack-mr derivation from each published release; PAG has not recomputed them.
6. Require `report_data` = `enc_pub(32) ‖ signer(20) ‖ version 1 ‖ zero(8)` and the signer to equal the on-chain `teeSignerAddress`.

A failing candidate is skipped and logged under target `zero_g`. With none left, the result is `failed`, no prompt is sent, and the request is refused like any other verification failure (503 `upstream_verification_failed` with a refusal receipt), whether or not the caller required verification. On 2026-10-20 only compute-network-19 passed. compute-network-41 and the Baidu/Tencent relays declare `TargetSeparated: true`; compute-network-41 forwards to RedPill's gateway, so a gateway request never reaches the provider that forwards to RedPill.

## Forwarding

The backend uses the first provider, in preview order, whose recorded key hashes to a binding the current event carries.

- `messages`, `tools` and `tool_choice` are sealed (§5.1 default). As 0G's client does, a streamed request gets `stream_options.include_usage: true` before sealing, so the router need not rewrite it. Routing and sampling fields (`model`, `stream`, `temperature`, `max_tokens`, …) stay cleartext but are bound in the AAD; nothing is unbound, so a rewritten `model` fails the provider's `Open`. Any other field is refused with a 400 `invalid_request_error` instead of being sent in the clear. Only `/v1/chat/completions` is served.
- The router receives exactly: the API key, `X-0G-Provider-Trust-Mode: private`, `X-0G-Provider-Address` (and `-Identity` when the preview gave one) and `X-0G-Provider-Allow-Fallbacks: false`. Caller and route headers are not forwarded. `_e2ee.signer_addr` pins the verified signer.
- A non-200 router or broker response becomes a generic upstream error; its body is not returned.

## Responses

Every frame must be sealed, seal `choices`, not seal `usage` or `model`, not unbind `usage`, and decrypt to exactly its declared fields. Frames open in order under one HPKE context, so reordering, dropping or tampering fails.

The §8 signature is fetched from `<record url>/v1/proxy/signature/<ZG-Res-Key>` on the broker, not the router, with three attempts on 404, 5xx and transport errors. Its text must equal the binding computed locally over the sealed request and every frame (`zg-sig-v1/e2ee-ct` or `zg-sig-v1/e2ee-ct-stream`), and it must recover (EIP-191) to the verified signer.

- **Buffered:** returned only after the signature verifies. A fetch failure (`signature_unavailable`) or a bad signature (`signature_invalid`) fails the request; both are logged under target `zero_g`.
- **Streamed:** frames are delivered as they open, but the provider's `[DONE]` is withheld. The gateway emits `[DONE]` only after the final frame and a verified signature. Truncation, a bad frame or a bad signature ends the stream with an error, so the gateway appends its error tail, never a success terminator. Frames already delivered are provisional until `[DONE]`.

A receipt cites the session of the provider that served it. That session's claims hold the provider address, signer, encryption key and the enforced binding. A buffered receipt exists only for a verified response. A streamed receipt covers bytes that end in `[DONE]` only when the signature verified; there is no separate per-response signature field.

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Hardware-proven: verified TDX quote whose key is the enforced binding. |
| `tcb_up_to_date` | Hardware-proven from the provider's TCB status. |
| `os_known_good`, `gpu_attested`, `serving_software_known_good`, `model_weights_provenance` | Unknown. |

`claims.extra` also records `os_image`, `compose_hash` (v1 `mr_config_id`), `onchain_verifiability` (recorded, not trusted) and `plaintext_destination_verified: false`.

## Limitations

- Where the broker sends plaintext is not proven. Per-model targets come from unmeasured broker config, plaintext forwarding follows redirects, and the controller holds `docker.sock`. Confinement to the provider TEE is not claimed.
- The private tier is enforced by the router. PAG's own gates are the on-chain record and quote checks above, so a provider that misdeclares `TargetSeparated` passes them.
- There is no quote freshness challenge (0G's quote takes no nonce). GPU evidence (0G's `ZG-Quote-Signature` over NVIDIA evidence) and weights are not checked.
- A send failure does not fall back to another verified provider.
- Receipts carry no per-response signature-status field: the outcome is implied by the receipt existing (buffered) or by the stream ending in `[DONE]` (streamed).
- Response cleartext floats could fail to open, because `serde_json` float parsing is not guaranteed correctly rounded. This fails closed; current responses carry none.

## Tests

- `src/aci/upstream/zero_g/tests.rs`:
  - interop with `tests/fixtures/0g-private-computer/interop.json`, produced by 0G's Go protocol library at the pinned release (provenance in that directory's README), plus the broker signature KAT;
  - tampered, reordered, truncated and unsealed frames;
  - a mock router and broker covering header pinning, wrong signer, missing signature, router errors, stream truncation, reorder and tamper, unknown fields, and binding mismatch.
- `src/aci/verifier/tests/zero_g.rs`: the real compute-network-19 quote with patched report_data. It covers TargetSeparated, unacknowledged signer, model mismatch, signer mismatch, boot chain, quote redirect, and root-check rejections, and failing closed when no candidate passes.
- `tests/0g_private_computer_live.rs`: an ignored live glm-5.3 test. See [Live E2E test suite](../../live-e2e-test-suite.md).
