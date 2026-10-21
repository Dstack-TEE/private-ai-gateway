# 0G Private Computer fixtures

- `interop.json`: generated once with 0G's own Go protocol library,
  `github.com/0gfoundation/0g-pc-e2ee/protocol` at
  `6d823311abcbf8ba41656f38395cc50f19360c1b` (release-2026.10.19.2). It holds a
  chat request sealed by `wire.SealRequestFor`, with the enclave key to open it
  and its §8 request binding hash. It also holds a buffered and a three-frame
  streamed response sealed by `wire.ResponseSealer`, with the client key, the
  opened plaintext and broker-style EIP-191 signatures over the
  `proof.SignedTextE2EE[Stream]` texts. `src/aci/upstream/zero_g/tests.rs`
  checks PAG's implementation against it byte for byte.
- `quote-cn19.hex`: the TDX quote compute-network-19 served on 2026-10-20. The
  verifier tests patch its report_data.

These are frozen snapshots. `tests/0g_private_computer_live.rs` covers interop
with the current 0G deployment.
