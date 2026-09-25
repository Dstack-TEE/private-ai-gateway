//! Serves an axum router the way `axum::serve` does, with the limits it leaves
//! out ("intentionally simple and doesn't support any configuration"): hyper's
//! HTTP/1 header read timeout, which needs a timer that `axum::serve` does not
//! set, and a cap on open connections. The loop follows axum's
//! `serve-with-hyper` example and hyper-util's `server_graceful` example.

use std::{future::Future, pin::pin, sync::Arc, time::Duration};

use axum::{extract::ConnectInfo, serve::Listener, Router};
use hyper::{body::Incoming, server::conn::http1, Request};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    server::graceful::GracefulShutdown,
};
use tokio::sync::Semaphore;
use tower_service::Service;

/// A client must send a request's headers within this time, hyper's default.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Serves `router` on `listener` until `signal` completes, holding at most
/// `max_connections` connections open; the next connection is accepted once
/// one closes. Every request carries [`ConnectInfo`] with the peer address.
/// Once `signal` completes the listener closes, idle connections close, and
/// this returns when the open requests have been answered.
pub async fn serve<L>(
    mut listener: L,
    router: Router,
    max_connections: usize,
    signal: impl Future<Output = ()>,
) where
    L: Listener,
    L::Addr: Clone + Sync,
{
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    let connections = Arc::new(Semaphore::new(max_connections));
    let graceful = GracefulShutdown::new();
    let mut signal = pin!(signal);
    loop {
        let accepted = async {
            let permit = connections.clone().acquire_owned().await;
            (permit, listener.accept().await)
        };
        let (permit, (io, address)) = tokio::select! {
            accepted = accepted => accepted,
            () = &mut signal => break,
        };
        let router = router.clone();
        let service = hyper::service::service_fn(move |mut request: Request<Incoming>| {
            request
                .extensions_mut()
                .insert(ConnectInfo(address.clone()));
            router.clone().call(request)
        });
        let connection = graceful.watch(builder.serve_connection(TokioIo::new(io), service));
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::trace!("Connection closed: {error}");
            }
            drop(permit);
        });
    }
    drop(listener);
    graceful.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn a_client_that_never_finishes_its_headers_is_disconnected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, Router::new(), 1, std::future::pending()));
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let started = tokio::time::Instant::now();
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).await.unwrap();
        assert!(started.elapsed() >= HEADER_READ_TIMEOUT);
    }
}
