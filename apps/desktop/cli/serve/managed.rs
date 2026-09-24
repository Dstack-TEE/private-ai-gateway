//! Managed mode: the per-user backend runs the verifier in process and hands
//! each verified identity to the agent-bridge Local API proxy.

use std::future::Future;
use std::sync::Arc;

use agent_bridge::proxy::{
    ForwardContext, ProxyEvent, VerifiedResponse, VerifiedService, MAX_BODY_BYTES,
};
use axum::body::{to_bytes, Body};
use axum::http::StatusCode;
use desktop_runtime::verifier_session::{
    VerifierConfig, VerifierEvent, VerifierEventSink, VerifierLauncher, VerifierTask,
};

use super::{initialize, proxy_request, text_response, ProxyState, Reporter, VerifierOptions};

impl VerifiedService for ProxyState {
    fn call(
        self: Arc<Self>,
        request: axum::http::Request<Body>,
        context: Option<ForwardContext>,
    ) -> VerifiedResponse {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = match tokio::select! {
                _ = self.shutdown.cancelled() => {
                    return text_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "verifier stopped before the request could be read\n",
                    );
                }
                result = to_bytes(body, MAX_BODY_BYTES) => result,
            } {
                Ok(body) => body,
                Err(_) => {
                    return text_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request body exceeds the verifier limit\n",
                    )
                }
            };
            proxy_request(self, parts.method, parts.uri, parts.headers, body, context).await
        })
    }
}

pub struct InProcessVerifierLauncher {
    runtime: tokio::runtime::Handle,
}

impl InProcessVerifierLauncher {
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self { runtime }
    }

    pub(super) fn spawn_task<F, Fut>(
        &self,
        events: VerifierEventSink,
        worker: F,
    ) -> Box<dyn VerifierTask>
    where
        F: FnOnce(tokio_util::sync::CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let worker = self.runtime.spawn(worker(cancellation.clone()));
        self.runtime.spawn(async move {
            let error = match worker.await {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(error) if error.is_cancelled() => None,
                Err(error) => Some(format!("Verifier task failed: {error}")),
            };
            events(VerifierEvent::Terminated { error });
        });
        Box::new(InProcessVerifierTask { cancellation })
    }
}

struct InProcessVerifierTask {
    cancellation: tokio_util::sync::CancellationToken,
}

impl VerifierTask for InProcessVerifierTask {
    fn stop(&mut self) -> Result<(), String> {
        self.cancellation.cancel();
        Ok(())
    }
}

impl Drop for InProcessVerifierTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl VerifierLauncher for InProcessVerifierLauncher {
    fn spawn(
        &self,
        config: VerifierConfig,
        events: VerifierEventSink,
        requests: tokio::sync::mpsc::Sender<ProxyEvent>,
    ) -> Result<Box<dyn VerifierTask>, String> {
        let worker_events = events.clone();
        let task = self.spawn_task(events, move |cancelled| async move {
            let reporter = managed_reporter(requests);
            let initialized = tokio::select! {
                _ = cancelled.cancelled() => return Ok::<(), String>(()),
                result = initialize(
                    VerifierOptions {
                        base_url: config.remote_url.clone(),
                        accepted_composes: Vec::new(),
                        require_production_os: config.require_production_os,
                        enforce_verified: true,
                        fixed_pins: Vec::new(),
                        required_claims: Vec::new(),
                        print_progress: false,
                    },
                    reporter,
                    worker_events.clone(),
                    cancelled.clone(),
                ) => result,
            };
            match initialized {
                Ok((state, identity, remote_url)) => {
                    worker_events(VerifierEvent::Ready {
                        identity,
                        remote_url,
                        service: state,
                    });
                    cancelled.cancelled().await;
                    Ok::<(), String>(())
                }
                Err(error) => {
                    worker_events(VerifierEvent::Fatal {
                        message: error.clone(),
                    });
                    Err(error)
                }
            }
        });
        Ok(task)
    }
}

pub(super) fn managed_reporter(events: tokio::sync::mpsc::Sender<ProxyEvent>) -> Reporter {
    // Receipt verdicts are correctness-sensitive. Serialize them through a
    // lossless handoff and await the runtime's bounded queue off the response
    // hot path. The agent-bridge proxy's own usage-progress emitter remains
    // intentionally lossy when its bounded queue is saturated.
    let (verdicts, mut pending) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = pending.recv().await {
            if events.send(event).await.is_err() {
                break;
            }
        }
    });
    Arc::new(move |outcome| {
        let Some(context) = outcome.context else {
            return;
        };
        let _ = verdicts.send(ProxyEvent {
            generation: context.generation,
            request_id: context.request_id,
            session_id: context.session_id,
            agent: Some(context.agent),
            method: outcome.method.as_str().to_string(),
            path: outcome.path,
            model: context.model,
            status: outcome.status,
            streamed: outcome.streamed,
            receipt_id: outcome.receipt_id,
            verified: outcome.verified,
            detail: outcome.detail,
            at: context.at,
            local_policy_applied: Some(outcome.local_policy_applied),
            rewritten: outcome.rewritten,
            left_device: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_usd: None,
        });
    })
}
