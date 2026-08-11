//! Creates a quick tunnel and serves HTTP requests over the HTTP/2 edge
//! transport.
//!
//! Run with: cargo run --example h2_tunnel
//!
//! The tunnel runs until Ctrl-C. The printed hostname is the public URL.

use libcfd::{
    Body, EdgeConnector, EdgeOptions, HttpOrigin, QuickTunnelOptions, Response, Transport, Tunnel,
    create_quick_tunnel,
};

#[derive(Clone)]
struct HelloOrigin;

impl HttpOrigin for HelloOrigin {
    async fn handle(&self, request: libcfd::Request) -> Result<libcfd::Response, libcfd::Error> {
        let body = format!(
            "hello over h2 from libcfd!\nmethod={}\nuri={}\n",
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

    let options = QuickTunnelOptions::default();
    println!("requesting a quick tunnel from {}", options.service_url);
    let tunnel = create_quick_tunnel(&options).await?;
    println!("tunnel created: {}", tunnel.url());

    let edge_options = EdgeOptions {
        transport: Transport::H2,
        ..Default::default()
    };
    let connector = EdgeConnector::new(edge_options);
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        println!("shutting down");
    };
    connector
        .run(
            Tunnel::quick(tunnel),
            libcfd::Origin::http(HelloOrigin),
            shutdown,
        )
        .await
}
