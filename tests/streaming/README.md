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

The test supplies fixed request identities, signing keys and receipt clocks
through existing interfaces. Encryption, receipt IDs, generated timestamps and
elapsed times use the production implementations. The harness replaces encrypted
blobs (including their ephemeral keys and nonces), receipt IDs/signatures,
generated timestamps and elapsed times with stable snapshot placeholders. Before
normalizing, it decrypts every encrypted field with the client key, compares its
source plaintext, rejects tampered AAD, verifies each receipt signature, and checks
receipt hashes against the captured source and client bytes. Snapshot returned
hashes then cover the normalized wire bytes; source hashes remain unchanged.
JSON ordering/whitespace and SSE framing are preserved. The source stream
deliberately splits every seven bytes and withholds EOF separately from its
terminal event.

The white-box tests live in standard source test modules:
`src/aggregator/service/tests/characterization.rs` and `support.rs`,
`src/aggregator/service/streaming/tests/accounting.rs`, and
`src/sse/tests/library_contract.rs`. Provider envelope fixture builders live in
Chutes and 0G test modules. Production modules expose no fixture helper functions.

An incomplete provider-envelope line at clean EOF is an upstream transport
failure (502, `upstream_transport`), with no synthesized DONE or receipt.
A pending read error, including a timeout, retains its original classification.

Run without credentials or inference requests:

```sh
cargo test --locked -j 8 --lib exact_wire_characterization_corpus
```

`UPDATE_STREAMING_CORPUS=1` records a baseline. Do not regenerate it to accommodate
refactoring differences: preserve the original snapshot and explicitly identify
any approved parsing correction with its old and new expectation.

Both fixtures use compact JSON Lines, one object per case sorted by case name.
The loader projects the original raw baselines through the same normalization;
the main recordings are retained unchanged, without re-running or re-recording
main. New recordings use the normalized representation.
`streaming_corrections.jsonl` records every approved difference with the original
wire bytes and the corrected snapshot; its case name is `expected.name`.
The corpus retains 145 main-recorded cases; exact-match and correction counts
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
Once the owner emits a failed response's destination terminal, continuation
output is discarded. The existing bridge chain still validates and updates its
source state, preserving malformed-continuation report labels without duplicating
bridge acceptance rules in stream control. Accounting observes source records
until EOF or client disconnect, independently
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
`src/sse/tests/library_contract.rs`, `src/sse.rs` tests and typed pipeline tests. They cover LF,
CRLF and bare CR at every split, BOM removal, canonical field spacing and order,
last valid duplicate metadata, numeric retry formatting, omitted unknown fields,
comments and empty blocks, retained empty-data contributions, exact terminals,
contradictory control types and strict UTF-8 rejection. At clean EOF, a complete
final field line is dispatched even without the blank delimiter (LF, CRLF and CR
are all supported). Only a genuinely incomplete line is discarded. Transport
errors and truncated clean EOF before a terminal emit the protocol error tail;
no partial payload has reached the client.
Unedited JSON text retains its whitespace. `src/aggregator/service/streaming/tests/accounting.rs` exercises the production
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
`discarded_continuations_use_bridge_validation_and_keep_source_usage` checks
source terminal state, Anthropic controls, malformed continuations and later usage
through the production native-to-bridge and two-bridge pipelines.

`source_billing_prices_cache_writes_strings_and_partial_messages_on_every_bridge`
checks 17 additional main-recorded cases: Chat cache writes on native and bridged
paths, ignored inconsistent writes, explicit-zero precedence, numeric strings,
start-only Messages at EOF/disconnect, and larger Messages delta input counts.
The shared counter parser pins non-negative integer JSON numbers and ASCII
digit-only strings (including leading zeros), bounded by JavaScript's maximum
safe integer, in pricing unit tests. Strings are never trimmed; signs, decimal
points, exponents and whitespace are rejected. Client-injected cost and report
usage are asserted separately.

## Terminal publication

The response owner publishes the receipt in the poll returning the first eligible
destination terminal. It keeps the same source input and #563's source-based
accounting through EOF, suppressing all later client bytes. `terminal.rs` checks
lookup and signature validity before another body poll, keeps the source pending,
and drops the body immediately after the terminal to check its completed report.
Native Chat, Messages and Responses, every bridge, legacy completion, early commit,
E2EE, synthesized terminals and direct serving use the same publication boundary.
LF, CRLF and CR prefixes at six chunk sizes pin the received-hash cutoff, including
when terminal and suffix arrive in one chunk. A dispatching CR excludes its next LF.

The receipt store captures the adapter's source bytes and EOF state at publication.
Before normalization, the harness reuses the production decoder with byte counting
to locate the source terminal block and requires its exact prefix hash to equal
`response.received`; EOF publication checks every captured source byte. The raw
`response.returned` hash must equal the captured client bytes. Existing terminal
patches apply to raw baselines before both pass through the shared normalization,
so no receipt-ID, signature, timestamp or encryption overrides are needed.

Early publication requires the source protocol's own accepted terminal with a
trusted backend protocol declaration, or clean adapter EOF. Real 0G adapter regressions
cover unsigned forged frames, missing final frames, rejected signatures and valid
signed controls on native and bridged paths. A bridge failure terminal without
an accepted source terminal keeps source authentication and EOF finalization.
Both bridges that synthesize a terminal at clean Chat EOF publish in that poll.

`candidate_format_must_match_the_configured_upstream_protocol` rejects a declared
Anthropic format for a Chat backend before verification or forwarding, using the
real router and 0G fixture on Chat and Messages clients. Matching controls verify
and publish. `backend_protocol_declarations_accept_only_the_matching_format`
covers known backend declarations on versioned, unversioned, prefixed and custom
paths. The real OpenAI-compatible backend on `/custom/infer` publishes before EOF
and rejects contradictory formats. Unknown backend declarations retain forwarding
and require clean EOF for publication; pending-source and real-adapter tests pin
this rule. Their test-only path alias leaves its protocol unknown and exists
because production 0G accepts solely `/v1/chat/completions`. Responses and legacy
completion use the selected typed upstream operation, never the URL path.

`configured_backend_publishes_receipt_before_terminal_then_pending_disconnect`
in `tests/http.rs` uses `UpstreamConfigManager::backend()` and the production HTTP
router: receipt GET succeeds before EOF or another body poll, and dropping after
the terminal yields one 200 report with full observed usage. The custom-path
publication/rejection test and the provider/operation declaration matrix also
load through the config manager. These tests cover the dynamic configuration
wrapper that hand-built routers bypassed. The backend trait requires an explicit
declaration on every implementation so a wrapper cannot inherit unknown by
omission.

The 145-case corpus has 71 unchanged cases and 74 approved changes: 59 earlier
publication (including completed status after a published-terminal disconnect),
10 source-hash cutoff and 5 output cutoff. Every report's status, usage, pricing
and computed cost equals #563, except the approved post-publication disconnect
status. Those billing assertions are independent of terminal patches. Main and
#563 snapshots remain untouched. Compact JSON Lines in
`streaming_terminal_corrections.jsonl` name every difference; stale or unused
patches fail verification.

Of the 18 new source-billing cases, 12 are unchanged. The other six are:

- Earlier publication: `billing_chat_cumulative_duplicates`,
  `native_messages_billing_allowlist_merge`, and
  `native_messages_billing_usage_after_malformed`.
- Source-hash cutoff: `billing_chat_messages_usage_after_terminal` and
  `billing_chat_responses_usage_after_terminal`.
- Output cutoff: `billing_chat_chat_usage_after_terminal`.

The existing `diff_messages_responses_error_middle_1` signature patch and
`messages_responses_inband_error_then_timeout` receipt patch reflect #563's new
source-based usage projection into the Responses failure output. Their categories
are unchanged. All other prior terminal patches remain unchanged.

Of the 17 counter/cache cases, the six start-only Messages EOF/disconnect cases
are unchanged. The eleven others change only `/before_eof/receipt`: cache writes
on all three Chat-source surfaces, ignored inconsistent writes, explicit-zero
precedence, numeric strings on all three surfaces, and larger Messages deltas on
all three surfaces. Their wire bytes, receipts and complete reports match #563;
all 63 existing terminal patches are unchanged.

All nine timeout cases retain 504 `upstream_timeout`, including the five that
publish a receipt before the timeout. They have no report before the timeout.
`post_terminal_e2ee_failure_keeps_observing_source_usage` checks that later usage99
is still billed on a 502 gateway failure while the early receipt and wire hashes
stay frozen. Publication failures withhold the successful terminal, continue
source observation and report once at EOF; the retention-failure regression pins
later cumulative usage and genuine-EOF metrics. A disconnect before the terminal
remains 499 without a receipt. Drop performs no receipt or completion work.

The `native_post_terminal_usage` receipt loses its `req-fixture` alias because its
only source id arrives after `[DONE]`. Normal pre-terminal ids remain queryable in
the yielding poll. Native Messages, native Responses and Messages-to-Responses
already lacked a `chat_id` alias on #563; this limitation is preserved.
