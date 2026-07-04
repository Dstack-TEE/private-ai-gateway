# ACI E2EE client libraries

Client-side end-to-end encryption for the ACI protocol (spec §7). These
libraries encrypt the content-bearing fields of a request to the attested
service E2EE key, so plaintext is readable only inside the TEE even when TLS
terminates elsewhere.

Both cipher suites of §7.1 are supported; the client picks one by the `algo`
of the keyset entry it encrypts to:

- `x25519-aes-256-gcm-hkdf-sha256` (RECOMMENDED)
- `secp256k1-aes-256-gcm-hkdf-sha256`

Each encrypted field value is the lowercase hex of
`ephemeral_public_key || aes_gcm_nonce(12) || ciphertext || tag(16)`, bound to
its location and request context by an AES-GCM AAD (§7.3). The AAD is the JCS
canonicalization of a purpose-tagged object whose `field` is the location's
field path (§7.2), e.g. `messages.0.content`,
`messages.1.content.0.image_url.url`, `prompt`, `input.2`.

The [Rust](rust/aci-e2ee) and [TypeScript](typescript) implementations are
verified against each other with a shared byte-exact known-answer test, and
both reproduce the AAD test vectors in `spec/test-vectors.md §7`.

## Typical flow

1. Fetch and verify the attestation report; pick an `e2ee_public_keys` entry
   whose `algo` is a §7.1 suite. Send its public key as `X-Model-Pub-Key`.
2. For each field you send, encrypt the value at its field path and put the hex
   ciphertext back in place, keeping the JSON OpenAI-compatible.
3. Send `X-E2EE-Version: 2`, `X-Client-Pub-Key` (your key, same curve),
   `X-E2EE-Nonce`, and `X-E2EE-Timestamp`. The nonce is a fresh 32-byte CSPRNG
   value as 64 lowercase hex characters (§7.5); use `generate_nonce()` /
   `generateNonce()`. Response fields are encrypted to your client key; decrypt
   them with the matching `responseAad`.

## Rust

```rust
use aci_e2ee::{encrypt_request_field, generate_nonce, ALGO_X25519};

let nonce = generate_nonce(); // 64 lowercase hex chars for X-E2EE-Nonce
let ciphertext_hex = encrypt_request_field(
    service_key_hex,        // X-Model-Pub-Key you selected
    ALGO_X25519,            // its algo
    "gpt-x",                // request `model`, byte-exact
    "messages.0.content",   // field path (§7.2)
    &nonce,                 // X-E2EE-Nonce
    timestamp,              // X-E2EE-Timestamp
    b"hello",
)?;
```

## TypeScript

```ts
import { encryptRequestField, generateNonce, ALGO_X25519 } from "@aci/e2ee";

const nonce = generateNonce(); // 64 lowercase hex chars for X-E2EE-Nonce
const ciphertextHex = encryptRequestField(
  serviceKeyHex,          // X-Model-Pub-Key you selected
  ALGO_X25519,            // its algo
  "gpt-x",                // request model, byte-exact
  "messages.0.content",   // field path (§7.2)
  nonce,                  // X-E2EE-Nonce
  timestamp,              // X-E2EE-Timestamp
  new TextEncoder().encode("hello"),
);
```

Both expose the lower-level `encrypt(serviceKey, algo, plaintext, aad)` plus
`requestAad` / `responseAad` if you need to drive the AAD yourself (for example
to decrypt response fields).
