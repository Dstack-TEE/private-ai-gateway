# ACI and Related Work

How the [ACI Spec](aci.md) relates to other confidential-inference
systems and to the relevant standards, as of mid-2026. Two facts frame the
comparison:

1. **No interoperable, cross-provider specification for attested AI
   inference exists.** Every deployed system is a single-vendor design on
   shared primitives (TDX/SEV-SNP quotes, DCAP verification, NVIDIA NRAS,
   Sigstore). ACI is, to our knowledge, the first published API-level spec
   intended for independent implementation.
2. **Per-request signed receipts are rare.** Big-tech systems bind responses
   only implicitly to an attested session; among public APIs, only the
   dstack lineage (Phala vllm-proxy, NEAR AI, 0G, Redpill) and Nillion sign
   individual responses. ACI's structured receipt — event log, transparency
   events, upstream verification, session references — has no published
   counterpart.

## Big-tech systems

| System | Client verifies | Per-request binding | Metadata privacy | Open? |
| --- | --- | --- | --- | --- |
| Apple Private Cloud Compute | Device encrypts only to node keys whose attested measurements appear in a public transparency log; published binaries, partial source, Virtual Research Environment | None (implicit via HPKE to attested key) | Third-party OHTTP relay + blind-signature tokens | Closed protocol, most researchable artifacts |
| Google Private AI Compute | Not externally verifiable today: attestation validated against a Google-internal keystore; external inspection is a stated roadmap item; NCC Group audit | None | Third-party IP-blinding relays + anonymous tokens | Closed |
| Azure AI Confidential Inferencing (preview) | Client verifies KMS attestation + transparency receipts before HPKE-sealing to the served key; key release gated on MAA + NVIDIA RIM checks; SCITT-lineage ledger | None yet; response receipts are an explicit roadmap item | OHTTP with optional customer-run relay | Closed service; standards-based design (OHTTP, HPKE, SCITT, EAT), open samples |
| Meta Private Processing (WhatsApp) | Client RA-TLS cross-checked against a Cloudflare-operated third-party transparency log; Trail of Bits / NCC audits published | None | Fastly OHTTP relay + anonymous credentials | Closed |
| OpenAI / Anthropic / AWS managed AI | Nothing client-verifiable today (Anthropic: research paper; AWS: Nitro attestation primitives for customer-built systems only) | None | None | — |

What ACI takes from this tier: the *shape* of the trust argument (attested
keys + enforceable guarantees + transparency) is converging industry-wide,
and each of these systems pairs attestation with a metadata-privacy layer
(OHTTP, RFC 9458) and a software-transparency story. ACI scopes both out of
the core protocol but is designed to compose with them (spec §12, §14).
What none of this tier offers: an interface a third party can implement, or
per-response evidence a client can retain and re-verify.

## Confidential inference providers

| System | Client verifies | Per-request binding | E2EE beyond TLS | Aggregation |
| --- | --- | --- | --- | --- |
| Tinfoil | Self-contained open verifier: SEV-SNP/TDX quote against Sigstore-logged build measurements; TLS key + HPKE key in attestation; dm-verity model packs | None | HPKE body encryption (EHBP) to attested key | Single-vendor enclave router (chained attestation) |
| Privatemode / Continuum (Edgeless) | Client attests a central Coordinator, which attests workers (delegated); manifest-based | None | AES-GCM body encryption via key upload to attested key service; model/params metadata stay plaintext | — |
| NEAR AI Cloud | dstack-lineage report: TDX quote (dcap-qvl), NRAS GPU payload the client submits itself, compose-hash binding, Sigstore image provenance, TLS-key binding | Per-chat ECDSA/Ed25519 signature over `model:sha256(request):sha256(response)` | Field-level (X25519 + XChaCha20-Poly1305) | — |
| Secret AI / SecretVM | In-VM attestation endpoints; TLS-cert fingerprint + GPU nonce bound in `report_data`; open verifier SDK exists but the inference SDK verifies nothing | None (roadmap) | None | — |
| 0G "Sealed Inference" | dstack-shape report (TDX quote + NRAS payload + compose hash); enclave-born signing key acknowledged on-chain | Per-chat response signatures, SDK-verified | None documented | — |
| Nillion nilAI | SEV-SNP report with TLS-cert fingerprint in report data, plus NVIDIA CC token | Signature on every buffered completion — but the verifying key is self-reported beside the quote, not bound into it, and covers the response only | None documented | — |
| Oasis ROFL | On-chain, consensus-verified enclave registration; contracts check transaction origin; no client-checkable binding for HTTPS endpoints (plain ACME TLS) | On-chain, for app outputs (PoC for AI) | — | — |
| Marlin Oyster | Raw AWS Nitro attestation document + open verifiers (CLI/SDKs, on-chain EIP-712 verifier, RISC Zero proof of verification); Nitro-only, no GPU TEE | Self-deploy guide pattern: in-enclave proxy signs responses with an attested key | Attested-channel Noise protocol ("Scallop") | — |

Two observations. First, the strongest independent designs (Tinfoil, NEAR)
verify substantially the same facts ACI's report and verifier profiles
cover: quote to vendor root, measured code linked to public source or
Sigstore-logged builds, and a channel key bound into the evidence. ACI's
`workload_id`/keyset indirection adds what those designs lack — a stable
service identity that survives key rotation, and one attested document from
which *all* channel and signing keys derive.

Second, the per-chat signature convention that NEAR, 0G, OpenRouter-hosted
Phala models, and this gateway's legacy surface share (`/v1/signature/{id}`
over `model:req_hash:resp_hash`) is the de-facto ancestor of ACI receipts.
ACI formalizes and supersedes it: signatures gain a key identity anchored in
the attested keyset, an event log that also commits to rewrites and upstream
verification, and defined semantics for streaming and E2EE responses.

Two cautionary counterexamples. Atoma Network (since pivoted away from its
decentralized offering) shipped request encryption whose key authenticity
rested entirely on an unverified coordinator — its SDKs checked neither
attestation nor response signatures, so the encryption reduced to trusting
the routing proxy. ACI's rule that the client's chosen service key MUST
appear in the attested keyset (spec §7.4), and that the keyset itself is
quote-bound, exists precisely to make that failure mode impossible for a
conformant client. Super Protocol illustrates a second anti-pattern:
attestation consumed by a vendor-operated X.509 authority (with shared root
keys across swarm nodes and partially closed PKI code), leaving clients a
certificate chain to trust rather than evidence to appraise. ACI's
verifier-profile model keeps appraisal on the relying party's side by
construction. A third, softer failure appears in systems whose response
signatures use a key merely published beside the attestation rather than
bound into it — the signature then proves less than it appears to. ACI's
report-data binding (spec §4.4) and receipt key checks (§8.5) exist to
close exactly that gap.

**Aggregation is ACI's unique ground.** Fail-closed upstream verification
with enforced channel bindings, recorded per request and backed by
immutable, content-addressed session records with source-honest claims,
exists in no other published system. Tinfoil's enclave router chains
attestation within one vendor; ACI's aggregator model verifies heterogeneous
third-party providers and gives the client an auditable record of exactly
what was checked.

## Standards alignment

- **IETF RATS (RFC 9334, EAT RFC 9711, AR4SI/EAR drafts).** ACI fits the
  RATS architecture: the service is the Attester, the report is Evidence,
  and the relying party appraises it under a verifier profile (its appraisal
  policy). ACI's typed session claims parallel AR4SI's trustworthiness
  vectors — tri-state verdicts with explicit provenance — in a JSON,
  inference-specific vocabulary. The spec states this mapping (§1.3).
- **Attested TLS (IETF SEAT WG).** No standard exists yet; all deployed
  systems, ACI included, bind a TLS key into attestation by convention. ACI
  pins the certificate SPKI listed in the attested keyset; SEAT's
  exported-authenticator work is the expected future stronger profile
  (spec §1.1).
- **SCITT (RFC 9943) and COSE Receipts (RFC 9942).** Published June 2026 —
  the standardized primitive for transparency-log receipts. ACI receipts and
  sessions are deliberately log-ready (signed, content-addressed, bounded
  size); anchoring them into a SCITT transparency service is the intended
  path to third-party-operated transparency (spec §12), matching the
  transparency-log pattern Apple and Meta ship and Azure's ledger design.
- **OHTTP (RFC 9458).** The standard answer to *who is asking* — every
  big-tech system pairs TEE attestation with relayed transport. ACI proves
  *what is serving* and *what happened*; deployments needing client
  unlinkability compose an OHTTP relay in front of an ACI service without
  protocol changes (spec §12, §14).
- **NVIDIA attestation (NRAS, nvtrust) and the GPU-binding gap.** Hopper-era
  GPU attestation cannot be hardware-bound to the serving CVM; every system
  bridges it in software (nonce conventions) or checks it transitively at
  boot. ACI is unusual in stating this honestly in the artifact itself: the
  `gpu_attested` claim is defined as *not* proving CPU-TEE binding, with
  nonce-binding required for assertion. PCIe TDISP / TEE-I/O on
  Blackwell-class platforms is the forward path (spec §9.3).
- **Sigstore and OpenSSF Model Signing.** Source provenance in ACI reports
  is verifier-profile territory; Sigstore-logged builds and reproducible
  images are the expected evidence, as Tinfoil and NEAR already practice.
  OpenSSF Model Signing is the emerging evidence format for the
  `model_weights_provenance` claim, which no system — ACI included —
  verifies today.
- **Canonical JSON (RFC 8785).** ACI signs JCS bytes and constrains signed
  objects to integer-only numbers, avoiding the known JCS number-formatting
  pitfalls. Sessions sidestep canonicalization entirely by content-addressing.
  COSE would align more closely with RATS/SCITT tooling at the cost of
  human-readable artifacts; a COSE/JOSE binding remains a possible extension
  (spec §4.6).

## Alternative integrity approaches (complements, not competitors)

- **zkML** (EZKL, DeepProve): cryptographic proof of inference; still orders
  of magnitude too slow for production LLM serving in 2026.
- **opML** (ORA): optimistic re-execution with fraud proofs; economic
  security, challenge windows, deterministic reference execution required.
- **Statistical auditing** (model equality testing, TOPLOC): black-box
  detection of model substitution or quantization; probabilistic,
  audit-grade. Natural companions to ACI receipts — a receipt fixes *which
  bytes* a workload served, and statistical tests can then interrogate
  *which model* plausibly produced them.

TEE attestation is the only approach that delivers per-deployment privacy
and integrity at production speed today, which is why every serious system
in this survey — from Apple to the dstack lineage — is built on it.

## Position summary

ACI's combination is not offered by any other published system: an
OpenAI-compatible surface, a stable attested workload identity from which
every channel and signing key derives, per-request signed receipts with
transparency events, fail-closed verified aggregation with content-addressed
audit records, and a spec that third parties can implement. Its deliberate
scope cuts — metadata privacy to OHTTP, durable transparency to SCITT,
build transparency to Sigstore — track exactly the standards the rest of
the field is converging on.
