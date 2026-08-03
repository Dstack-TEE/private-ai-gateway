//! Byte-exact stream teeing for after-the-fact receipt checks.
//!
//! Forwards a response downstream unchanged while incrementally digesting
//! the wire bytes, so a receipt's hashes (spec 9.3) can later be checked
//! against exactly what went over the wire — without ever buffering or
//! storing bodies.

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};

use crate::checks::BodyDigest;

/// How a teed stream ended.
pub enum StreamEnd {
    /// Clean end-of-stream: digest over the full wire bytes.
    Complete(BodyDigest),
    /// The upstream errored mid-stream: digest over the bytes forwarded so
    /// far plus the error. §6.5/§9.3(4) make truncation exactly the case a
    /// wire-hash verifier must surface, so it must not end unreported.
    Errored { partial: BodyDigest, error: String },
}

/// Fired once with how the stream ended.
pub type CompletionHook = Box<dyn FnOnce(StreamEnd) + Send>;

/// Forwards each upstream chunk downstream byte-exact while digesting a copy.
/// The hook fires on clean end-of-stream and on an upstream error; a client
/// that disconnects early drops the stream and leaves it unfired (nothing was
/// truncated — the client walked away).
pub fn tee<E: std::error::Error + Send + Sync + 'static>(
    upstream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    on_complete: CompletionHook,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut upstream = std::pin::pin!(upstream);
        let mut hasher = Sha256::new();
        let mut len = 0u64;
        while let Some(item) = upstream.next().await {
            match item {
                Ok(chunk) => {
                    hasher.update(&chunk);
                    len += chunk.len() as u64;
                    yield Ok(chunk);
                }
                Err(error) => {
                    on_complete(StreamEnd::Errored {
                        partial: BodyDigest::from_sha256(hasher.finalize().into(), len),
                        error: error.to_string(),
                    });
                    yield Err(std::io::Error::other(error));
                    return;
                }
            }
        }
        on_complete(StreamEnd::Complete(BodyDigest::from_sha256(
            hasher.finalize().into(),
            len,
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use std::pin::Pin;

    type Teed = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

    fn chunks(
        items: Vec<Result<Bytes, std::io::Error>>,
    ) -> (Teed, std::sync::mpsc::Receiver<StreamEnd>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let teed = Box::pin(tee(
            stream::iter(items),
            Box::new(move |end| {
                let _ = tx.send(end);
            }),
        ));
        (teed, rx)
    }

    #[tokio::test]
    async fn forwards_byte_exact_and_fires_hook_on_clean_end() {
        let (teed, rx) = chunks(vec![
            Ok(Bytes::from_static(b"he")),
            Ok(Bytes::from_static(b"llo")),
        ]);
        let out: Vec<Bytes> = teed.map(|item| item.unwrap()).collect().await;
        assert_eq!(out.concat(), b"hello");
        match rx.try_recv().unwrap() {
            StreamEnd::Complete(digest) => assert_eq!(digest, BodyDigest::of(b"hello")),
            StreamEnd::Errored { .. } => panic!("clean end reported as errored"),
        }
    }

    #[tokio::test]
    async fn an_upstream_error_reports_the_truncation() {
        let (mut teed, rx) = chunks(vec![
            Ok(Bytes::from_static(b"he")),
            Err(std::io::Error::other("upstream died")),
        ]);
        assert_eq!(
            teed.next().await.unwrap().unwrap(),
            Bytes::from_static(b"he")
        );
        assert!(teed.next().await.unwrap().is_err());
        match rx.try_recv().unwrap() {
            StreamEnd::Errored { partial, error } => {
                assert_eq!(partial, BodyDigest::of(b"he"));
                assert!(error.contains("upstream died"));
            }
            StreamEnd::Complete(_) => panic!("truncation reported as complete"),
        }
    }

    #[tokio::test]
    async fn a_dropped_stream_leaves_the_hook_unfired() {
        let (mut teed, rx) = chunks(vec![Ok(Bytes::from_static(b"he"))]);
        let _ = teed.next().await;
        drop(teed);
        assert!(rx.try_recv().is_err(), "a client disconnect is not an end");
    }
}
