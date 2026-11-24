# 0G Private Computer fixtures

- `interop.json` uses 0G's Go protocol library at
  [`release-2026.11.20.3`](https://github.com/0gfoundation/0g-pc-e2ee/tree/37d697f2944f31aaaf10c5d1a1c81376ad364cf5/protocol).
  The base-mode and §7.6 response-bound vectors each have a request from
  `wire.SealRequestFor`, a buffered response and a three-frame stream.
  The bound vector uses `wire.AttachResponseBind` and
  `wire.NewBoundResponseSealerFor`. Both sets use synthetic keys and EIP-191
  signatures over the library's §8 proof text. The Rust tests open the frames
  and check each binding and signature.
- `quote-dstack-0.6.5.hex` comes from a public 0G broker quote captured on
  2026-11-23 PST. The encryption key and signer in `report_data` were zeroed.
  The verifier tests stub DCAP and patch the report to exercise both
  `mr_config_id` layouts using synthetic boot events.

`tests/0g_private_computer_live.rs` covers authenticated interop with a live
0G deployment.
