//! Aggregator-owned Prometheus metrics.
//!
//! These counters describe work performed by this ACI service. They do
//! not proxy or copy upstream-provider metrics.

use prometheus::{Encoder, IntCounterVec, Opts, Registry, TextEncoder};

use crate::aci::receipt::UpstreamVerifiedEvent;

#[derive(Clone)]
pub struct ServiceMetrics {
    registry: Registry,
    requests_total: IntCounterVec,
    upstream_verifications_total: IntCounterVec,
    upstream_responses_total: IntCounterVec,
    receipts_issued_total: IntCounterVec,
    stream_errors_total: IntCounterVec,
}

pub struct MetricsSnapshot {
    pub body: Vec<u8>,
    pub content_type: String,
}

impl ServiceMetrics {
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let requests_total = IntCounterVec::new(
            Opts::new(
                "private_ai_gateway_requests_total",
                "Inference requests accepted by this ACI aggregator.",
            ),
            &["endpoint", "mode", "e2ee"],
        )?;
        let upstream_verifications_total = IntCounterVec::new(
            Opts::new(
                "private_ai_gateway_upstream_verifications_total",
                "Upstream verification decisions made before forwarding.",
            ),
            &["result", "required"],
        )?;
        let upstream_responses_total = IntCounterVec::new(
            Opts::new(
                "private_ai_gateway_upstream_responses_total",
                "Upstream response status classes observed by this aggregator.",
            ),
            &["endpoint", "mode", "status_class", "model_id"],
        )?;
        let receipts_issued_total = IntCounterVec::new(
            Opts::new(
                "private_ai_gateway_receipts_issued_total",
                "ACI receipts signed and stored by this aggregator.",
            ),
            &["endpoint", "mode", "model_id"],
        )?;
        let stream_errors_total = IntCounterVec::new(
            Opts::new(
                "private_ai_gateway_stream_errors_total",
                "Streaming requests that ended before a receipt could be finalized.",
            ),
            &["endpoint", "kind"],
        )?;

        registry.register(Box::new(requests_total.clone()))?;
        registry.register(Box::new(upstream_verifications_total.clone()))?;
        registry.register(Box::new(upstream_responses_total.clone()))?;
        registry.register(Box::new(receipts_issued_total.clone()))?;
        registry.register(Box::new(stream_errors_total.clone()))?;

        Ok(Self {
            registry,
            requests_total,
            upstream_verifications_total,
            upstream_responses_total,
            receipts_issued_total,
            stream_errors_total,
        })
    }

    pub fn render(&self) -> Result<MetricsSnapshot, prometheus::Error> {
        let encoder = TextEncoder::new();
        let mut body = Vec::new();
        encoder.encode(&self.registry.gather(), &mut body)?;
        Ok(MetricsSnapshot {
            body,
            content_type: encoder.format_type().to_string(),
        })
    }

    pub fn record_request(&self, endpoint: &str, mode: RequestMode, e2ee_applied: bool) {
        self.requests_total
            .with_label_values(&[endpoint, mode.as_str(), bool_label(e2ee_applied)])
            .inc();
    }

    pub fn record_upstream_verification(&self, event: &UpstreamVerifiedEvent) {
        self.upstream_verifications_total
            .with_label_values(&[event.result.as_str(), bool_label(event.required)])
            .inc();
    }

    pub fn record_upstream_response(
        &self,
        endpoint: &str,
        mode: RequestMode,
        status_code: u16,
        model_id: Option<&str>,
    ) {
        self.upstream_responses_total
            .with_label_values(&[
                endpoint,
                mode.as_str(),
                status_class(status_code),
                model_id.unwrap_or("unknown"),
            ])
            .inc();
    }

    pub fn record_receipt_issued(&self, endpoint: &str, mode: RequestMode, model_id: Option<&str>) {
        self.receipts_issued_total
            .with_label_values(&[endpoint, mode.as_str(), model_id.unwrap_or("unknown")])
            .inc();
    }

    pub fn record_stream_error(&self, endpoint: &str, kind: StreamErrorKind) {
        self.stream_errors_total
            .with_label_values(&[endpoint, kind.as_str()])
            .inc();
    }
}

#[derive(Clone, Copy)]
pub enum RequestMode {
    Buffered,
    Streaming,
}

impl RequestMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Buffered => "buffered",
            Self::Streaming => "streaming",
        }
    }
}

#[derive(Clone, Copy)]
pub enum StreamErrorKind {
    UpstreamNon2xx,
    UpstreamRead,
    ReceiptFinalize,
    E2ee,
}

impl StreamErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamNon2xx => "upstream_non_2xx",
            Self::UpstreamRead => "upstream_read",
            Self::ReceiptFinalize => "receipt_finalize",
            Self::E2ee => "e2ee",
        }
    }
}

fn bool_label(v: bool) -> &'static str {
    if v {
        "true"
    } else {
        "false"
    }
}

fn status_class(status_code: u16) -> &'static str {
    match status_code {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}
