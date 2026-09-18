//! ACI receipt canonicalization for relying-party signature verification.

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
