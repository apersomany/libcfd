//! Runs a named tunnel from a cloudflared credentials file.
//!
//! Run with: cargo run --example named_tunnel -- /path/to/credentials.json
//!
//! The credentials file is the JSON cloudflared writes on
//! `cloudflared tunnel login` / `cloudflared tunnel create`, with keys
//! `AccountTag`, `TunnelID` and `TunnelSecret` (standard base64). The
//! transport auto-selects QUIC and falls back to HTTP/2.

use libcfd::{Body, EdgeConnector, EdgeOptions, HttpOrigin, NamedTunnel, Response, Tunnel};

#[derive(Clone)]
struct HelloOrigin;

impl HttpOrigin for HelloOrigin {
    async fn handle(&self, request: libcfd::Request) -> Result<libcfd::Response, libcfd::Error> {
        let body = format!(
            "hello from a named tunnel!\nmethod={}\nuri={}\n",
            request.method, request.uri
        );
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/plain"),
        );
        Ok(Response::new(
            http::StatusCode::OK,
            headers,
            Body::from_bytes(body.into_bytes()),
        ))
    }
}

#[tokio::main]
async fn main() -> Result<(), libcfd::Error> {
    tracing_subscriber::fmt::init();

    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "credentials.json".into());
    let tunnel = NamedTunnel::from_credentials_file(&path)?;
    println!(
        "loaded named tunnel {} (account {})",
        tunnel.tunnel_id, tunnel.account_tag
    );

    let connector = EdgeConnector::new(EdgeOptions::default());
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        println!("shutting down");
    };
    connector
        .run(
            Tunnel::named(tunnel),
            libcfd::Origin::http(HelloOrigin),
            shutdown,
        )
        .await
}
