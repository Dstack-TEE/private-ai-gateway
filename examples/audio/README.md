The upstream example omits credentials. Provision `bearer_token` through the
existing secret-backed runtime configuration workflow; do not commit it.
`transcribe.sh` reads the caller credential and file path from environment
variables. For production, use a released verifying client with multipart
capability negotiation and receipt policy checks.

The control plane must explicitly list `/v1/audio/transcriptions` in the
candidate's `supportedEndpoints` and supply a verified `audioCostPerSecond`.
NEAR's native JSON contains only `text`; the example enables
`audio_force_verbose_json` to obtain duration while preserving the caller's
output format. NEAR's rate still requires verification. Privatemode is pending
verification and remains blocked.
