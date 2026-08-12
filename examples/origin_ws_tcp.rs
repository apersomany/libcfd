//! Quick tunnel with websocket and TCP origin handlers that echo raw bytes.
//!
//! Run with: cargo run --example origin_ws_tcp
//!
//! Every websocket or TCP stream opened by the edge is echoed byte-for-byte
//! through a loopback TCP connection. The consumer owns the origin I/O: the
//! handler dials its own origin and hands the transport a raw duplex.

use libcfd::{
    Body, Duplex, EdgeConnector, EdgeOptions, HttpOrigin, Origin, QuickTunnelOptions, Request,
    Response, TcpOrigin, Transport, Tunnel, WebSocketConnection, WebSocketOrigin,
    create_quick_tunnel, websocket_accept,
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Clone)]
struct HelloOrigin;

impl HttpOrigin for HelloOrigin {
    async fn handle(&self, _request: Request) -> Result<Response, libcfd::Error> {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/plain"),
        );
        Ok(Response::new(
            http::StatusCode::OK,
            headers,
            Body::from_bytes(b"hello over http".to_vec()),
        ))
    }
}

/// Answers the websocket handshake and echoes the raw stream through a
/// loopback TCP connection to `addr`.
#[derive(Clone)]
struct EchoWebSocketOrigin {
    addr: std::net::SocketAddr,
}

impl WebSocketOrigin for EchoWebSocketOrigin {
    async fn connect(&self, request: Request) -> Result<WebSocketConnection, libcfd::Error> {
        let sock = tokio::net::TcpStream::connect(self.addr).await?;
        let (r, w) = sock.into_split();
        let mut headers = http::HeaderMap::new();
        let key = request
            .headers
            .get("sec-websocket-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        headers.insert(
            "Sec-WebSocket-Accept",
            websocket_accept(key).parse().unwrap(),
        );
        Ok(WebSocketConnection {
            response: Response::new(
                http::StatusCode::SWITCHING_PROTOCOLS,
                headers,
                Body::empty(),
            ),
            origin: Duplex::new(r.compat(), w.compat_write()),
        })
    }
}

/// Echoes the raw stream through a loopback TCP connection to `addr`.
#[derive(Clone)]
struct EchoTcpOrigin {
    addr: std::net::SocketAddr,
}

impl TcpOrigin for EchoTcpOrigin {
    async fn connect(&self, _request: Request) -> Result<Duplex, libcfd::Error> {
        let sock = tokio::net::TcpStream::connect(self.addr).await?;
        let (r, w) = sock.into_split();
        Ok(Duplex::new(r.compat(), w.compat_write()))
    }
}

#[tokio::main]
async fn main() -> Result<(), libcfd::Error> {
    tracing_subscriber::fmt::init();

    let options = QuickTunnelOptions::default();
    println!("requesting a quick tunnel from {}", options.service_url);
    let tunnel = create_quick_tunnel(&options).await?;
    println!("tunnel created: {}", tunnel.url());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });

    let origin = Origin::http(HelloOrigin)
        .with_websocket(EchoWebSocketOrigin { addr })
        .with_tcp(EchoTcpOrigin { addr });

    let connector = EdgeConnector::new(EdgeOptions {
        transport: Transport::Quic,
        ..Default::default()
    });
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        println!("shutting down");
    };
    connector.run(Tunnel::quick(tunnel), origin, shutdown).await
}
