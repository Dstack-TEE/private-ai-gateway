# Provider Review Reports

Provider-owned research reports derived from the router-mode review.

These reports combine the two review lanes:

- soundness, privacy, and source provenance
- load balancing, routing, and cache locality

Source lane reports:

- [router-mode-soundness.md](../router-mode-soundness.md)
- [router-mode-load-balancing-cache.md](../router-mode-load-balancing-cache.md)

Provider reports:

- [audit-criteria.md](audit-criteria.md)
- [tinfoil-router-mode.md](tinfoil-router-mode.md)
- [near-ai-router-mode.md](near-ai-router-mode.md)
- [chutes-e2ee.md](chutes-e2ee.md)
- [secret-ai.md](secret-ai.md)

Chutes is not a router-mode provider. Its report covers direct E2EE instance
binding, catalog risks, and nonce-throughput limits.

SecretAI is not a router-mode provider either. Its report covers single-VM
SEV-SNP attestation with `report_data = sha256(tls_cert) || gpu_nonce`
binding, compose-into-cmdline launch-measurement binding, and the
`secret-ai-caddy` plaintext-egress review.
