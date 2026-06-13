//! Forwarding to the optional middleware over a Unix domain socket, plus
//! response finalization (receipt journaling, E2EE, cleanup).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{Stream, StreamExt};

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{
    AciService, E2eeRequestContext, E2eeResponseInfo, GatewayRequestContext,
    MiddlewareReceiptJournal, ReceiptOwner, ServiceError, ServiceResponseStream,
};

use super::backend::reqwest_response_headers;
use super::error_responses::{
    e2ee_error_response, error_response, insert_str_header, internal_error_response,
};
use super::{GatewayRequestStore, UdsMiddleware};

pub(super) async fn forward_to_middleware(
    middleware: UdsMiddleware,
    endpoint_path: &'static str,
    context: GatewayRequestContext,
    body: Vec<u8>,
    stream: bool,
    user_headers: &HeaderMap,
) -> Result<MiddlewareHttpResponse, Response> {
    let url = format!("{}{}", middleware.base_url, endpoint_path);
    let mut builder = middleware.client.post(url);
    builder = forward_user_headers(builder, user_headers);
    builder = builder
        .header("content-type", "application/json")
        .header("x-private-ai-gateway-request-id", context.request_id);
    if let Some(user_model) = context.user_model {
        builder = builder.header("x-private-ai-gateway-user-model", user_model);
    }
    middleware_completion_response(builder.body(body).send().await, stream).await
}

pub(super) async fn get_from_middleware(middleware: UdsMiddleware, path: &'static str) -> Response {
    let url = format!("{}{}", middleware.base_url, path);
    match buffered_middleware_response(middleware.client.get(url).send().await).await {
        Ok(response) => response.into_response(),
        Err(response) => response,
    }
}

pub(super) enum MiddlewareHttpResponse {
    Buffered(BufferedHttpResponse),
    Streaming(StreamingHttpResponse),
}

pub(super) struct BufferedHttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

pub(super) struct StreamingHttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: ServiceResponseStream,
}

impl IntoResponse for BufferedHttpResponse {
    fn into_response(self) -> Response {
        (self.status, self.headers, self.body).into_response()
    }
}

pub(super) async fn buffered_middleware_response(
    result: Result<reqwest::Response, reqwest::Error>,
) -> Result<BufferedHttpResponse, Response> {
    match result {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut headers = reqwest_response_headers(resp.headers());
            strip_gateway_owned_response_headers(&mut headers);
            match resp.bytes().await {
                Ok(body) => Ok(BufferedHttpResponse {
                    status,
                    headers,
                    body: body.to_vec(),
                }),
                Err(err) => Err(error_response(
                    StatusCode::BAD_GATEWAY,
                    "middleware_error",
                    format!("middleware response read failed: {err}"),
                )),
            }
        }
        Err(err) => Err(error_response(
            StatusCode::BAD_GATEWAY,
            "middleware_error",
            format!("middleware request failed: {err}"),
        )),
    }
}

pub(super) async fn middleware_completion_response(
    result: Result<reqwest::Response, reqwest::Error>,
    stream: bool,
) -> Result<MiddlewareHttpResponse, Response> {
    match result {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut headers = reqwest_response_headers(resp.headers());
            strip_gateway_owned_response_headers(&mut headers);
            if stream && is_sse_headers(&headers) {
                let body = reqwest_response_stream(resp);
                return Ok(MiddlewareHttpResponse::Streaming(StreamingHttpResponse {
                    status,
                    headers,
                    body,
                }));
            }
            match resp.bytes().await {
                Ok(body) => Ok(MiddlewareHttpResponse::Buffered(BufferedHttpResponse {
                    status,
                    headers,
                    body: body.to_vec(),
                })),
                Err(err) => Err(error_response(
                    StatusCode::BAD_GATEWAY,
                    "middleware_error",
                    format!("middleware response read failed: {err}"),
                )),
            }
        }
        Err(err) => Err(error_response(
            StatusCode::BAD_GATEWAY,
            "middleware_error",
            format!("middleware request failed: {err}"),
        )),
    }
}

pub(super) fn strip_gateway_owned_response_headers(headers: &mut HeaderMap) {
    let owned_headers: Vec<HeaderName> = headers
        .keys()
        .filter(|name| {
            let lower = name.as_str();
            lower == "x-receipt-id"
                || lower.starts_with("x-e2ee-")
                || lower.starts_with("x-aci-")
                || lower.starts_with("x-private-ai-gateway-")
        })
        .cloned()
        .collect();
    for name in owned_headers {
        headers.remove(name);
    }
}

pub(super) fn forward_user_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        if should_forward_user_header(name) {
            builder = builder.header(name, value.clone());
        }
    }
    builder
}

pub(super) fn should_forward_user_header(name: &HeaderName) -> bool {
    let lower = name.as_str();
    !matches!(
        lower,
        "host"
            | "connection"
            | "content-length"
            | "content-type"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "x-client-pub-key"
            | "x-model-pub-key"
            | "x-signing-algo"
    ) && !lower.starts_with("x-private-ai-gateway-")
        && !lower.starts_with("x-aci-")
        && !lower.starts_with("x-e2ee-")
}

pub(super) fn reqwest_response_stream(resp: reqwest::Response) -> ServiceResponseStream {
    Box::pin(resp.bytes_stream().map(|chunk| {
        chunk.map_err(|err| ServiceError::Upstream(UpstreamError::Transport(err.to_string())))
    }))
}

pub(super) fn is_sse_headers(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

pub(super) struct MiddlewareFinalizeContext {
    pub(super) service: Arc<AciService>,
    pub(super) request_store: GatewayRequestStore,
    pub(super) request_id: String,
    pub(super) receipt_journal: MiddlewareReceiptJournal,
    pub(super) endpoint_path: &'static str,
    pub(super) requester: Option<ReceiptOwner>,
    pub(super) e2ee: Option<E2eeRequestContext>,
}

pub(super) fn finalize_middleware_http_response(
    ctx: MiddlewareFinalizeContext,
    response: MiddlewareHttpResponse,
) -> Response {
    match response {
        MiddlewareHttpResponse::Buffered(response) => {
            ctx.request_store.take(&ctx.request_id);
            finalize_middleware_response(
                ctx.service,
                ctx.receipt_journal,
                response,
                ctx.endpoint_path,
                ctx.requester,
                ctx.e2ee,
            )
        }
        MiddlewareHttpResponse::Streaming(response) => {
            finalize_middleware_streaming_response(ctx, response)
        }
    }
}

pub(super) fn finalize_middleware_response(
    service: Arc<AciService>,
    receipt_journal: MiddlewareReceiptJournal,
    mut response: BufferedHttpResponse,
    endpoint_path: &'static str,
    requester: Option<ReceiptOwner>,
    e2ee: Option<E2eeRequestContext>,
) -> Response {
    let content_type = response
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let Some(draft) = receipt_journal.take() else {
        if e2ee.is_none() {
            return response.into_response();
        }
        let finalized = match service.finalize_middleware_generated_response(
            endpoint_path,
            &response.body,
            content_type.as_deref(),
            e2ee,
        ) {
            Ok(finalized) => finalized,
            Err(ServiceError::E2ee(err)) => return e2ee_error_response(err),
            Err(other) => return internal_error_response(other),
        };
        response.body = finalized.wire_body;
        apply_e2ee_response_headers(&mut response.headers, finalized.e2ee.as_ref(), false);
        return response.into_response();
    };
    let finalized = match service.finalize_middleware_receipt(
        draft,
        &response.body,
        content_type.as_deref(),
        requester,
        e2ee,
    ) {
        Ok(finalized) => finalized,
        Err(ServiceError::E2ee(err)) => return e2ee_error_response(err),
        Err(other) => return internal_error_response(other),
    };

    response.body = finalized.wire_body;
    insert_str_header(
        &mut response.headers,
        "x-receipt-id",
        &finalized.receipt.receipt_id,
    );
    apply_e2ee_response_headers(&mut response.headers, finalized.e2ee.as_ref(), true);
    response.into_response()
}

pub(super) fn finalize_middleware_streaming_response(
    ctx: MiddlewareFinalizeContext,
    mut response: StreamingHttpResponse,
) -> Response {
    let content_type = response
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let receipt_id = ctx.receipt_journal.peek_receipt_id();
    let finalized = match ctx.service.finalize_middleware_response_stream(
        ctx.receipt_journal,
        response.body,
        ctx.endpoint_path,
        content_type.as_deref(),
        ctx.requester,
        ctx.e2ee,
    ) {
        Ok(finalized) => finalized,
        Err(ServiceError::E2ee(err)) => return e2ee_error_response(err),
        Err(other) => return internal_error_response(other),
    };
    if let Some(receipt_id) = receipt_id {
        insert_str_header(&mut response.headers, "x-receipt-id", &receipt_id);
        apply_e2ee_response_headers(&mut response.headers, finalized.e2ee.as_ref(), true);
    } else {
        apply_e2ee_response_headers(&mut response.headers, finalized.e2ee.as_ref(), false);
    }
    response.headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response.headers.insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-cache"),
    );
    let body = Body::from_stream(
        RequestCleanupStream {
            inner: finalized.body,
            request_store: ctx.request_store,
            request_id: ctx.request_id,
            done: false,
        }
        .map(|chunk| chunk.map_err(|e| std::io::Error::other(e.to_string()))),
    );
    (response.status, response.headers, body).into_response()
}

pub(super) fn apply_e2ee_response_headers(
    headers: &mut HeaderMap,
    e2ee: Option<&E2eeResponseInfo>,
    include_plain_false: bool,
) {
    match e2ee {
        Some(info) => {
            headers.insert(
                HeaderName::from_static("x-e2ee-applied"),
                HeaderValue::from_static("true"),
            );
            insert_str_header(headers, "x-e2ee-version", &info.version);
            insert_str_header(headers, "x-e2ee-algo", &info.algo);
        }
        None if include_plain_false => {
            headers.insert(
                HeaderName::from_static("x-e2ee-applied"),
                HeaderValue::from_static("false"),
            );
            headers.remove(HeaderName::from_static("x-e2ee-version"));
            headers.remove(HeaderName::from_static("x-e2ee-algo"));
        }
        None => {}
    }
}

pub(super) struct RequestCleanupStream {
    inner: ServiceResponseStream,
    request_store: GatewayRequestStore,
    request_id: String,
    done: bool,
}

impl Unpin for RequestCleanupStream {}

impl Stream for RequestCleanupStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                this.done = true;
                this.request_store.take(&this.request_id);
                Poll::Ready(None)
            }
            Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(Err(err))) => {
                this.done = true;
                this.request_store.take(&this.request_id);
                Poll::Ready(Some(Err(err)))
            }
        }
    }
}
