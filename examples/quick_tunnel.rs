//! Creates a quick tunnel and serves HTTP requests through a simple origin.
//!
//! Run with: cargo run --example quick_tunnel
//!
//! The tunnel runs until Ctrl-C. The printed hostname is the public URL.

use libcfd::{
    Body, HttpOrigin, HttpResponder, QuickTunnelOptions, Request, Response, RunOptions,
    create_quick_tunnel, run_quick_tunnel,
};

#[derive(Clone)]
struct HelloOrigin;

impl HttpOrigin for HelloOrigin {
    fn handle(&self, request: Request, respond: HttpResponder) {
        let body = format!(
            "hello from libcfd!\nmethod={}\nuri={}\n",
            request.method, request.uri
        );
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/plain"),
        );
        respond.send(Response::new(
            http::StatusCode::OK,
            headers,
            Body::from_bytes(body.into_bytes()),
        ));
    }
}

#[tokio::main]
async fn main() -> Result<(), libcfd::Error> {
    tracing_subscriber::fmt::init();

    let options = QuickTunnelOptions::default();
    println!("requesting a quick tunnel from {}", options.service_url);
    let tunnel = create_quick_tunnel(&options).await?;
    println!("tunnel created: {}", tunnel.url());

    let run_options = RunOptions::default();
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        println!("shutting down");
    };
    run_quick_tunnel(tunnel, HelloOrigin, shutdown, &run_options).await
}
