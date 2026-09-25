//! ACI receipt canonicalization for relying-party signature verification,
//! and the channel-binding shapes receipts and sessions record.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::digest;

/// Return the canonical bytes covered by an ACI receipt signature: the
/// document without its `signature` member, encoded as JCS (§7.2).
pub fn receipt_signing_input(document: &Value) -> Result<Vec<u8>, digest::JcsError> {
    let mut unsigned = document.clone();
    if let Some(obj) = unsigned.as_object_mut() {
        obj.remove("signature");
    }
    digest::jcs_bytes(&unsigned)
}

/// Channel binding material verified before the aggregator forwards
/// sensitive bytes to an upstream (§8.2 shapes).
///
/// `tag = "type"` serializes this as a flat, self-describing object — e.g.
/// `{"type":"tls_spki_sha256","origin":..,"spki_sha256":..}` — rather than
/// serde's default externally tagged form, which would leak Rust variant
/// names; `rename_all` keeps the discriminator in the snake_case ACI JSON
/// convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelBinding {
    TlsSpkiSha256 {
        origin: String,
        spki_sha256: String,
    },
    E2eePublicKeySha256 {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_id: Option<String>,
        algorithm: String,
        public_key_sha256: String,
    },
}
