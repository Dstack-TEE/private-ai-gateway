//! Bounded reads shared by the canonical and legacy upstream report fetches.

/// Production reports measure about 54 KB (53,676 bytes). This leaves room
/// for JSON escaping of the 4 MiB compose and 1 MiB event log, matching clients.
pub(crate) const MAX_ATTESTATION_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AttestationBodyError {
    #[error("attestation response exceeds maximum byte size (32 MiB)")]
    TooLarge,
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
}

pub(crate) async fn read_attestation_body(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, AttestationBodyError> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ATTESTATION_RESPONSE_BYTES as u64)
    {
        return Err(AttestationBodyError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_ATTESTATION_RESPONSE_BYTES - body.len() {
            return Err(AttestationBodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
