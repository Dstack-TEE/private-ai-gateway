# ACI client libraries

- [`verifier-ts`](verifier-ts) — `@dstack/aci-verifier`, a zero-dependency,
  Web-Crypto ACI client: verify attestation reports and receipts, and open an
  **E2EE channel** to a *verified* workload (`openE2eeChannel`) to encrypt
  request fields and decrypt replies (X25519 suite, §7).

secp256k1 and non-browser (Rust) clients are separate extensions, kept out of
the base library so the common path stays small and browser-native.
