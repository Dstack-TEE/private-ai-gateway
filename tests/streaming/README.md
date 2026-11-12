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
Of the 110 cases, 54 match main exactly and 56 pin approved corrections. The eight
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
records are converted or forwarded. Accounting still observes the source to EOF
with main's bridge usage rules: Chat-source bridges stop accounting at their source
terminal, and all bridges stop at a malformed continuation that main's converter
would reject. Until then, usage after the destination failure terminal remains
billable. The report retains the conversion failure's `upstream_transport` label;
a later upstream read failure supplies its report status and class even after a
failure terminal: reset is 502 `upstream_transport`, timeout is 504
`upstream_timeout`. The eight `failed_bridge_accounting_cases` snapshots pin
complete reports against main, including retained pre-terminal usage. Malformed
continuations still cannot produce another client error tail; reading and receipt
publication continue to EOF. Native nonterminal failure records continue normally
until their protocol terminal. The `diff_messages_chat_usage_after_error_1` case
pins the removed continuation chunk and the unchanged completion-token report.
Legacy Anthropic completion reports a source failure instead of success.

Nine `inband_timeout_cases` were recorded on main `0edde219`, covering each native
protocol and bridge, a native failed Messages delta, and both native Responses
failure terminals. Every complete report matches main's 504 `upstream_timeout`,
including the exact `response.error` stream in the sanitizer-renaming regression.
Usage and pricing retain each path's main semantics. Six complete snapshots match
main exactly; the other three pin existing framing/canonical-failure corrections.

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
Native Messages usage retains main's existing unfiltered report semantics; adding
a Messages usage allowlist is a separate privacy follow-up.
