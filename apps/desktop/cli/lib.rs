//! ACI relying-party verification and audit primitives owned by Private AI Proxy.

pub mod aci;

/// Select the provider used by the CLI and its verifier dependencies.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
