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

`pap serve` applies serving policy to JSON objects and to multipart
`POST /v1/audio/transcriptions` requests. Multipart requires the verified
gateway's `service_capabilities.supported_multipart_constraint_endpoints`
to list that path; otherwise the upload is refused locally. Policy travels
in `X-ACI-Provider`, whose JSON object uses the JSON `provider` schema.
The carrier must be US-ASCII; encode non-ASCII string values with JSON
`\uXXXX` escapes.
Caller pins are intersected with local pins, including when local verified
serving is disabled. Duplicate or invalid carriers, a `provider` form part,
constrained malformed JSON, and unsupported constrained media types return
400 before an inference send. The original multipart bytes are preserved.
Receipts hash the body, so the client separately retains and audits the
effective header policy. These client protections must be released before
a gateway enables transcription uploads.

Compatibility change: constrained JSON requests must set
`Content-Type: application/json` (or `application/*+json`). JSON bytes with a
missing or different content type are now refused. For example, a `fetch`
string body defaults to `text/plain`, and `curl -d` defaults to
`application/x-www-form-urlencoded`; both need an explicit JSON content type.
