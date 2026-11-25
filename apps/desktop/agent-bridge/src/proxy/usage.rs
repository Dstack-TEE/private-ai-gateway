use super::*;

// A downstream disconnect drops the body without polling it to EOF.
pub(super) struct UsageReport {
    pub(super) capture: Option<UsageCapture>,
    pub(super) state: Arc<ProxyState>,
    pub(super) event: ProxyEvent,
}

impl Drop for UsageReport {
    fn drop(&mut self) {
        if let Some(capture) = self.capture.take() {
            // Record known billing refusals without altering the response
            // delivered to the agent or logging arbitrary upstream text.
            if self.event.detail.is_empty() && !capture.streamed && !capture.body_overflow {
                if let Some(refusal) = subscription_refusal(self.event.status, &capture.body) {
                    self.event.detail = refusal;
                }
            }
            let usage = capture.finish();
            self.event.input_tokens = usage.input_tokens;
            self.event.output_tokens = usage.output_tokens;
            self.event.cache_read_tokens = usage.cache_read_tokens;
            self.event.cache_write_tokens = usage.cache_write_tokens;
            self.event.cost_usd = usage.cost_usd;
            self.event.at = now_secs();
            self.state.emit(self.event.clone());
        }
    }
}

fn subscription_refusal(status: u16, body: &[u8]) -> Option<String> {
    if !matches!(status, 402 | 403 | 429) || body.len() > 4096 {
        return None;
    }
    let envelope: Value = serde_json::from_slice(body).ok()?;
    let message = envelope.get("error")?.get("message")?.as_str()?;
    match (status, message) {
        (402, "Subscription is not active. Renew your subscription or use a credits key.")
        | (403, "This model is not included in your subscription plan. Use a credits key for this model.")
        | (403, "The requested provider is not available on your subscription plan.")
        | (429, "Too many concurrent requests for this subscription. Please retry shortly.")
        | (429, "Subscription usage limit reached.") => Some(message.to_owned()),
        (429, _) => {
            let reset = message.strip_prefix("Subscription usage limit reached. Resets at ")?.strip_suffix('.')?;
            desktop_core::account::subscription_reset_at(reset)?;
            Some(message.to_owned())
        }
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct UsageValues {
    pub(super) input_tokens: Option<u64>,
    pub(super) output_tokens: Option<u64>,
    pub(super) cache_read_tokens: Option<u64>,
    pub(super) cache_write_tokens: Option<u64>,
    pub(super) cost_usd: Option<f64>,
}

impl UsageValues {
    pub(super) fn merge(&mut self, newer: Self) {
        if newer.input_tokens.is_some() {
            self.input_tokens = newer.input_tokens;
        }
        if newer.output_tokens.is_some() {
            self.output_tokens = newer.output_tokens;
        }
        if newer.cache_read_tokens.is_some() {
            self.cache_read_tokens = newer.cache_read_tokens;
        }
        if newer.cache_write_tokens.is_some() {
            self.cache_write_tokens = newer.cache_write_tokens;
        }
        if newer.cost_usd.is_some() {
            self.cost_usd = newer.cost_usd;
        }
    }
}

pub(super) struct UsageCapture {
    pub(super) streamed: bool,
    pub(super) body: Vec<u8>,
    pub(super) lines: DataLines,
    pub(super) latest: UsageValues,
    pub(super) body_overflow: bool,
    max_body_bytes: usize,
}

impl UsageCapture {
    pub(super) fn new(streamed: bool, surface: Surface) -> Self {
        Self {
            streamed,
            body: Vec::new(),
            lines: DataLines::new(),
            latest: UsageValues::default(),
            body_overflow: false,
            max_body_bytes: if surface == Surface::Images {
                MAX_IMAGE_USAGE_CAPTURE_BYTES
            } else {
                MAX_USAGE_CAPTURE_BYTES
            },
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        if self.streamed {
            let latest = &mut self.latest;
            self.lines.push(bytes, |data| merge_event(latest, data));
        } else if !self.body_overflow {
            if self.body.len().saturating_add(bytes.len()) <= self.max_body_bytes {
                self.body.extend_from_slice(bytes);
            } else {
                self.body = Vec::new();
                self.body_overflow = true;
            }
        }
    }

    pub(super) fn finish(mut self) -> UsageValues {
        if self.streamed {
            let latest = &mut self.latest;
            self.lines.finish(|data| merge_event(latest, data));
            self.latest
        } else if self.body_overflow {
            UsageValues::default()
        } else {
            decode_usage(&self.body).unwrap_or_default()
        }
    }
}

#[derive(Deserialize)]
pub(super) struct UsageNode {
    pub(super) usage: Option<serde_json::Map<String, Value>>,
}

#[derive(Deserialize)]
pub(super) struct UsageEnvelope {
    pub(super) usage: Option<serde_json::Map<String, Value>>,
    pub(super) response: Option<UsageNode>,
    pub(super) message: Option<UsageNode>,
}

pub(super) fn decode_usage(bytes: &[u8]) -> Option<UsageValues> {
    // Ignore output/tool payloads without constructing a second in-memory response tree.
    let envelope: UsageEnvelope = serde_json::from_slice(bytes).ok()?;
    let usage = envelope
        .usage
        .or_else(|| envelope.response.and_then(|response| response.usage))
        .or_else(|| envelope.message.and_then(|message| message.usage))?;
    Some(parse_usage(&usage))
}

pub(super) fn parse_usage(usage: &serde_json::Map<String, Value>) -> UsageValues {
    let cache_read = token(usage, "cache_read_input_tokens")
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(number_u64)
        })
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(number_u64)
        });
    UsageValues {
        input_tokens: token(usage, "prompt_tokens").or_else(|| token(usage, "input_tokens")),
        output_tokens: token(usage, "completion_tokens").or_else(|| token(usage, "output_tokens")),
        cache_read_tokens: cache_read,
        cache_write_tokens: token(usage, "cache_creation_input_tokens"),
        cost_usd: usage.get("cost").and_then(number_f64),
    }
}

pub(super) fn token(usage: &serde_json::Map<String, Value>, name: &str) -> Option<u64> {
    usage.get(name).and_then(number_u64)
}

pub(super) fn number_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

pub(super) fn number_f64(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    (value.is_finite() && value >= 0.0).then_some(value)
}

/// Takes the usage an event's `data` reports, if any; `[DONE]` carries none.
fn merge_event(latest: &mut UsageValues, data: &str) {
    if let Some(usage) = decode_usage(data.as_bytes()) {
        latest.merge(usage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_refusals_record_safe_messages_and_quota_reset() {
        let parse = |status, message| {
            subscription_refusal(
                status,
                json!({"error": {"message": message}})
                    .to_string()
                    .as_bytes(),
            )
        };
        for (status, message) in [
            (402, "Subscription is not active. Renew your subscription or use a credits key."),
            (403, "This model is not included in your subscription plan. Use a credits key for this model."),
            (403, "The requested provider is not available on your subscription plan."),
            (429, "Too many concurrent requests for this subscription. Please retry shortly."),
            (429, "Subscription usage limit reached."),
            (429, "Subscription usage limit reached. Resets at 2026-11-27T09:00:00.000Z."),
        ] {
            assert_eq!(parse(status, message).as_deref(), Some(message));
            assert!(parse(400, message).is_none());
        }
        assert!(parse(429, "Subscription usage limit reached. Resets at tomorrow.").is_none());
        assert!(parse(403, "An arbitrary upstream error.").is_none());
    }
}
