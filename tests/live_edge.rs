//! Live-edge integration tests for the QUIC quick tunnel path.
//!
//! These tests talk to the real Cloudflare edge and the trycloudflare.com
//! API, so they are ignored by default and only run on demand:
//!
//! ```text
//! scripts/live-test.sh
//! ```
//!
//! Credentials are saved to `.test-creds/quick-tunnel.json` (gitignored) and
//! reused across runs instead of creating a tunnel every time, which keeps
//! the number of API requests low. The secret is never printed or logged.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use libcfd::{
    Body, QuickTunnel, QuickTunnelOptions, Request, Response, RunOptions, create_quick_tunnel,
    run_quick_tunnel,
};

const CREDS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.test-creds");
const CREDS_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.test-creds/quick-tunnel.json");
/// How long to keep polling the public hostname for the origin response.
const POLL_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

fn creds_path() -> std::path::PathBuf {
    Path::new(CREDS_FILE).to_path_buf()
}

fn load_creds() -> Option<QuickTunnel> {
    let bytes = std::fs::read(creds_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_creds(tunnel: &QuickTunnel) {
    std::fs::create_dir_all(Path::new(CREDS_DIR)).expect("create credentials dir");
    let json = serde_json::to_vec_pretty(tunnel).expect("serialize quick tunnel credentials");
    std::fs::write(creds_path(), json).expect("write credentials file");
}

async fn load_or_create_creds() -> QuickTunnel {
    if let Some(tunnel) = load_creds() {
        tracing::info!("reusing saved quick tunnel credentials");
        return tunnel;
    }
    tracing::info!("no saved credentials, creating a quick tunnel");
    let options = QuickTunnelOptions::default();
    let tunnel = create_quick_tunnel(&options)
        .await
        .expect("quick tunnel API should create a tunnel");
    save_creds(&tunnel);
    tracing::info!("created and saved quick tunnel credentials");
    tunnel
}

/// Minimal HTTPS GET for the live test. TLS is verified against the public
/// web PKI, since the tunnel hostname serves a public certificate.
async fn https_get(url: &str) -> Result<(u16, Vec<u8>), String> {
    use rustls_pki_types::ServerName;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let uri: http::Uri = url
        .parse()
        .map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let host = uri
        .host()
        .ok_or_else(|| format!("url {url:?} has no host"))?
        .to_string();
    let port = uri.port_u16().unwrap_or(443);
    let path = if uri.path().is_empty() {
        "/"
    } else {
        uri.path()
    };
    let server_name =
        ServerName::try_from(host.clone()).map_err(|e| format!("invalid host {host:?}: {e}"))?;

    let mut store = rustls::RootCertStore::empty();
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let addr = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| format!("resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("no address for {host}"))?;
    let tcp = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("tcp connect {addr}: {e}"))?;
    let mut stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls handshake with {host}: {e}"))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: libcfd-live-test\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write request: {e}"))?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .await
        .map_err(|e| format!("read response: {e}"))?;

    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "response has no header terminator".to_string())?;
    let head = std::str::from_utf8(&buf[..header_end])
        .map_err(|e| format!("response head is not utf-8: {e}"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "empty status line".to_string())?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed status line {status_line:?}"))?
        .parse()
        .map_err(|e| format!("malformed status code: {e}"))?;
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse::<usize>().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
    }
    let raw = &buf[header_end + 4..];
    let body = if chunked {
        decode_chunked(raw)?
    } else {
        match content_length {
            Some(len) if len <= raw.len() => raw[..len].to_vec(),
            _ => raw.to_vec(),
        }
    };
    Ok((status, body))
}

/// Decodes an HTTP/1.1 chunked body. The terminating `0\r\n\r\n` may or may
/// not still be in `data` when the size line is parsed.
fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "chunk size line unterminated".to_string())?;
        let size_str = std::str::from_utf8(&data[..line_end])
            .map_err(|e| format!("chunk size not utf-8: {e}"))?;
        let size_str = size_str.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|e| format!("bad chunk size {size_str:?}: {e}"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            break;
        }
        if data.len() < size + 2 {
            return Err("chunk body truncated".to_string());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
    Ok(out)
}

fn init_logging() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(tracing::Level::INFO);
    let _ = tracing_subscriber::fmt().with_max_level(level).try_init();
}

/// Creates a quick tunnel through the real API and saves its credentials for
/// reuse. Run explicitly when no credentials file exists yet.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn create_and_save_quick_tunnel() {
    init_logging();

    let options = QuickTunnelOptions::default();
    let tunnel = create_quick_tunnel(&options)
        .await
        .expect("quick tunnel API should create a tunnel");
    assert!(!tunnel.tunnel_id.is_empty(), "tunnel id present");
    assert!(!tunnel.hostname.is_empty(), "hostname present");
    assert!(!tunnel.secret.is_empty(), "secret present");
    assert!(!tunnel.account_tag.is_empty(), "account tag present");
    save_creds(&tunnel);
    tracing::info!(hostname = %tunnel.hostname, "saved quick tunnel credentials");
}

/// Runs a real quick tunnel over QUIC against the live Cloudflare edge and
/// verifies end-to-end HTTP serving through the public hostname.
///
/// Uses saved credentials when available, creating and saving them on first
/// run. Shuts the tunnel down cleanly and asserts the run completes.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_quick_tunnel_over_quic_serves_http() {
    init_logging();

    let tunnel = load_or_create_creds().await;
    let hostname = tunnel.hostname.clone();
    tracing::info!(%hostname, "using quick tunnel");

    // The public hostname must resolve to the Cloudflare edge.
    let addrs: Vec<_> = tokio::net::lookup_host((hostname.as_str(), 443))
        .await
        .unwrap_or_else(|e| panic!("{hostname} should resolve: {e}"))
        .collect();
    assert!(!addrs.is_empty(), "{hostname} resolved to no addresses");
    tracing::debug!(?addrs, "hostname resolved");

    let served = Arc::new(AtomicUsize::new(0));
    let origin_served = served.clone();
    let origin = move |request: Request| {
        let served = origin_served.clone();
        async move {
            served.fetch_add(1, Ordering::SeqCst);
            let body = format!("live-ok{}", request.uri.path());
            Ok(Response::new(
                http::StatusCode::OK,
                http::HeaderMap::new(),
                Body::from_bytes(body.into_bytes()),
            ))
        }
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async move {
        let _ = shutdown_rx.await;
    };
    let options: &'static RunOptions = Box::leak(Box::new(RunOptions::default()));
    let run_task = tokio::spawn(run_quick_tunnel(tunnel, origin, shutdown, options));

    let url = format!("https://{hostname}/hello");
    let deadline = Instant::now() + POLL_TIMEOUT;
    let mut last_status = 0u16;
    let mut last_body = String::new();
    let mut reached_origin = false;
    while Instant::now() < deadline {
        if run_task.is_finished() {
            let result = run_task.await.expect("tunnel run task join");
            panic!("tunnel run ended before serving a request: {result:?}");
        }
        match https_get(&url).await {
            Ok((status, body)) => {
                last_status = status;
                last_body = String::from_utf8_lossy(&body).into_owned();
                if status == 200 && last_body.contains("live-ok") {
                    reached_origin = true;
                    break;
                }
            }
            Err(e) => {
                last_status = 0;
                last_body = format!("https get error: {e}");
            }
        }
        tracing::debug!(status = last_status, body = %last_body, "polling tunnel hostname");
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    assert!(
        reached_origin,
        "tunnel never served the origin response within {POLL_TIMEOUT:?} \
         (last status={last_status}, body={last_body:?})"
    );
    assert_eq!(last_body, "live-ok/hello");
    assert!(
        served.load(Ordering::SeqCst) >= 1,
        "the origin handler was never invoked"
    );
    tracing::info!(status = last_status, body = %last_body, "tunnel served the origin response");

    let _ = shutdown_tx.send(());
    match tokio::time::timeout(Duration::from_secs(60), run_task).await {
        Ok(Ok(result)) => result.expect("tunnel should shut down cleanly"),
        Ok(Err(e)) => panic!("tunnel run task panicked: {e}"),
        Err(_) => panic!("tunnel did not shut down within 60s"),
    }
    tracing::info!("tunnel shut down cleanly");
}
