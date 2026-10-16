# dstack v1 event-log verification policy

The TypeScript verifier and `aci-verify` (used by pap and desktop) enforce the
same policy before accepting a compose measurement. This is a dstack TDX policy;
it does not accept arbitrary TDX applications' RTMR3 event formats.

## Upstream evidence

Both [v0.5.9's emitter](https://github.com/Dstack-TEE/dstack/blob/v0.5.9/dstack-attest/src/lib.rs#L17)
and [v0.6.1's emitter](https://github.com/Dstack-TEE/dstack/blob/v0.6.1/dstack/dstack-attest/src/lib.rs#L190)
extend RTMR3 with `RuntimeEvent::sha384_digest()` and `cc_event_type()`.
Their [v0.5.9](https://github.com/Dstack-TEE/dstack/blob/v0.5.9/cc-eventlog/src/tdx.rs)
and [v0.6.1](https://github.com/Dstack-TEE/dstack/blob/v0.6.1/dstack/cc-eventlog/src/tdx.rs)
log assemblers append runtime events with `imr: 3` and type `0x08000001` to
the firmware log. Firmware samples use registers 0–2. Neither dstack emitter
produces a non-runtime RTMR3 event. The recorded production fixture also has
only runtime events in RTMR3. Accordingly, every RTMR3 entry must have this
type; every digest is recomputed and checked against its stored value before
replay. Other registers do not contribute to RTMR3 or compose selection.

The built-in names in the
[v0.5.9](https://github.com/Dstack-TEE/dstack/blob/v0.5.9/dstack-util/src/system_setup.rs)
and [v0.6.1](https://github.com/Dstack-TEE/dstack/blob/v0.6.1/dstack/dstack-util/src/system_setup.rs)
emitters are:

`app-id`, `boot-mr-done`, `compose-hash`, `gpu-attestation`, `gpu-policy-hash`,
`init-script-hash`, `instance-id`, `key-provider`, `mr-kms`, `os-image-hash`,
`storage-encrypted`, `storage-fs`, `system-preparing`, `system-ready`.

All match the grammar `[a-z][a-z0-9]*(?:-[a-z0-9]+)*`. This grammar also accepts
custom names in that form. Upstream's generic extension API accepts caller
strings; this verifier deliberately restricts those names because `:` makes
the v1 binary preimage ambiguous. Names with colons, empty names, whitespace,
or non-ASCII characters are rejected for runtime events. Empty firmware names
in other registers remain valid.

Only v1 is supported: `version` must be absent or the integer token `1`.
The digest preimage is `u32le(event_type) || ":" || UTF8(name) || ":" || payload`.
Opt-in v2, null and string versions are rejected. Float and exponent spellings
of all three integer fields are rejected, including `version: 1.0`, rather
than being silently normalized.

## Parsing and resource limits

The production quote captured on October 16, 2026 at 5:17 AM PDT in
[`dstack_v1_attestation.json`](../clients/verifier-ts/test/fixtures/dstack_v1_attestation.json)
has a 7,357-byte event log, a 30,220-byte UTF-8 compose, 30 entries (10 in
RTMR3), nesting depth 2, and a largest decoded string field of 408 bytes.
Both implementations apply the following limits:

| Input | Maximum | Headroom over the recorded fixture |
| --- | ---: | ---: |
| `event_log` UTF-8 bytes | 1 MiB (1,048,576) | 142× |
| `app_compose` UTF-8 bytes | 4 MiB (4,194,304) | 138× |
| Event-log entries | 1,024 | 34× |
| Nested JSON containers, including the root array | 16 | 8× |
| Each decoded event-log string key or value | 128 KiB (131,072) | 321× |

The event-log size is checked before parsing. Compose size is checked before
hashing; compose is an opaque string, not parsed as JSON by this check.
Entry count, container depth, duplicate keys, integer tokens, and field sizes
are checked during a bounded preflight, including in unknown metadata. All
event-log object keys must be unique after JSON escape decoding. Unknown
members remain allowed within these limits.

TS uses Microsoft's maintained
[`jsonc-parser`](https://github.com/microsoft/node-jsonc-parser) visitor with
comments and trailing commas disabled, followed by `JSON.parse`. The visitor
exposes decoded keys and original token spans, so it can reject duplicates
and non-integer spellings before information is lost. This avoids maintaining
a security-sensitive JSON lexer. Visitor exceptions stop descent at the depth
limit. String validation counts UTF-8 bytes without encoding and rejects lone
surrogates before `TextEncoder` could replace them.

Rust uses a `serde_json` deserializer visitor before typed deserialization;
it rejects duplicate decoded keys, bounds unknown metadata, and deserializes
integer fields directly as `u32`. Rust strings are valid UTF-8, and
`serde_json` already rejects lone surrogate escapes, including in the outer
`app_compose` field.

The offline TS and Rust tests apply the same
[`dstack_policy_cases.json`](../clients/verifier-ts/test/fixtures/dstack_policy_cases.json)
corpus to the recorded quote and event log. The delimiter vectors explicitly
use synthetic RTMR3 values and make no hardware-validity claim. The separate
recorded-quote transcript test verifies vendor signatures using recorded
collateral and asserts that digest tampering produces `NOT VERIFIED`.
