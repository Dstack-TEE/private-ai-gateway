# 0G Private Computer review

Date: 2026-11-23 PST (round 2).

> [!NOTE]
> This is a dated admission record. See the [documentation conventions](../../README.md#documentation-conventions).
> [Round 2](#round-2-2026-11-23-pst) covers the current broker deployments and
> measurements. The [round-1 record](#round-1-2026-10-20-utc) is retained below.

Provider: [0G Private Computer](https://pc.0g.ai/), private tier only.
Adapter: native `0g-private-computer` provider. See [Verification](verification.md).

## Round 2 (2026-11-23 PST)

Reviewed [`0gfoundation/0g-pc-e2ee`][pc-r2] at `release-2026.11.20.3`
(`37d697f2944f31aaaf10c5d1a1c81376ad364cf5`) and
[`0gfoundation/0g-serving-broker`][broker-r2] at `v0.6.5`
(`04cb230ef954e3875a1f3bdc57f78f926fb2d8fe`). The `glm-5.3` provider ran
[`v0.6.4`][broker-r2-glm] (`sha256:0f7d3961…3312`) and the `0GM-1.0-35B-A3B`
provider ran `v0.6.5` (`sha256:b6c6f96c…f446`). The round-1 broker `v0.5.5`
and its two boot chains were absent from the live brokers checked.

### Verdict

**Acceptable with conditions.** The broker's plaintext destination remains
outside what the gateway adapter verifies. This bars strict inclusion under
the [hard reject conditions](../audit-criteria.md#hard-reject-conditions) until
a relying party can enforce the reviewed plaintext destination.
`TargetSeparated` is an on-chain provider declaration. A route using only the
round-1 boot-chain pins rejects both current providers before sending a prompt.

### Live check (2026-11-23 20:08–20:12 PST)

| Check | Result |
| --- | --- |
| On-chain registry | 12 services. `glm-5.3` was `0xd179f452…e503` (`compute-network-19e`) and `0GM-1.0-35B-A3B` was `0xf4fb8d2b…b250` (`compute-network-20c`). Both had acknowledged signers and declared `TargetSeparated: false`. |
| TDX quotes | Both providers passed `dcap-qvl` and the quote checks in 0G's `pcverify`, with TCB `UpToDate` and no advisories. |
| Boot chains | `dstack-0.6.5` (`8c795543…9d34`) and `dstack-nvidia-0.5.15` (`8dbfd8fe…f191`) both match 0G's allowlist. See [Boot-chain pins](verification.md#boot-chain-pins). |
| `report_data` | All 11 responding brokers served the v1 layout with the on-chain signer. Requests for the v2 layout returned 404. |
| Compose | Both providers' Compose files matched the hashes bound by their quotes and used digest-pinned images and in-CVM targets. The `glm-5.3` Compose ran without a controller or Docker socket. |
| `pcverify` outcome | The event logs replayed to RTMR3. Both runs reported `PASS (INCOMPLETE)` because no reviewed Compose baseline or recorded upstream set was available. |
| Shared CVM | The `0GM-1.0-35B-A3B` provider shares its TD, signer and encryption key with several other provider addresses. |
| Round-1 separated providers | `compute-network-41` and the Baidu and Tencent relays were absent from the registry. |
| Response binding | All responding brokers advertised `e2ee_features: ["resp-bind"]`. |

The on-chain and quote checks that could be replayed against the adapter's
verification rules pass with the round-2 pins. Full adapter verification still
depends on the authenticated router preview offering the corresponding
providers.

### Changes since round 1

- The reviewed deployments specify their in-CVM engines in quote-bound Compose.
  Broker source requires an in-CVM target for decentralized single-model
  providers declaring `TargetSeparated: false` and restricts redirects to the
  same origin. The adapter still reports `plaintext_destination_verified: false`
  because it does not pin the Compose hash.
- [Response binding][pc-r2-response] lets a client seal `_bind` and require an
  HPKE `mode_psk` response, so it can reject a substituted stream on the first
  frame. The adapter still uses base mode and releases provisional chunks
  before checking the final signature.
- A controller-less private broker generates its encryption key at startup.
  A stale key can produce HTTP 409 `e2ee_key_mismatch` until the gateway
  refreshes verification. The adapter records `compose_hash` only for the
  older `mr_config_id` layout, so the claim is absent for the reviewed
  `dstack-0.6.5` provider.

### Open findings

- **Prefix-cache isolation is unverified.** Source reading of broker `v0.6.4`
  and `v0.6.5` found that the top-level allowlist for self-hosted engines omits
  `cache_salt`. The broker may drop the field after opening the request. Live
  forwarding and per-user cache isolation were not re-observed in this check.
  See the [broker allowlist][broker-r2-fields] and the
  [earlier prefix-cache test](../../prefix-cache-isolation.md#observed-enforcement).
- **Incident scope is unknown.** The `glm-5.3` Compose and broker source refer
  to separate November 5 and November 19 security incidents. The reviewed
  `v0.6.4` and `v0.6.5` tags contain the upstream fix for the November 19
  incident. The audit found no public incident report or evidence that resolves
  whether prompts were exposed.
- **Measurement changes.** 0G added the boot chains to its allowlist on
  November 19 and 20 PST. The checked provider records changed within four
  days. The audit found no advance notice to relying parties. See
  [criterion 7](../audit-criteria.md#7-release-and-measurement-updates).

### Conditions

Round-1 conditions 1 to 5 apply. The re-review triggers in condition 6 include
a protocol change, a new broker release line, changed boot-chain pins and newly
registered chat providers declaring `TargetSeparated: false` (`glm-5.3-flash`,
`0GM-1.0-35B-A3B-SIA` and `Unlimited-OCR`). Each new model needs its own live
test before routing traffic to it.

### Not verified

The authenticated router preview and sealed round trip, including signature
retrieval and request-field forwarding, were not run against these brokers.
The public router catalog counted two `glm-5.3` providers and the audit
identified one on-chain. The audit did not independently recompute the OS
measurements from release artifacts, verify broker image signatures or build
provenance, re-observe GPU attestation, or assess network isolation between
containers in the shared CVM.

[pc-r2]: https://github.com/0gfoundation/0g-pc-e2ee/tree/37d697f2944f31aaaf10c5d1a1c81376ad364cf5
[pc-r2-response]: https://github.com/0gfoundation/0g-pc-e2ee/blob/37d697f2944f31aaaf10c5d1a1c81376ad364cf5/protocol/SPEC.md#76-response-binding-synchronous-profiles
[broker-r2]: https://github.com/0gfoundation/0g-serving-broker/tree/04cb230ef954e3875a1f3bdc57f78f926fb2d8fe
[broker-r2-glm]: https://github.com/0gfoundation/0g-serving-broker/releases/tag/v0.6.4
[broker-r2-fields]: https://github.com/0gfoundation/0g-serving-broker/blob/04cb230ef954e3875a1f3bdc57f78f926fb2d8fe/api/inference/internal/ctrl/engine_controls.go#L52-L75

## Round 1 (2026-10-20 UTC)

Supersedes the 2026-10-14 and 2026-10-16 drafts.

Reviewed sources: [`0gfoundation/0g-pc-e2ee`][pc] at `release-2026.10.19.2`
(`6d823311abcbf8ba41656f38395cc50f19360c1b`) and
[`0gfoundation/0g-serving-broker`][broker] at `v0.5.5`
(`962142b3552c921f1c32a911d3986328453d3fdb`).

### Verdict

**Acceptable with conditions.** Each request is sealed to a provider broker
whose TDX quote, boot chain and on-chain signer the gateway verifies before
sending, and each response is accepted only after that broker's signature
verifies. The gateway cannot prove that the broker keeps plaintext inside its
TEE all the way to the model. This is the same class of limitation already
accepted for [NEAR AI](../near-ai/review.md) and
[Phala direct](../phala-direct/review.md), and it bars strict inclusion under
the [hard reject conditions](../audit-criteria.md#hard-reject-conditions).

### Root cause of the trust gap

The broker source is public and the running broker is a signed release, but
that source forwards plaintext to the destinations in its configuration. The
configuration arrives as an encrypted environment value; the quote commits to
its hash, not its content. The broker can record its permitted destinations in
RTMR3 (`recordUpstreamSet`), but the option is off by default and no reviewed
provider enables it ([`upstream_set.go`][upstream-set]).

Two consequences follow:

- Whether a provider forwards to another service (`TargetSeparated`) is
  declared by its operator in the on-chain service record. It is not attested.
- The router chooses which providers it offers. The gateway verifies each
  offered provider but cannot make the router offer a particular one.

### Evidence

Live reads on 2026-10-19 and 2026-10-20 (public data, no inference except the
acceptance runs below):

| Check | Result |
| --- | --- |
| TDX quotes of the private-tier `glm-5.3` providers | Verified independently with `dcap-qvl` and 0G's `pcverify`; TCB `UpToDate` |
| Boot chains | `dstack-nvidia-0.5.4.1` and `dstack-nvidia-0.5.9`; pinned per route as `accepted_subjects` and checked against 0G's allowlist and live quote bytes |
| report_data signer vs on-chain `teeSignerAddress` | Equal and acknowledged |
| Running broker | Signed release `v0.5.5` per RTMR3 replay |
| Recorded upstream set | Absent on every provider |
| `TargetSeparated` | `false` on one private-tier provider; `true` on the other private-tier provider and on the third-party relays |
| GPU evidence (local provider) | 8 Hopper GPUs; NVIDIA NRAS tokens valid; bound to the quote signer through `ZG-Quote-Signature`. Not enforced by the adapter |

The router's `TeeTLS`/`TeeML` labels and the on-chain `verifiability` string
do not separate relays from local serving: relays also declare `TeeML` on
chain. The adapter does not use either.

Acceptance runs of the native adapter on 2026-10-20:

- Local: buffered and streamed completions for `glm-5.3` and
  `0GM-1.0-35B-A3B` passed with live DCAP and signature verification. Only
  `TargetSeparated: false` providers were selected.
- Throwaway dstack CVM: the same through the deployed gateway. Client-supplied
  `X-0G-*` headers had no effect; an unsupported request field returned 400
  (such fields are now sealed); an unconfigured model returned 404. Each
  session carried the `e2ee_public_key_sha256` binding of the selected
  provider.
- Private AI Proxy: `audit` of five response receipts had no failed check
  (receipt signature, request and response hashes, cited 0G session and
  binding). Online `verify` of the gateway passed five checks with the Phala KMS
  root and app-id policy. The remaining check was for in-CVM TLS termination,
  which the disposable test setup lacked.

### Conditions

1. **Pre-send gate.** The gateway sets the private trust mode itself, strips
   client `X-0G-*` headers, and seals only to a provider that passes DCAP, TCB
   policy, the boot-chain allowlist, report_data-to-on-chain signer equality,
   on-chain acknowledgement, a model match, and `TargetSeparated: false`.
2. **Model identity from configuration.** Each route maps a public name to the
   provider's on-chain model id; the router's candidate and the on-chain record
   must both equal it. Router catalog ids are ignored.
3. **Declared, not attested.** `TargetSeparated` and the router's candidate list
   are trusted inputs. Receipts identify the serving provider; they do not
   claim plaintext confinement after the broker.
4. **Response authenticity.** HPKE base mode does not authenticate the sender.
   Buffered responses are released only after the broker signature verifies;
   streamed chunks are provisional and the stream ends in `[DONE]` only after
   verification. Receipts carry no per-response signature-status field.
5. **Not claimed.** GPU execution, model weights, and quote freshness: 0G quotes
   take no client nonce.
6. **Re-review triggers.** A protocol or envelope change in `0g-pc-e2ee`, a new
   broker release line, a provider enabling `recordUpstreamSet`, a new
   private-tier provider declaring `TargetSeparated: false`, or a new boot-chain
   pin.

### Not required

The adapter rejects separated targets before forwarding, which prevents a
routing loop through those providers.

[pc]: https://github.com/0gfoundation/0g-pc-e2ee/tree/6d823311abcbf8ba41656f38395cc50f19360c1b
[broker]: https://github.com/0gfoundation/0g-serving-broker/tree/962142b3552c921f1c32a911d3986328453d3fdb
[upstream-set]: https://github.com/0gfoundation/0g-serving-broker/blob/962142b3552c921f1c32a911d3986328453d3fdb/api/controller/internal/ctrl/upstream_set.go#L52-L78
