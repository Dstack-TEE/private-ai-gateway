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

`streaming_corrections.json` keeps the two approved changed expectations alongside
their original bytes and a reason. Sixteen cases remain identical to the original
snapshot. The declared non-SSE JSON case loses its old SSE blank-line suffix;
legacy completion loses the bare `[DONE]` block and emits one typed data terminal.
Only the wire bytes, returned digest and receipt signature change in those cases.

Framing normalization and rejection cases are pinned separately by
`library_contract.rs`, `src/sse.rs` tests and typed pipeline tests. They cover LF,
CRLF and bare CR at every split, BOM removal, canonical field spacing and order,
last valid duplicate metadata, numeric retry formatting, omitted unknown fields,
comments and empty blocks, retained empty-data contributions, discarded pending
EOF data, exact terminals, contradictory control types and strict UTF-8 rejection.
Unedited JSON text retains its whitespace. `accounting.rs` exercises the production
typed stages and response owner for cost, usage, timeout, cancellation and E2EE
failure settlement, without a public byte-transform API.
