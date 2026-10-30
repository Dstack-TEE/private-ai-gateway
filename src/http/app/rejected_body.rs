//! Bounded lingering reads for requests rejected from headers alone.

use std::time::Duration;

use axum::{
    body::Body,
    http::{header, HeaderMap},
    response::Response,
};
use futures_util::StreamExt;
use tokio::sync::Semaphore;

const IDLE_TIMEOUT: Duration = Duration::from_secs(1);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(5);
static DISCARD_CONNECTIONS: Semaphore = Semaphore::const_new(32);

/// Hyper 1.9's HTTP/1 dispatcher only tries one read after an unread body is
/// dropped, then closes. Unread TCP data can reset the connection and hide our
/// JSON refusal from a streaming reverse proxy. Like nginx's lingering close,
/// discard without buffering, under independent connection, byte and time caps.
/// The rejection is already decided; no parser, upload budget or inference
/// admission is involved. Saturation retains the immediate-close behavior.
pub(super) async fn discard_before_reply(
    headers: &HeaderMap,
    body: Body,
    limit: usize,
    mut response: Response,
) -> Response {
    // Hyper emits 100 Continue when the body is polled. Refuse without inviting
    // an upload; its dispatcher skips the interim response when dropping Body.
    if headers
        .get_all(header::EXPECT)
        .iter()
        .any(|value| value.as_bytes().eq_ignore_ascii_case(b"100-continue"))
    {
        response.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("close"),
        );
        return response;
    }
    discard_with_budget(body, limit, response, &DISCARD_CONNECTIONS).await
}

async fn discard_with_budget(
    body: Body,
    limit: usize,
    mut response: Response,
    budget: &Semaphore,
) -> Response {
    let complete = if let Ok(_permit) = budget.try_acquire() {
        tokio::time::timeout(TOTAL_TIMEOUT, async move {
            let mut chunks = body.into_data_stream();
            let mut remaining = limit;
            while remaining > 0 {
                match tokio::time::timeout(IDLE_TIMEOUT, chunks.next()).await {
                    Ok(None) => return true,
                    Ok(Some(Ok(chunk))) if chunk.len() <= remaining => {
                        remaining -= chunk.len();
                    }
                    _ => return false,
                }
            }
            false
        })
        .await
        .unwrap_or(false)
    } else {
        false
    };
    if !complete {
        response.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("close"),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse};
    use bytes::Bytes;
    use futures_util::stream;

    fn refusal() -> Response {
        (StatusCode::BAD_REQUEST, "refused").into_response()
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_discard_stops_at_idle_deadline_and_releases_its_slot() {
        let budget = Semaphore::new(1);
        let started = tokio::time::Instant::now();
        let body = Body::from_stream(stream::pending::<Result<Bytes, std::io::Error>>());
        let response = discard_with_budget(body, 1024, refusal(), &budget).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[header::CONNECTION], "close");
        assert_eq!(started.elapsed(), IDLE_TIMEOUT);
        assert_eq!(budget.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn trickled_discard_cannot_extend_the_total_deadline() {
        let budget = Semaphore::new(1);
        let started = tokio::time::Instant::now();
        let body = Body::from_stream(stream::unfold((), |_| async {
            tokio::time::sleep(Duration::from_millis(900)).await;
            Some((Ok::<_, std::io::Error>(Bytes::from_static(b"x")), ()))
        }));
        let response = discard_with_budget(body, 1024, refusal(), &budget).await;
        assert_eq!(response.headers()[header::CONNECTION], "close");
        assert_eq!(started.elapsed(), TOTAL_TIMEOUT);
        assert_eq!(budget.available_permits(), 1);
    }

    #[tokio::test]
    async fn discard_capacity_refuses_immediately_without_polling_the_body() {
        let budget = Semaphore::new(32);
        let _occupied = budget.acquire_many(32).await.unwrap();
        let body = Body::from_stream(stream::poll_fn(
            |_| -> std::task::Poll<Option<Result<Bytes, std::io::Error>>> {
                panic!("a saturated discard budget must not read another body");
            },
        ));
        let response = discard_with_budget(body, 1024, refusal(), &budget).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[header::CONNECTION], "close");
    }

    #[tokio::test]
    async fn discard_stops_at_the_byte_cap_without_waiting_for_eof() {
        let body = Body::from_stream(
            stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"1234")) })
                .chain(stream::pending()),
        );
        let response = discard_with_budget(body, 4, refusal(), &Semaphore::new(1)).await;
        assert_eq!(response.headers()[header::CONNECTION], "close");
    }
}
