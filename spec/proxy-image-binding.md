# Co-deployed Proxy Image Channel Binding

> **Binding type:** `proxy_image_sha256`
> **Status:** draft extension to `aci/1`
> **Audience:** aggregator implementers and session verifiers
> **Conformance language:** MUST, SHOULD, and MAY are used in the RFC 2119 sense

This document defines the `proxy_image_sha256` channel binding for
[Attested Confidential Inference (ACI)](aci.md) attested sessions
([§8](aci.md#8-attested-sessions)). It is a published extension to the
channel binding types in [Appendix B](aci.md#appendix-b-protocol-constants-and-extension-points),
not part of the core `aci/1` protocol. A verifier that does not implement it
treats the binding as not enforceable.

## 1. Purpose

Some providers protect inference with their own attestation and end-to-end
encryption, implemented by an official client proxy rather than an attested
TLS key or a published E2EE key. An aggregator can run that proxy inside its
own attested workload and send plaintext only to it. The upstream hop is then
bound by the aggregator's own measurement: the proxy image, its private
endpoint, and the credential it uses. This binding records that deployment in
the session.

## 2. Shape

```json
{ "type": "proxy_image_sha256", "provider": "<label>", "proxy_image_digest": "sha256:<hex>", "credential_sha256": "<hex>" }
```

| Member | Value |
| --- | --- |
| `type` | Exactly `proxy_image_sha256`. |
| `provider` | Provider label, such as `privatemode`. |
| `proxy_image_digest` | OCI image digest of the proxy, `sha256:` followed by 64 lowercase hex characters. |
| `credential_sha256` | SHA-256 of the exact provider credential bytes the proxy uses, as 64 lowercase hex characters. |

## 3. Service requirements

An aggregator that records this binding MUST ensure that:

1. The proxy is part of the aggregator's attested deployment, so the
   deployment measurement (for dstack, the compose hash in RTMR3) covers the
   proxy image pinned by `proxy_image_digest` and its configuration.
2. The proxy's endpoint is private to that deployment and fixed by measured
   configuration. Mutable routing configuration cannot change it, and no
   port publishes it.
3. The credential the proxy uses hashes to `credential_sha256`, and the
   aggregator checks this before serving.
4. Inference reaches the proxy only through handlers the proxy encrypts to
   the provider. The aggregator does not follow redirects or use ambient HTTP
   proxies on that hop.
5. A request is forwarded only when the verified event's bindings equal
   exactly this binding for the deployment.

## 4. Claims

The binding proves the deployment, not which provider workload served a
request. A session MUST NOT derive claims beyond what the proxy's own
verification establishes before serving. A provider manifest or other
evidence the proxy fetched is observational unless the proxy binds it to the
secret used for the cited request; a session MAY report it in
`claims.extra`, labelled as unbound.

## 5. Verifying a session

A verifier that implements this binding, in addition to
[§9.2](aci.md#92-verify-a-session-aggregators):

1. Verifies the aggregator's report and accepts its deployment measurement
   under local policy, for example a compose hash whose content the verifier
   reviewed.
2. Confirms that the accepted deployment pins `proxy_image_digest` for the
   proxy and binds `credential_sha256`, and that the session's binding equals
   those values.
3. Applies the provider's own documented trust model to the proxy image, for
   example by reviewing the image or its published source.

## 6. Test vector

The [§3 session vector](test-vectors.md#3-attested-session--session_id-spec-8)
with its binding replaced by:

```text
{"credential_sha256":"f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2","provider":"privatemode","proxy_image_digest":"sha256:e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1","type":"proxy_image_sha256"}
```

has the id:

```text
session_id = c9e76e6247b2bf28ce034b7c3b4866b9fe9ce9177e9858ad999503e63ed4f0df
```

[`tools/gen-vectors.py`](tools/gen-vectors.py) and `tests/spec_vectors.rs`
reproduce it.
