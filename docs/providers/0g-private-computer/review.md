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
| Boot chains | `dstack-nvidia-0.5.4.1` and `dstack-nvidia-0.5.9`; pinned per route as `accepted_subjects` and checked against 0G's allowlist and live quote bytes |
| report_data signer vs on-chain `teeSignerAddress` | Equal and acknowledged |
| Running broker | Signed release `v0.5.5` per RTMR3 replay |
| Recorded upstream set | Absent on every provider |
| `TargetSeparated` | `false` on one private-tier provider; `true` on the other private-tier provider and on the third-party relays |
| Separated private-tier target | The second private-tier provider's `TargetTeeAddress` equals the signing address of RedPill's production gateway |
| GPU evidence (local provider) | 8 Hopper GPUs; NVIDIA NRAS tokens valid; bound to the quote signer through `ZG-Quote-Signature`. Not enforced by the adapter |

The router's `TeeTLS`/`TeeML` labels and the on-chain `verifiability` string
do not separate relays from local serving: relays also declare `TeeML` on
chain. The adapter does not use either.

Acceptance runs of the native adapter at #464 head `e25c1b99`, 2026-10-20:

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
  binding). Online `verify` of the gateway passed id-1 to id-5 with the Phala KMS
  root and app-id policy; id-6 failed only because the test deployment had no
  in-CVM TLS termination, as in earlier test deployments.

## Conditions

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

## Not required

A routing-loop guard is not needed. The second private-tier provider forwards
to RedPill's gateway, but the adapter refuses separated providers before
sending, so a gateway request never reaches it.

[pc]: https://github.com/0gfoundation/0g-pc-e2ee/tree/6d823311abcbf8ba41656f38395cc50f19360c1b
[broker]: https://github.com/0gfoundation/0g-serving-broker/tree/962142b3552c921f1c32a911d3986328453d3fdb
[upstream-set]: https://github.com/0gfoundation/0g-serving-broker/blob/962142b3552c921f1c32a911d3986328453d3fdb/api/controller/internal/ctrl/upstream_set.go#L52-L78
