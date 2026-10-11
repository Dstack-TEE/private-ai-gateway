//! Explicit upstream deployment policy for requesting streaming usage.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{UpstreamError, UpstreamRequest};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamingUsage {
    Final,
    Continuous,
}

/// OpenAI chat and legacy completion wire surfaces, including unversioned paths.
pub(crate) fn supports_streaming_usage(path: &str) -> bool {
    path.ends_with("/chat/completions") || path.ends_with("/completions")
}

impl StreamingUsage {
    pub(crate) fn apply(self, request: &mut UpstreamRequest) -> Result<(), UpstreamError> {
        if !request
            .path
            .as_deref()
            .is_some_and(supports_streaming_usage)
        {
            return Ok(());
        }
        let mut body: Value = serde_json::from_slice(&request.body)
            .map_err(|e| UpstreamError::Routing(e.to_string()))?;
        if body.get("stream") != Some(&Value::Bool(true)) {
            return Ok(());
        }
        let options = body
            .as_object_mut()
            .ok_or_else(|| UpstreamError::Routing("request body must be an object".into()))?
            .entry("stream_options")
            .or_insert_with(|| json!({}));
        if !options.is_object() {
            *options = json!({});
        }
        let options = options.as_object_mut().expect("stream options object");
        options.insert("include_usage".into(), Value::Bool(true));
        if self == Self::Continuous {
            options.insert("continuous_usage_stats".into(), Value::Bool(true));
        }
        request.body =
            serde_json::to_vec(&body).map_err(|e| UpstreamError::Routing(e.to_string()))?;
        Ok(())
    }
}
