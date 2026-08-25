//! Downstream write-idle timeout for served connections.
//!
//! A streaming forward holds the upstream connection open while it pumps the
//! response to the downstream. If the downstream stops reading (its TCP/UDS
//! receive side is not drained — e.g. a client or middleware that stalls without
//! closing), the serving connection's write blocks, the response body stops
//! being polled, and the upstream connection is held indefinitely (eventually
//! `CLOSE_WAIT`, leaking sockets and kernel memory). Hyper only learns of a
//! *closed* downstream; a stalled-open one has no signal.
//!
//! [`WriteIdleTimeout`] wraps the downstream IO and fails a write that stays
//! pending past `idle`. The error tears the connection down, which drops the
//! response body and with it the upstream stream — closing the upstream
//! connection. The timeout lives in the IO layer (which hyper always polls),
//! not in the body's poll chain (which a stalled downstream never polls).
//!
//! It is a no-progress timeout, not a total one: any accepted write resets it,
//! so a long but flowing stream (a slow LLM that keeps emitting) is never cut —
//! only a downstream that accepts nothing for `idle` is.
//!
//! The flip side, and it is deliberate: *any* accepted write resets the deadline,
//! including a single byte. A downstream that advertises a tiny window and drains
//! a byte every few minutes keeps the connection alive indefinitely — the same
//! leak reached by trickle instead of by full stall. Bounding that needs a
//! byte-rate floor or an absolute cap, which is a separate policy decision;
//! this type only promises to reap downstreams that accept *nothing*.

use std::future::Future;
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{sleep, Sleep};

/// How long a downstream write may make no progress before the connection is
/// torn down. Generous on purpose: legitimate streams keep flowing well under
/// this, so it only reaps genuinely stalled downstreams. Matches the upstream
/// read timeout order of magnitude.
pub(super) const DOWNSTREAM_WRITE_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

pub(super) struct WriteIdleTimeout<IO> {
    inner: IO,
    idle: Duration,
    timer: Option<Pin<Box<Sleep>>>,
}

impl<IO> WriteIdleTimeout<IO> {
    pub(super) fn new(inner: IO, idle: Duration) -> Self {
        Self {
            inner,
            idle,
            timer: None,
        }
    }

    /// Called while the inner write is pending. Arms (once) and polls the idle
    /// timer; fires a `TimedOut` error if it elapses. Generic in the success
    /// type so both `poll_write` (`usize`) and the vectored variant can reuse it.
    fn poll_idle<T>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        let idle = self.idle;
        let timer = self.timer.get_or_insert_with(|| Box::pin(sleep(idle)));
        match timer.as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.timer = None;
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "downstream write idle timeout",
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for WriteIdleTimeout<IO> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for WriteIdleTimeout<IO> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(r) => {
                self.timer = None;
                Poll::Ready(r)
            }
            Poll::Pending => self.poll_idle(cx),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write_vectored(cx, bufs) {
            Poll::Ready(r) => {
                self.timer = None;
                Poll::Ready(r)
            }
            Poll::Pending => self.poll_idle(cx),
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    /// Flush is bounded too. With a bare `TcpStream` underneath this is a no-op,
    /// but the type is generic: insert any buffering IO (in-process TLS being the
    /// obvious one) and a stalled downstream would otherwise park here forever,
    /// silently defeating the timeout with nothing to catch it.
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(r) => {
                self.timer = None;
                Poll::Ready(r)
            }
            Poll::Pending => self.poll_idle(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use tokio::io::AsyncWriteExt;

    /// An IO whose writes never make progress — models a downstream that has
    /// stopped reading.
    struct StalledIo;
    impl AsyncRead for StalledIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
    impl AsyncWrite for StalledIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    /// An IO that stalls, then accepts exactly one write, then stalls for good
    /// — models a downstream that pauses reading, resumes briefly, and stops.
    /// The leading stall matters: it is what arms the timer that the accepted
    /// write must then reset.
    struct StallsAroundOneWrite {
        pending_before_accept: usize,
        accepted: bool,
    }
    impl AsyncRead for StallsAroundOneWrite {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
    impl AsyncWrite for StallsAroundOneWrite {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.accepted {
                Poll::Pending
            } else if self.pending_before_accept > 0 {
                self.pending_before_accept -= 1;
                Poll::Pending
            } else {
                self.accepted = true;
                Poll::Ready(Ok(buf.len()))
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_write_times_out_after_idle() {
        let mut io = WriteIdleTimeout::new(StalledIo, Duration::from_secs(600));
        let err = io.write(b"hello").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    /// The other half of the invariant: progress must *reset* an already-armed
    /// deadline. The timer is armed by a pending write, so the test must first
    /// stall to arm it, then let the write through, then stall again — without
    /// that leading stall, dropping `self.timer = None` from the `Poll::Ready`
    /// arms would go unnoticed while cutting every healthy stream at `idle`.
    #[tokio::test(start_paused = true)]
    async fn accepted_write_resets_the_deadline() {
        let idle = Duration::from_secs(600);
        let mut io = WriteIdleTimeout::new(
            StallsAroundOneWrite {
                pending_before_accept: 1,
                accepted: false,
            },
            idle,
        );

        // First poll goes pending and arms the timer at t=0. Wake it well into
        // the window; the stub then accepts, which must discard that timer.
        let start = tokio::time::Instant::now();
        let mut first = std::pin::pin!(io.write(b"first"));
        assert!(
            futures_util::poll!(first.as_mut()).is_pending(),
            "the stub must stall the first poll so the timer gets armed"
        );
        tokio::time::advance(Duration::from_secs(500)).await;
        let written = first.await.expect("accepted write must succeed");
        assert_eq!(written, 5, "the stub accepts the whole buffer");
        assert_eq!(start.elapsed(), Duration::from_secs(500));

        // A second write now stalls. Had the accepted write not reset the timer
        // armed at t=0 it would fire at t=600, ~100s from now; it must instead
        // take the full idle window from here.
        let resumed = tokio::time::Instant::now();
        let err = io.write(b"second").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            resumed.elapsed() >= idle,
            "deadline was not reset by the accepted write: fired after {:?}",
            resumed.elapsed()
        );
    }
}
