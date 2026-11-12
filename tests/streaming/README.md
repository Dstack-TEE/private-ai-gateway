# Streaming wire characterization

`streaming_wire.jsonl` records the unmodified streaming behavior at revision
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

Both fixtures use compact JSON Lines, one object per case sorted by case name.
`streaming_corrections.jsonl` records every approved difference with the original
wire bytes and the corrected snapshot; its case name is `expected.name`.
The corpus retains 128 main-recorded cases; exact-match and correction counts
are reported in the PR verification results. The eight
failed-bridge accounting/reset cases were also recorded on `9b745bda`; their
complete snapshots are identical on both main revisions.
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
Once the owner emits a failed response's destination terminal, no continuation
records are converted or forwarded. Accounting observes source records until EOF or client disconnect, independently
of terminal position, failures, malformed records or destination protocol. Reports
use the source protocol's filtered usage: the latest cumulative value for Chat,
merged start/delta counters for Messages, and reported response usage for Responses.
Repeated cumulative observations are never summed. All usage passes its protocol
allowlist before pricing or reporting, including Messages and buffered responses.
A contradictory control is rejected after source usage observation; later source
records remain observable. Invalid UTF-8 and size overflow remain fatal decoding
errors.
Buffered conversion failures retain readable source usage on their existing 502
report. Declared non-SSE JSON objects also report filtered source usage, on normal
and early paths, without changing client bytes or injecting cost. Opaque, invalid,
non-object and over-cap bodies report no usage. Conversion, encoding and E2EE failures suppress further output while source
observation continues synchronously; there is no background drain. Their reports
therefore settle at EOF or disconnect, retaining the original failure status,
attribution and label, with no receipt where the old abort suppressed publication.
A malformed continuation's report label is separate from billing. A later upstream
read failure still supplies its existing status/class: reset is 502
`upstream_transport`, timeout is 504 `upstream_timeout`. Native nonterminal failure
records continue until their protocol terminal.
Legacy Anthropic completion reports a source failure instead of success.

Nine `inband_timeout_cases` were recorded on main `0edde219`, covering each native
protocol and bridge, a native failed Messages delta, and both native Responses
failure terminals. Every complete report matches main's 504 `upstream_timeout`,
including the exact `response.error` stream in the sanitizer-renaming regression.
Status and attribution remain main's; usage now follows the source billing rule.

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

Streaming sanitation always follows the existing payload schema, regardless of
unknown event names. Unrepresentable bridge payloads are upstream read failures;
queue overflow remains a gateway failure. Direct serving uses the existing idle
keep-alive stage at its fixed 5-second default without a new configuration API.
Middleware's zero keep-alive setting disables middleware heartbeat and early commit.
The `source_billing_is_filtered_cumulative_and_independent_of_bridge_or_position`
regression covers cumulative duplicates, all supported bridges, post-terminal and
post-malformed usage, Messages cache merging/filtering, and buffered filtering.
`owner_e2ee_failure_observes_later_usage_until_eof_or_disconnect` checks fatal output
failure and cancellation while source observation is pending.
