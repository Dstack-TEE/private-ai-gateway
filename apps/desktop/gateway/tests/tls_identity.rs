//! The local endpoint's TLS identity is what stops an impersonator on the
//! same port. These tests use a representative rustls client that trusts only
//! the installation CA and prove it fails before sending anything when the
//! listener is plain HTTP, presents a foreign certificate, or has no
//! certificate the client trusts; and that the real proxy handshake succeeds.

use std::{
    fs,
    net::TcpListener,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use desktop_gateway::{
    proxy::{self, ProxyState},
    secrets::MemoryStore,
    tls,
};
use tokio::sync::mpsc;

fn client(ca_pem: &str) -> reqwest::Client {
    reqwest::Client::builder()
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .add_root_certificate(reqwest::Certificate::from_pem(ca_pem.as_bytes()).unwrap())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// RAII temp dir: removed when dropped, so no `pag-tls-it-*` residue is left
/// behind even when an assertion fails.
fn temp_dir(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("pag-tls-it-{name}-"))
        .tempdir()
        .unwrap()
}

/// A listener that records whether any HTTP request line ever arrived, so a
/// failed handshake is distinguishable from a rejected request.
fn plain_http_sink(listener: TcpListener) -> Arc<AtomicUsize> {
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let counter = counter.clone();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut buf = [0u8; 8];
                if let Ok(n) = stream.read(&mut buf) {
                    // An HTTP request starts with an ASCII method; a TLS
                    // ClientHello starts with 0x16.
                    if n > 0 && buf[0] != 0x16 {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                }
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
            });
        }
    });
    seen
}

#[tokio::test]
async fn the_proxy_serves_only_its_own_identity_and_clients_refuse_impostors() {
    let dir = temp_dir("identity");
    let secrets = MemoryStore::default();
    let identity = tls::load_or_create(dir.path(), &secrets).unwrap();
    let trusted = client(&identity.ca_pem);

    // 1. The real proxy: handshake succeeds; the request reaches auth (401).
    let (events, _rx) = mpsc::channel(4);
    let state = ProxyState::new(events);
    let listener = proxy::bind_std("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(proxy::serve_tls(
        state,
        listener,
        identity.server_config.clone(),
    ));
    let response = trusted
        .get(format!("https://127.0.0.1:{}/v1/models", addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);

    // 2. An impostor with plain HTTP on the port: the client never sends a
    //    request (only a ClientHello that the sink saw as TLS bytes).
    let squatter = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = squatter.local_addr().unwrap().port();
    let seen = plain_http_sink(squatter);
    let error = trusted
        .post(format!("https://127.0.0.1:{port}/v1/messages"))
        .header("authorization", "Bearer agent-token")
        .body("{\"prompt\":\"secret\"}")
        .send()
        .await
        .unwrap_err();
    assert!(error.is_connect() || error.is_request(), "{error}");
    assert_eq!(
        seen.load(Ordering::SeqCst),
        0,
        "no HTTP request left the client"
    );

    // 3. An impostor with its own certificate: verification fails before the
    //    request, even though the name matches.
    let foreign_dir = temp_dir("foreign");
    let foreign = tls::load_or_create(foreign_dir.path(), &MemoryStore::default()).unwrap();
    let (events, _rx) = mpsc::channel(4);
    let listener = proxy::bind_std("127.0.0.1:0".parse().unwrap()).unwrap();
    let foreign_addr = listener.local_addr().unwrap();
    let foreign_state = ProxyState::new(events);
    tokio::spawn(proxy::serve_tls(
        foreign_state,
        listener,
        foreign.server_config.clone(),
    ));
    let error = trusted
        .post(format!(
            "https://127.0.0.1:{}/v1/messages",
            foreign_addr.port()
        ))
        .header("authorization", "Bearer agent-token")
        .body("{}")
        .send()
        .await
        .unwrap_err();
    assert!(error.is_connect() || error.is_request(), "{error}");
    let text = format!("{error:?}");
    assert!(
        text.contains("certificate") || text.contains("Certificate") || text.contains("verify"),
        "{text}"
    );

    // 4. A client without the CA refuses the real proxy too: trust is explicit.
    let untrusting = reqwest::Client::builder()
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    assert!(untrusting
        .get(format!("https://127.0.0.1:{}/health", addr.port()))
        .send()
        .await
        .is_err());
}

/// Real-client proof: the installed Claude Code CLI must refuse an impostor
/// and accept the installation CA via the settings.json `env` block, the
/// same projection the app writes. Runs only where `claude` and coreutils
/// `timeout` are installed (Claude Code retries connection failures with
/// backoff, so each run is capped; the assertions are about what the
/// listeners observed, not about the CLI's exit). The proxy must keep
/// serving while the CLI runs, hence the multi-threaded runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the claude CLI"]
async fn claude_code_verifies_the_local_identity() {
    use std::process::Stdio;
    let dir = temp_dir("claude");
    let secrets = MemoryStore::default();
    let identity = tls::load_or_create(dir.path(), &secrets).unwrap();

    let run = |port: u16, trust: bool| {
        let config_dir = dir.path().join(format!("claude-config-{port}-{trust}"));
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join(".claude.json"),
            "{\"hasCompletedOnboarding\":true}",
        )
        .unwrap();
        let mut env = serde_json::json!({
            "ANTHROPIC_BASE_URL": format!("https://127.0.0.1:{port}"),
            "ANTHROPIC_AUTH_TOKEN": "agent-token",
            "ANTHROPIC_MODEL": "openai/gpt-oss-20b",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            "DISABLE_TELEMETRY": "1",
        });
        if trust {
            env["NODE_EXTRA_CA_CERTS"] = serde_json::json!(identity.ca_path.display().to_string());
        }
        fs::write(
            config_dir.join("settings.json"),
            serde_json::json!({ "env": env }).to_string(),
        )
        .unwrap();
        std::process::Command::new("timeout")
            .args(["25", "claude", "-p", "reply with ok", "--max-turns", "1"])
            .env("CLAUDE_CONFIG_DIR", &config_dir)
            .env_remove("NODE_EXTRA_CA_CERTS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("timeout + claude CLI")
    };

    // 1. Impostor on plain HTTP with an https URL: no HTTP request is sent.
    let squatter = TcpListener::bind("127.0.0.1:0").unwrap();
    let squat_port = squatter.local_addr().unwrap().port();
    let seen = plain_http_sink(squatter);
    let output = run(squat_port, true);
    assert_eq!(
        seen.load(Ordering::SeqCst),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // 2. Real identity but no trust configured: no authenticated attempt
    //    reaches the proxy (the handshake fails first).
    let (events, mut untrusted_rx) = mpsc::channel(8);
    let state = ProxyState::new(events);
    let listener = proxy::bind_std("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(proxy::serve_tls(
        state,
        listener,
        identity.server_config.clone(),
    ));
    let _ = run(port, false);
    assert!(
        untrusted_rx.try_recv().is_err(),
        "no request reached auth without trust"
    );

    // 3. Trust via the settings.json env block: the request reaches the
    //    proxy, which answers 401 for the unknown token (auth happened after
    //    a successful handshake against the installation identity).
    let (events, mut rx) = mpsc::channel(8);
    let state = ProxyState::new(events);
    let listener = proxy::bind_std("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(proxy::serve_tls(
        state,
        listener,
        identity.server_config.clone(),
    ));
    let _ = run(port, true);
    let event = rx
        .try_recv()
        .expect("an authenticated attempt reached the proxy");
    assert_eq!(event.status, 401);
    assert_eq!(event.path, "/v1/messages");
}
