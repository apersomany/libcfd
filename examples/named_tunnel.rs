//! Runs a named tunnel from a cloudflared credentials file or a dashboard
//! connector token, and verifies it end-to-end.
//!
//! Run with:
//! ```text
//! cargo run --example named_tunnel -- /path/to/credentials.json
//! cargo run --example named_tunnel -- <connector-token>
//! ```
//!
//! The credentials file is the JSON cloudflared writes on
//! `cloudflared tunnel login` / `cloudflared tunnel create`, with keys
//! `AccountTag`, `TunnelID` and `TunnelSecret` (standard base64). The
//! connector token is what the Zero Trust dashboard shows for
//! `cloudflared tunnel run --token`.
//!
//! For remotely-managed tunnels the edge pushes the tunnel's configuration
//! after registration, so libcfd learns the public hostnames routed to the
//! tunnel via RPC. The example prints them and polls each hostname until
//! the origin answers, then keeps serving until Ctrl-C.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use libcfd::{
    Body, EdgeConnector, EdgeOptions, HttpOrigin, NamedTunnel, RemoteConfiguration, Response,
    Tunnel,
};

/// How long to keep polling a public hostname for the origin response.
const POLL_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct HelloOrigin {
    served: Arc<AtomicUsize>,
}

impl HttpOrigin for HelloOrigin {
    async fn handle(&self, _request: libcfd::Request) -> Result<libcfd::Response, libcfd::Error> {
        self.served.fetch_add(1, Ordering::SeqCst);
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/plain"),
        );
        Ok(Response::new(
            http::StatusCode::OK,
            headers,
            Body::from_bytes("hello from a named tunnel!".as_bytes().to_vec()),
        ))
    }
}

/// Minimal HTTPS GET for the live check. TLS is verified against the public
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
    let configuration = rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(configuration));

    let address = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| format!("resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("no address for {host}"))?;
    let tcp = TcpStream::connect(address)
        .await
        .map_err(|e| format!("tcp connect {address}: {e}"))?;
    let mut stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls handshake with {host}: {e}"))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: libcfd-named-tunnel-example\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write request: {e}"))?;
    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .await
        .map_err(|e| format!("read response: {e}"))?;

    let header_end = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "response has no header terminator".to_string())?;
    let head = std::str::from_utf8(&buffer[..header_end])
        .map_err(|e| format!("response head is not utf-8: {e}"))?;
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("malformed status line {head:?}"))?;
    let body = buffer[header_end + 4..].to_vec();
    Ok((status, body))
}

/// Verifies each public hostname routed to the tunnel by polling it until
/// the origin responds, then reports the outcome.
async fn verify_hostnames(hostnames: Vec<String>) {
    for hostname in hostnames {
        let url = format!("https://{hostname}/");
        let deadline = Instant::now() + POLL_TIMEOUT;
        let mut last_status = 0u16;
        let mut last_body = String::new();
        let mut reached_origin = false;
        while Instant::now() < deadline {
            match https_get(&url).await {
                Ok((status, body)) => {
                    last_status = status;
                    last_body = String::from_utf8_lossy(&body).into_owned();
                    if status == 200 && last_body.contains("hello from a named tunnel!") {
                        reached_origin = true;
                        break;
                    }
                }
                Err(e) => {
                    last_status = 0;
                    last_body = format!("https get error: {e}");
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        if reached_origin {
            println!("VERIFIED https://{hostname}/ -> origin response");
        } else {
            println!(
                "NOT VERIFIED https://{hostname}/ within {POLL_TIMEOUT:?} \
                 (last status={last_status}, body={last_body:?})"
            );
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), libcfd::Error> {
    tracing_subscriber::fmt::init();

    let arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "credentials.json".into());
    let tunnel = if std::path::Path::new(&arg).is_file() {
        NamedTunnel::from_credentials_file(&arg)?
    } else {
        NamedTunnel::from_token(&arg)?
    };
    println!(
        "loaded named tunnel {} (account {})",
        tunnel.tunnel_identifier, tunnel.account_tag
    );

    let hostnames: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let verifier_hostnames = hostnames.clone();
    let on_remote_configuration = move |configuration: RemoteConfiguration| {
        let mut guard = verifier_hostnames.lock().expect("hostnames lock");
        guard.extend(configuration.hostnames.iter().cloned());
        println!(
            "edge pushed remote configuration (version {}): hostnames {:?}",
            configuration.version, configuration.hostnames
        );
    };
    let options = libcfd::EdgeOptions {
        on_remote_configuration: Some(Arc::new(on_remote_configuration)),
        ..EdgeOptions::default()
    };

    let origin = HelloOrigin {
        served: Arc::new(AtomicUsize::new(0)),
    };
    let connector = EdgeConnector::new(options);
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        println!("shutting down");
    };

    let verify = tokio::spawn(async move {
        // Wait for the first config push from the edge; it also proves the registration was accepted.
        loop {
            let current = hostnames.lock().expect("hostnames lock").clone();
            if !current.is_empty() {
                verify_hostnames(current).await;
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });

    connector
        .run(
            Tunnel::named(tunnel),
            libcfd::Origin::http(origin.clone()),
            shutdown,
        )
        .await?;
    let _ = verify.await;
    println!(
        "tunnel run ended; origin served {} request(s)",
        origin.served.load(Ordering::SeqCst)
    );
    Ok(())
}
