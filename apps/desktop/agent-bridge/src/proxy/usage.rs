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
    pub(super) line: Vec<u8>,
    pub(super) latest: UsageValues,
    pub(super) body_overflow: bool,
    pub(super) line_overflow: bool,
}

impl UsageCapture {
    pub(super) fn new(streamed: bool) -> Self {
        Self {
            streamed,
            body: Vec::new(),
            line: Vec::new(),
            latest: UsageValues::default(),
            body_overflow: false,
            line_overflow: false,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        if self.streamed {
            self.push_sse(bytes);
        } else if !self.body_overflow {
            if self.body.len().saturating_add(bytes.len()) <= MAX_USAGE_CAPTURE_BYTES {
                self.body.extend_from_slice(bytes);
            } else {
                self.body.clear();
                self.body_overflow = true;
            }
        }
    }

    pub(super) fn push_sse(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if *byte == b'\n' {
                if !self.line_overflow {
                    self.parse_sse_line();
                }
                self.line.clear();
                self.line_overflow = false;
            } else if !self.line_overflow {
                if self.line.len() < MAX_SSE_LINE_BYTES {
                    self.line.push(*byte);
                } else {
                    self.line.clear();
                    self.line_overflow = true;
                }
            }
        }
    }

    pub(super) fn parse_sse_line(&mut self) {
        let line = self.line.strip_suffix(b"\r").unwrap_or(&self.line);
        let payload = line
            .strip_prefix(b"data: ")
            .or_else(|| line.strip_prefix(b"data:"))
            .map(trim_ascii_start);
        let Some(payload) = payload else { return };
        if payload == b"[DONE]" {
            return;
        }
        if let Some(usage) = decode_usage(payload) {
            self.latest.merge(usage);
        }
    }

    pub(super) fn finish(mut self) -> UsageValues {
        if self.streamed {
            if !self.line.is_empty() && !self.line_overflow {
                self.parse_sse_line();
            }
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

pub(super) fn trim_ascii_start(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    value
}
