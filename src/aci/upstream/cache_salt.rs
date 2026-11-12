//! Apply the selected provider's cache partition before receipt hashing.

use serde_json::Value;

use crate::middleware::cache_salt::CacheSalt;

use super::{UpstreamError, UpstreamRequest};

/// The supported field and the provider's stable serde name.
#[derive(Debug, Clone, Copy)]
pub enum CacheSaltField {
    CacheSalt(&'static str),
    UserCacheSecret(&'static str),
}

impl CacheSaltField {
    pub(crate) fn apply(
        self,
        request: &mut UpstreamRequest,
        salt: CacheSalt,
    ) -> Result<(), UpstreamError> {
        if request.form.is_some()
            || !request.path.as_deref().is_some_and(|path| {
                path.ends_with("/completions")
                    || path.ends_with("/responses")
                    || path.ends_with("/messages")
            })
        {
            return Ok(());
        }
        let mut body: Value = serde_json::from_slice(&request.body)
            .map_err(|e| UpstreamError::Routing(e.to_string()))?;
        let object = body
            .as_object_mut()
            .ok_or_else(|| UpstreamError::Routing("request body must be an object".into()))?;
        object.remove("cache_salt");
        object.remove("user_cache_secret");
        let (field, provider) = match self {
            Self::CacheSalt(provider) => ("cache_salt", provider),
            Self::UserCacheSecret(provider) => ("user_cache_secret", provider),
        };
        object.insert(field.into(), Value::String(salt.for_provider(provider)));
        request.body = serde_json::to_vec(&body)
            .map_err(|e| UpstreamError::Routing(e.to_string()))?
            .into();
        Ok(())
    }
}
