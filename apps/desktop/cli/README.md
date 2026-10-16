# The `private-ai-proxy` crate

This crate builds the `pap` CLI of Private AI Proxy: every command-line
surface, the ACI relying-party verifier, the local verifying proxy, and the
`private-ai-proxy-service` entry point. [The CLI reference](../docs/cli.md)
documents the commands, flags and exit codes, and
[the architecture](../docs/client-architecture.md) places this crate among the
others.

```bash
cargo run --package private-ai-proxy --bin private-ai-proxy -- <command> --help
```

## Where verification lives

The crate's `aci/` modules own the relying-party appraisal: DCAP quote
verification, which checks run, the custody policy, and receipt signatures.
The policy-neutral mechanisms beneath those checks come from the `aci-verify`
crate: the §9.1(2) binding chain, dstack event-log replay and the KMS custody
chain, and the declared TLS selection. The `aci-protocol` crate supplies wire
types, JCS, attestation-statement construction, and receipt canonicalization.
The CLI maps verification outcomes to a pass, fail, or honest skip.

Deliberate differences from the specification, such as the CLI's honest skips
and checks only a relying party can run, are recorded in
[the ACI conformance notes](../../../docs/reviews/aci-spec-conformance-gaps.md).
[The ACI quickstart](../../../docs/quickstart.md) walks through the commands
against a live deployment.

A session contains an establishing-evidence snapshot. A reused session does not supply evidence from every later verification round. Session and evidence hashes prove integrity; they do not perform provider-specific deep audit. Fixed session pins require explicit replacement after expiry or material migration.
