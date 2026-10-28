//! Per-endpoint inference admission retained by response bodies and write buffers.
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};

use axum::{
    body::{Body, Bytes, HttpBody},
    response::Response,
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

use super::request_transform::Endpoint;

/// Bound each endpoint independently, including buffered response bodies.
pub const MAX_IN_FLIGHT: usize = 16;
static IMAGES: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(MAX_IN_FLIGHT)));

static AUDIO: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(MAX_IN_FLIGHT)));

pub fn try_acquire(endpoint: Endpoint) -> Result<Option<OwnedSemaphorePermit>, TryAcquireError> {
    match endpoint {
        Endpoint::ImagesGenerations => IMAGES.clone().try_acquire_owned().map(Some),
        Endpoint::AudioTranscriptions => AUDIO.clone().try_acquire_owned().map(Some),
        _ => Ok(None),
    }
}

pub fn capacity_body(request_id: Option<&str>) -> Vec<u8> {
    let mut body = serde_json::json!({"error":{"type":"rate_limit_error", "code":"rate_limit_exceeded",
        "param":null, "message":"inference capacity reached"}});
    if let Some(request_id) = request_id {
        body["request_id"] = serde_json::json!(request_id);
    }
    serde_json::to_vec(&body).unwrap_or_default()
}

pub fn capacity_response() -> Response {
    let mut response = super::errors::parts_response(429, capacity_body(None));
    response
        .headers_mut()
        .insert("retry-after", axum::http::HeaderValue::from_static("1"));
    response
}

/// The response body owns admission after the handler returns to Hyper.
/// Completion, body errors, and disconnect/drop all release the slot.
pub fn hold_permit(response: Response, permit: Option<OwnedSemaphorePermit>) -> Response {
    match permit {
        Some(permit) => response.map(|body| {
            Body::new(PermitBody {
                body,
                permit: Some(Arc::new(permit)),
            })
        }),
        None => response,
    }
}

struct PermitBody {
    body: Body,
    permit: Option<Arc<OwnedSemaphorePermit>>,
}

impl HttpBody for PermitBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.body).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                Poll::Ready(Some(Ok(frame.map_data(|bytes| {
                    match self.permit.as_ref() {
                        Some(permit) => Bytes::from_owner(PermitBytes {
                            bytes,
                            _permit: permit.clone(),
                        }),
                        None => bytes,
                    }
                }))))
            }
            frame @ Poll::Ready(None | Some(Err(_))) => {
                self.permit.take();
                frame
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

// Hyper may drop Body as soon as it takes the last frame, while retaining that
// frame in its write buffer. Keep admission with the bytes until the writer
// releases them too. Bytes::from_owner preserves the allocation without copying.
struct PermitBytes {
    bytes: Bytes,
    _permit: Arc<OwnedSemaphorePermit>,
}

impl AsRef<[u8]> for PermitBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn response_body_owns_capacity_through_consumption_and_disconnect() {
        use axum::body::to_bytes;

        let capacity = Arc::new(Semaphore::new(1));
        let response = hold_permit(
            Response::new(Body::from("image")),
            Some(capacity.clone().try_acquire_owned().unwrap()),
        );
        assert!(capacity.clone().try_acquire_owned().is_err());
        assert_eq!(response.body().size_hint().exact(), Some(5));
        let bytes = to_bytes(response.into_body(), 100).await.unwrap();
        assert_eq!(bytes, "image");
        // The buffered bytes may still belong to Hyper's pending write.
        assert_eq!(capacity.available_permits(), 0);
        drop(bytes);
        assert_eq!(capacity.available_permits(), 1);

        let response = hold_permit(
            Response::new(Body::from("image")),
            Some(capacity.clone().try_acquire_owned().unwrap()),
        );
        assert_eq!(capacity.available_permits(), 0);
        drop(response); // Client disconnected before polling the body.
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn queued_frame_owns_capacity_even_after_hyper_drops_the_body() {
        use http_body_util::BodyExt;

        let capacity = Arc::new(Semaphore::new(1));
        let response = hold_permit(
            Response::new(Body::from("image")),
            Some(capacity.clone().try_acquire_owned().unwrap()),
        );
        let mut body = response.into_body();
        let bytes = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert!(body.is_end_stream());
        drop(body);
        assert_eq!(capacity.available_permits(), 0);
        let pending_write = bytes.clone();
        drop(bytes);
        assert_eq!(capacity.available_permits(), 0);
        drop(pending_write);
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn response_body_errors_release_capacity() {
        use http_body_util::BodyExt;

        let capacity = Arc::new(Semaphore::new(1));
        let failing = futures_util::stream::once(async {
            Err::<Bytes, _>(std::io::Error::other("body failed"))
        });
        let response = hold_permit(
            Response::new(Body::from_stream(failing)),
            Some(capacity.clone().try_acquire_owned().unwrap()),
        );
        let mut body = response.into_body();
        assert_eq!(capacity.available_permits(), 0);
        assert!(body.frame().await.unwrap().is_err());
        assert_eq!(capacity.available_permits(), 1);
    }
    #[test]
    fn image_capacity_is_bounded_and_never_queues() {
        let permits: Vec<_> = (0..MAX_IN_FLIGHT)
            .map(|_| try_acquire(Endpoint::ImagesGenerations).unwrap())
            .collect();
        let audio = try_acquire(Endpoint::AudioTranscriptions).unwrap();
        drop(audio);
        assert!(matches!(
            try_acquire(Endpoint::ImagesGenerations),
            Err(TryAcquireError::NoPermits)
        ));
        drop(permits);
        assert!(try_acquire(Endpoint::ImagesGenerations).is_ok());
        assert!(try_acquire(Endpoint::ChatComplete).unwrap().is_none());
    }
}
