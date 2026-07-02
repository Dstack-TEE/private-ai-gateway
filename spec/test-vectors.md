# ACI Test Vectors

Deterministic vectors for the digest, canonicalization, and signature
constructions in the [ACI Spec](README.md). An implementation that
reproduces every value below byte-for-byte has its canonicalization and
binding logic right — which is where independent implementations diverge in
practice.

Conventions: JCS is RFC 8785; every string here is printable ASCII and every
number an integer, so any conformant JCS implementation produces identical
bytes. Digests are `sha256:<lowercase-hex>` unless shown raw. The vectors use
the recommended cipher suites — Ed25519 signatures (deterministic) and the
X25519 E2EE suite. `ecdsa-secp256k1` and `secp256k1-*` vectors are not
included; verify those against the reference implementation.

## Fixed keys

| Key | Seed (private) | Public key (raw, hex) |
| --- | --- | --- |
| identity (`ed25519`) | `0101…01` (32 × `0x01`) | `8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c` |
| receipt `receipt-1` (`ed25519`) | `0202…02` (32 × `0x02`) | `8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394` |

The E2EE and TLS keyset entries below are placeholders (not valid curve
points / real SPKIs); digest vectors do not require validity.

## 1. `workload_id` (spec §4.1)

JCS of the identity public key object:

```text
{"algo":"ed25519","public_key":"8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"}
```

```text
workload_id = sha256:57c2c8fa98bcf11441f1eff9ef087db67a5560a026082e96903e15365677b8c0
```

## 2. Workload keyset digest (spec §4.2)

Keyset (shown readable; digest is over its JCS form):

```json
{
  "workload_identity": {
    "public_key": { "algo": "ed25519", "public_key": "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c" },
    "subject": null
  },
  "keyset_epoch": { "version": 1, "not_after": 1800000000 },
  "receipt_signing_keys": [
    { "key_id": "receipt-1", "algo": "ed25519", "public_key": "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394" }
  ],
  "e2ee_public_keys": [
    { "key_id": "e2ee-1", "algo": "x25519-aes-256-gcm-hkdf-sha256", "public_key": "abab…ab (32 placeholder bytes: 32 × ab)" }
  ],
  "tls_public_keys": [
    { "spki_sha256": "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0", "domain": "api.example.com" }
  ]
}
```

```text
workload_keyset_digest = sha256:f2fba7e1b1451e0c0231df624f293407692ef939d3e0e55bca723131bea3f1ff
```

## 3. Attestation statement and `report_data` (spec §4.4)

With `nonce` = `test-nonce`, the JCS bytes hashed into report data are:

```text
{"nonce":"test-nonce","purpose":"aci.report_data.v1","workload_id":"sha256:57c2c8fa98bcf11441f1eff9ef087db67a5560a026082e96903e15365677b8c0","workload_keyset_digest":"sha256:f2fba7e1b1451e0c0231df624f293407692ef939d3e0e55bca723131bea3f1ff"}
```

```text
report_data (nonce "test-nonce") = 0b8cc28d7e989a88b1e969af20aa2b224afdc2c99f24c97c31a4af330c964ecf
report_data (nonce omitted → null) = e1818eadad3c28375c625e2fa2d2ffd983d2760c84ce17f8527ddcac884c21b9
```

## 4. Keyset endorsement and revocation (spec §4.3, §4.7)

Endorsement payload bytes (exactly these, no surrounding whitespace):

```text
{"purpose":"aci.keyset.endorsement.v1","workload_keyset_digest":"sha256:f2fba7e1b1451e0c0231df624f293407692ef939d3e0e55bca723131bea3f1ff"}
```

```text
keyset_endorsement.value (ed25519, identity key) = 64e0a4f5d7af28dfdacc102d14c13470b4ddbd90708e190fc0e787f07b36f20eda0ef1f42ea96b8a7f290eb64a918574dc914ce06b6ea023d2153275f06fd201
```

Revocation payload (same shape, different purpose tag):

```text
{"purpose":"aci.keyset.revocation.v1","workload_keyset_digest":"sha256:f2fba7e1b1451e0c0231df624f293407692ef939d3e0e55bca723131bea3f1ff"}
```

```text
keyset_revocation.value (ed25519, identity key) = 5f30e02aa53bb628c7f6410636e9f5e33402d2b0b416a6ed278ea3e6e40b48a9af6ba6e5e55abb89a7ad4627eca444a73cad9d25e22bf239c9c6b362d48ed50f
```

## 5. Attested session `session_id` (spec §9.2)

Evidence bytes = ASCII `example-evidence`, so
`evidence.digest = sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d`.

Content-addressing material (JCS bytes; note `identity: null` for the
absent field, and claim objects carrying `source`/`reason` only when not
`unknown`):

```text
{"channel_binding":[{"origin":"https://upstream.example.com","spki_sha256":"d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1","type":"tls_spki_sha256"}],"claims":{"gpu_attested":{"status":"unknown"},"model_weights_provenance":{"status":"unknown"},"os_known_good":{"status":"unknown"},"serving_software_known_good":{"status":"unknown"},"tcb_up_to_date":{"status":"unknown"},"tee_attested":{"reason":"example quote verified","source":"hardware_proven","status":"asserted"}},"endpoint":"https://upstream.example.com","evidence_digest":"sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d","identity":null,"provider":"demo-upstream","verifier_id":"example/1"}
```

```text
session_id = as_d850af230a623da93279f86922f2d15aeb0a05c3c448feac9198c5b2b0a3b9c3
```

## 6. Receipt signing (spec §8.5)

Request body bytes (ASCII):

```text
{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}
→ body_hash = sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b
```

Response body bytes (ASCII, plaintext case so cleartext and wire hashes are
equal):

```text
{"choices":[],"id":"chatcmpl-123"}
→ sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61
```

Canonical bytes for signing — the JCS of the whole receipt with
`signature.value` omitted (`algo` and `key_id` retained). The receipt carries
the top-level `model` (the user-requested model), `served_at` 1750000000, the
§5 session id in its `upstream.verified` event, and the §1/§2 identity values:

```text
{"api_version":"aci/1","chat_id":"chatcmpl-123","endpoint":"/v1/chat/completions","event_log":[{"body_hash":"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b","seq":0,"type":"request.received"},{"body_hash":"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b","seq":1,"type":"request.forwarded"},{"channel_bindings":[{"origin":"https://upstream.example.com","spki_sha256":"d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1","type":"tls_spki_sha256"}],"claims":{"gpu_attested":{"status":"unknown"},"model_weights_provenance":{"status":"unknown"},"os_known_good":{"status":"unknown"},"serving_software_known_good":{"status":"unknown"},"tcb_up_to_date":{"status":"unknown"},"tee_attested":{"reason":"example quote verified","source":"hardware_proven","status":"asserted"}},"model_id":"demo-model","provider":null,"provider_claims":null,"reason":null,"required":true,"result":"verified","seq":2,"session_id":"as_d850af230a623da93279f86922f2d15aeb0a05c3c448feac9198c5b2b0a3b9c3","type":"upstream.verified","upstream_name":"demo-upstream","url_origin":"https://upstream.example.com","verifier_id":"example/1"},{"cleartext_hash":"sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61","seq":3,"type":"response.returned","wire_hash":"sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61"}],"method":"POST","model":"demo-model","receipt_id":"rcpt-0001","served_at":1750000000,"signature":{"algo":"ed25519","key_id":"receipt-1"},"workload_id":"sha256:57c2c8fa98bcf11441f1eff9ef087db67a5560a026082e96903e15365677b8c0","workload_keyset_digest":"sha256:f2fba7e1b1451e0c0231df624f293407692ef939d3e0e55bca723131bea3f1ff"}
```

```text
sha256(canonical_bytes)      = 167f07a49364efbc74649e7b05555248f46f944262a96934a4b4e5f359f95cfa
signature.value (ed25519, receipt-1) = 84568f9eed0981d275407e3ea9b03d7f6c9a428923ac99c45b9ef94aa8bada6a39b5a63410fb48eb8057a6d4f8d2fb5a2ba28bc8f94d3c707e7fb7f360f39d00
```

## 7. E2EE AAD (spec §7.3)

Request AAD for the first message's whole-string content
(`field = messages.0.content`), X25519 suite, nonce `6e6f6e63652d31323334`,
timestamp 1750000000 — the AAD bytes are exactly:

```text
{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"messages.0.content","model":"demo-model","nonce":"6e6f6e63652d31323334","purpose":"aci.e2ee.request.v2","ts":1750000000}
```

Response AAD for `choices.0.message.content` of response id `chatcmpl-123`
(same request nonce/timestamp, request model):

```text
{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"choices.0.message.content","id":"chatcmpl-123","model":"demo-model","nonce":"6e6f6e63652d31323334","purpose":"aci.e2ee.response.v2","ts":1750000000}
```
