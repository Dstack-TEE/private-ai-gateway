//! Time-controlled integration fixture for admission, response ownership and retention.
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use private_ai_gateway::aci::receipt::UpstreamVerifiedEvent;
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, ChatCompletionRequest, Clock, ForwardCandidate,
    GatewayRequestContext, InMemoryReceiptStore, UpstreamVerificationRequest, UpstreamVerifier,
};

use crate::common::{verified_event, StaticKeyProvider, StubQuoter};

pub const REQUEST: &[u8] = br#"{"model":"model-a","messages":[]}"#;
pub const RESPONSE: &[u8] = br#"{"id":"chat-admitted","model":"model-a","choices":[]}"#;
pub const SSE: &[u8] =
    b"data: {\"id\":\"chat-admitted\",\"model\":\"model-a\"}\n\ndata: [DONE]\n\n";

pub struct TestClock(pub AtomicU64);
impl TestClock {
    pub fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }
}
impl Clock for TestClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub enum Attempt {
    Response { status: u16, at: u64 },
    Pending,
}

pub struct Backend {
    clock: Arc<TestClock>,
    pub attempts: Mutex<VecDeque<Attempt>>,
    pub events: Mutex<Vec<UpstreamVerifiedEvent>>,
    pub instance: Mutex<Option<String>>,
}
impl Backend {
    async fn respond(
        &self,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.events.lock().unwrap().push(event.clone());
        let attempt = self
            .attempts
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted attempt");
        match attempt {
            Attempt::Pending => std::future::pending().await,
            Attempt::Response { status, at } => {
                self.clock.set(at);
                Ok(UpstreamResponse {
                    status_code: status,
                    body: RESPONSE.to_vec(),
                    headers: HashMap::new(),
                    served_instance_id: self.instance.lock().unwrap().clone(),
                })
            }
        }
    }
}
#[async_trait]
impl UpstreamBackend for Backend {
    fn url_origin(&self) -> Option<&str> {
        None
    }
    fn name(&self) -> &str {
        "admission-test"
    }
    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        // A later unrouteable candidate must not erase a retained response.
        if req.target_route_id.as_deref() == Some("unrouteable") {
            self.clock.set(130);
            return Err(UpstreamError::Routing("test route unavailable".to_string()));
        }
        Ok(PreparedUpstreamRequest {
            request: req,
            upstream_name: self.name().to_string(),
            model_id: "model-a".to_string(),
            url_origin: None,
            route_id: None,
            is_tee: Some(true),
        })
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        panic!("must use verified forwarding")
    }
    async fn forward_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.respond(event).await
    }
    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let response = self.forward_verified_prepared(req, event).await?;
        Ok(UpstreamStreamResponse {
            status_code: response.status_code,
            headers: response.headers,
            body: Box::pin(futures_util::stream::iter([Ok(bytes::Bytes::from_static(
                SSE,
            ))])),
            served_instance_id: response.served_instance_id,
        })
    }
}

pub struct CachedVerifier(pub Mutex<UpstreamVerifiedEvent>);
#[async_trait]
impl UpstreamVerifier for CachedVerifier {
    async fn verify(&self, _request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.0.lock().unwrap().clone()
    }
    fn cached(&self, _request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        Some(self.0.lock().unwrap().clone())
    }
}

pub struct Harness {
    pub service: AciService,
    pub backend: Arc<Backend>,
    pub clock: Arc<TestClock>,
    pub verifier: Arc<CachedVerifier>,
}
impl Harness {
    pub fn new(attempts: Vec<Attempt>) -> Self {
        let clock = Arc::new(TestClock(AtomicU64::new(100)));
        let backend = Arc::new(Backend {
            clock: clock.clone(),
            attempts: Mutex::new(attempts.into()),
            events: Mutex::new(Vec::new()),
            instance: Mutex::new(None),
        });
        let verifier = Arc::new(CachedVerifier(Mutex::new(verified_event(
            "admission-test",
            "model-a",
        ))));
        let mut config = AciServiceConfig::for_test();
        config.attested_session_ttl_seconds = 10;
        config.receipt_ttl_seconds = 5;
        let service = AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            backend.clone(),
            verifier.clone(),
            Arc::new(InMemoryReceiptStore::default()),
            config,
            clock.clone(),
        )
        .unwrap();
        Self {
            service,
            backend,
            clock,
            verifier,
        }
    }
    pub fn listed(&self) -> Vec<private_ai_gateway::aggregator::session::AttestedSession> {
        self.service
            .list_current_sessions(&[UpstreamVerificationRequest {
                upstream_name: "admission-test".to_string(),
                model_id: "model-a".to_string(),
                url_origin: None,
                forwarded_body_hash: String::new(),
                required: true,
            }])
            .unwrap()
    }
}

pub fn request(ids: Vec<String>) -> ChatCompletionRequest<'static> {
    ChatCompletionRequest {
        context: GatewayRequestContext::default(),
        endpoint_path: "/v1/chat/completions",
        received_body: REQUEST,
        forwarded_body: None,
        aci_required: true,
        aci_session_ids: ids,
        upstream_verification_event: None,
        requester: None,
        e2ee: None,
    }
}
pub fn candidate(id: &str) -> ForwardCandidate {
    ForwardCandidate {
        route_id: id.to_string(),
        path: "/v1/chat/completions",
        body: REQUEST.to_vec(),
    }
}
