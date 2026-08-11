//! Quick tunnel served through an axum Router (HTTP only).
//!
//! Run with: cargo run --example axum_tunnel --features axum-origin
//!
//! The tunnel runs until Ctrl-C. The printed hostname is the public URL.
//!
//! Websocket upgrades are not bridged: axum's `WebSocketUpgrade` drives the
//! upgraded connection inside a callback and never exposes the raw duplex
//! that libcfd's `WebSocketOrigin` needs, so the adapter is HTTP-only.

use axum::Router;
use axum::routing::{get, post};
use libcfd::{
    AxumOrigin, EdgeConnector, EdgeOptions, QuickTunnelOptions, Transport, Tunnel,
    create_quick_tunnel,
};

#[tokio::main]
async fn main() -> Result<(), libcfd::Error> {
    tracing_subscriber::fmt::init();

    let app = Router::new()
        .route("/", get(|| async { "hello from axum through libcfd!" }))
        .route(
            "/echo",
            post(|body: String| async move { format!("echo:{body}") }),
        );

    let options = QuickTunnelOptions::default();
    println!("requesting a quick tunnel from {}", options.service_url);
    let tunnel = create_quick_tunnel(&options).await?;
    println!("tunnel created: {}", tunnel.url());

    let connector = EdgeConnector::new(EdgeOptions {
        transport: Transport::Quic,
        ..Default::default()
    });
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        println!("shutting down");
    };
    connector
        .run(
            Tunnel::quick(tunnel),
            libcfd::Origin::http(AxumOrigin::new(app)),
            shutdown,
        )
        .await
}
