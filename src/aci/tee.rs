//! Byte-exact stream teeing: forward a response downstream unchanged while
//! capturing the wire bytes, so a receipt's hashes (§10.2) can be checked
//! against exactly what went over the wire.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};

/// Fired with the full wire bytes when a [`TeeStream`] ends cleanly.
pub type CompletionHook = Box<dyn FnOnce(Vec<u8>) + Send>;

/// Forwards each upstream chunk downstream byte-exact while teeing a copy for
/// hashing. On clean end-of-stream it fires `on_complete` with the full wire
/// bytes; a client that disconnects early leaves the hook unfired (there is no
/// complete response to verify).
pub struct TeeStream<E> {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, E>> + Send>>,
    wire: Vec<u8>,
    on_complete: Option<CompletionHook>,
}

impl<E> TeeStream<E> {
    pub fn new(
        inner: Pin<Box<dyn Stream<Item = Result<Bytes, E>> + Send>>,
        on_complete: CompletionHook,
    ) -> Self {
        Self {
            inner,
            wire: Vec::new(),
            on_complete: Some(on_complete),
        }
    }
}

impl<E: std::error::Error + Send + Sync + 'static> Stream for TeeStream<E> {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.wire.extend_from_slice(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(std::io::Error::other(e)))),
            Poll::Ready(None) => {
                if let Some(hook) = this.on_complete.take() {
                    hook(std::mem::take(&mut this.wire));
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn chunks(
        items: Vec<Result<Bytes, std::io::Error>>,
    ) -> (
        TeeStream<std::io::Error>,
        std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let tee = TeeStream::new(
            stream::iter(items).boxed(),
            Box::new(move |wire| {
                let _ = tx.send(wire);
            }),
        );
        (tee, rx)
    }

    #[tokio::test]
    async fn forwards_byte_exact_and_fires_hook_on_clean_end() {
        let (tee, rx) = chunks(vec![
            Ok(Bytes::from_static(b"he")),
            Ok(Bytes::from_static(b"llo")),
        ]);
        let out: Vec<Bytes> = tee.map(|item| item.unwrap()).collect().await;
        assert_eq!(out.concat(), b"hello");
        assert_eq!(rx.try_recv().unwrap(), b"hello");
    }

    #[tokio::test]
    async fn hook_stays_unfired_when_the_stream_errors() {
        let (mut tee, rx) = chunks(vec![
            Ok(Bytes::from_static(b"he")),
            Err(std::io::Error::other("upstream died")),
        ]);
        assert_eq!(
            tee.next().await.unwrap().unwrap(),
            Bytes::from_static(b"he")
        );
        assert!(tee.next().await.unwrap().is_err());
        assert!(rx.try_recv().is_err(), "hook must not fire on error");
    }
}
