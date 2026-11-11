# Streaming wire characterization

`streaming_wire.json` records the unmodified streaming behavior at revision
`0edde219`, before the typed event migration. The corpus compares concatenated
HTTP entity bytes, source and returned SHA-256 hashes, complete signed receipts,
signature validity, control-plane reports (including usage, cost and route
attribution), and receipt/report availability at an explicit poll checkpoint
before upstream EOF.

The cases cover native Chat, Messages and Responses, every supported bridge,
legacy completion, early commitment and its initial heartbeat, E2EE, Chutes and
0G envelope opening, disconnect after partial usage, session lease handoff,
buffered JSON, buffered SSE with and without encryption, and declared non-SSE
streaming responses. Provider envelopes use real encryption and verification.
E2EE output is also decrypted and rejects tampered AAD.

The test supplies fixed request/receipt identities, signing keys, clocks and
X25519 encryption inputs. Those overrides compile only under `cfg(test)` and are
scoped to this test's thread. Production encryption retains OS randomness; other
crypto tests continue to exercise random inputs. Duration and TTFT use a fixed
test clock. The source stream deliberately splits every seven bytes and withholds
EOF separately from its terminal event.

Run without credentials or inference requests:

```sh
cargo test --locked -j 8 --lib exact_wire_characterization_corpus
```

`UPDATE_STREAMING_CORPUS=1` records a baseline. Do not regenerate it to accommodate
refactoring differences: preserve the original snapshot and explicitly identify
any approved parsing correction with its old and new expectation.

`streaming_corrections.json` records every approved difference with the original
wire bytes and the corrected snapshot. Of the 93 cases, 47 match main exactly and 46 pin approved corrections.
The failure cases cover each destination and bridge. Native failures retain
their canonical output, including rate-limit codes on Chat, Messages and both
Responses terminal forms. Failed Chat usage keeps public token counts and cost
without copying provider fields into the client body or report, including when
the usage arrives after the error and before `[DONE]`. Native failed records
are sanitized in place: Chat keeps allowlisted chunk fields and finish reasons,
Messages keeps a failed delta's content and usage, and Responses keeps failed
response content and lifecycle envelope fields. The native error sanitizers still
remove unknown fields on Messages errors and Responses error events. Bridge failures
omit startup events or the Chat chunk wrapper that existed only to carry an error.
After content has been emitted, the owner asks the bridge to use its existing
close path, keeping closing frames and partial Responses output before failure.
Legacy Anthropic completion reports a source failure instead of success.

Framing normalization and rejection cases are pinned separately by
`library_contract.rs`, `src/sse.rs` tests and typed pipeline tests. They cover LF,
CRLF and bare CR at every split, BOM removal, canonical field spacing and order,
last valid duplicate metadata, numeric retry formatting, omitted unknown fields,
comments and empty blocks, retained empty-data contributions, exact terminals,
contradictory control types and strict UTF-8 rejection. At clean EOF, a complete
final field line is dispatched even without the blank delimiter (LF, CRLF and CR
are all supported). Only a genuinely incomplete line is discarded. Transport
errors and truncated clean EOF before a terminal emit the protocol error tail;
no partial payload has reached the client.
Unedited JSON text retains its whitespace. `accounting.rs` exercises the production
typed stages and response owner for cost, usage, timeout, cancellation and E2EE
failure settlement, without a public byte-transform API.

The reproducible differential in `differential.rs` uses seed `56320261111`: nine
supported source/destination paths, 30 input patterns and two chunk splits per
pattern (540 cases). The original 24 patterns cover unknown and mismatched
names/types, missing types, extensions, failure and terminal positions, extra
provider fields and usage, malformed/non-JSON payloads, transport errors and
framing. The added patterns reset inside the field name, inside data, and after
the final data line, or end cleanly on LF, CRLF and CR. Every split pair has
identical snapshots on both revisions.

`streaming_differential_main.jsonl` contains the main `0edde219` snapshots, one
JSON object per line. `streaming_differential_corrections.json` retains a reason
and the exact expected snapshot for every approved difference. The total new
snapshot size is below 2 MB. Run the complete comparison without credentials:

```sh
cargo test --locked -j 8 --lib seeded_wire_differential -- --nocapture
```

Of the 540 cases, 264 match main exactly and 276 differ only in listed corrections.
The 18 original pending-EOF inputs now dispatch complete final lines as main does:
14 match exactly; the other four differ only in the existing legacy sentinel or
bridged usage privacy corrections. Genuinely incomplete clean EOF emits a failure
tail; resets retain main's 502 `upstream_transport` report and tail with canonical
blank-free framing. No new tail is appended after a produced terminal.

Usage follows main's payload schema independently of the classified control kind.
Unknown/mismatched Messages names retain main's usage on native and bridged paths,
while the approved classifier-based Chat preamble differences remain. Native
Messages usage stays unfiltered as on main. Source receipt hashes remain unchanged;
optional response identity can differ only where discarded incomplete data or a
removed legacy failure completion previously supplied it. Minimal reproductions
of fixed bugs remain in the ordinary corpus as well.

`UPDATE_STREAMING_DIFFERENTIAL=1` records snapshots and must only be used with
main's production code plus the test-only recording harness. The committed main
snapshot was recorded in the owned checkout on the existing instrumentation-only
baseline of `0edde219`, never on the typed implementation.

Streaming sanitation always follows the existing payload schema, regardless of
unknown event names. Unrepresentable bridge payloads are upstream read failures;
queue overflow remains a gateway failure. Direct serving uses the existing idle
keep-alive stage at its fixed 5-second default without a new configuration API.
Middleware's zero keep-alive setting disables middleware heartbeat and early commit.
Native Messages usage retains main's existing unfiltered report semantics; adding
a Messages usage allowlist is a separate privacy follow-up.
