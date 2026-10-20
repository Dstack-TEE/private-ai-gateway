# 0G Private Computer Review

Date: 2026-10-20 UTC. Supersedes the 2026-10-14 and 2026-10-16 drafts.

> [!NOTE]
> This is a dated admissions audit. It records the evidence and decision from
> the date above. Use [verification.md](verification.md) for the adapter
> algorithm, claims, and limitations.

Provider: [0G Private Computer](https://pc.0g.ai/), private tier only.
Reviewed sources: [`0gfoundation/0g-pc-e2ee`][pc] at `release-2026.10.19.2`
(`6d823311abcbf8ba41656f38395cc50f19360c1b`) and
[`0gfoundation/0g-serving-broker`][broker] at `v0.5.5`
(`962142b3552c921f1c32a911d3986328453d3fdb`).
Adapter: native `0g-private-computer` provider; see
[verification.md](verification.md).

## Verdict

**Acceptable with conditions.** Each request is sealed to a provider broker
whose TDX quote, boot chain and on-chain signer the gateway verifies before
sending, and each response is accepted only after that broker's signature
verifies. The gateway cannot prove that the broker keeps plaintext inside its
TEE all the way to the model. This is the same class of limitation already
accepted for [NEAR AI](../near-ai/review.md) and
[Phala direct](../phala-direct/review.md), and it bars strict inclusion under
the [hard reject conditions](../audit-criteria.md#hard-reject-conditions).

## Root cause of the trust gap

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

## Evidence

Live reads on 2026-10-19 and 2026-10-20 (public data, no inference except the
acceptance runs below):

| Check | Result |
| --- | --- |
| TDX quotes of the private-tier `glm-5.3` providers | Verified independently with `dcap-qvl` and 0G's `pcverify`; TCB `UpToDate` |
| Boot chain | `dstack-nvidia-0.5.4.1`, in 0G's audited allowlist |
| report_data signer vs on-chain `teeSignerAddress` | Equal and acknowledged |
| Running broker | Signed release `v0.5.5` per RTMR3 replay |
| Recorded upstream set | Absent on every provider |
| `TargetSeparated` | `false` on one private-tier provider; `true` on the other private-tier provider and on the third-party relays |
| Separated private-tier target | The second private-tier provider's `TargetTeeAddress` equals the signing address of RedPill's production gateway |
| GPU evidence (local provider) | 8 Hopper GPUs; NVIDIA NRAS tokens valid; bound to the quote signer through `ZG-Quote-Signature`. Not enforced by the adapter |

The router's `TeeTLS`/`TeeML` labels and the on-chain `verifiability` string
do not separate relays from local serving: relays also declare `TeeML` on
chain. The adapter does not use either.

Acceptance runs of the native adapter:

- Local, 2026-10-20: buffered and streamed `glm-5.3` completions passed with
  live DCAP and signature verification. Only the `TargetSeparated: false`
  provider was selected.
- Throwaway dstack CVM, 2026-10-20: the same, through the deployed gateway.
  Client-supplied `X-0G-*` headers had no effect. Unsupported request fields and
  unverifiable models failed before sending. The session carried the
  `e2ee_public_key_sha256` binding of the selected provider.

## Conditions

1. **Pre-send gate.** The gateway sets the private trust mode itself, strips
   client `X-0G-*` headers, and seals only to a provider that passes DCAP, TCB
   policy, the boot-chain allowlist, report_data-to-on-chain signer equality,
   on-chain acknowledgement, a model match, and `TargetSeparated: false`.
2. **Declared, not attested.** `TargetSeparated` and the router's candidate list
   are trusted inputs. Receipts identify the serving provider; they do not
   claim plaintext confinement after the broker.
3. **Response authenticity.** HPKE base mode does not authenticate the sender.
   Buffered responses are released only after the broker signature verifies;
   streamed chunks are provisional and the stream ends in `[DONE]` only after
   verification. Receipts carry no per-response signature-status field.
4. **Not claimed.** GPU execution, model weights, and quote freshness: 0G quotes
   take no client nonce.
5. **Re-review triggers.** A protocol or envelope change in `0g-pc-e2ee`, a new
   broker release line, a provider enabling `recordUpstreamSet`, or a new
   private-tier provider declaring `TargetSeparated: false`.

## Not required

A routing-loop guard is not needed. The second private-tier provider forwards
to RedPill's gateway, but the adapter refuses separated providers before
sending, so a gateway request never reaches it.

[pc]: https://github.com/0gfoundation/0g-pc-e2ee/tree/6d823311abcbf8ba41656f38395cc50f19360c1b
[broker]: https://github.com/0gfoundation/0g-serving-broker/tree/962142b3552c921f1c32a911d3986328453d3fdb
[upstream-set]: https://github.com/0gfoundation/0g-serving-broker/blob/962142b3552c921f1c32a911d3986328453d3fdb/api/controller/internal/ctrl/upstream_set.go#L52-L78
