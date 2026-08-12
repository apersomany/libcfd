//! QUIC loopback tests through the mock edge.

use std::sync::Arc;
use std::time::Duration;

use libcfd_rpc::quic::{ConnectRequest, ConnectionType};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::edge::control::{self, RegistrationOptions};
use crate::edge::quic::QuicConnection;
use crate::edge::serve;
use crate::error::Result;
use crate::origin::{Body, Origin, Request, Response};
use crate::tunnel::Tunnel;

use super::make_tunnel;
use super::mock_edge::MockEdge;

#[tokio::test(flavor = "multi_thread")]
async fn quic_tunnel_end_to_end() {
    let certified = rcgen::generate_simple_self_signed(vec![
        "quic.cftunnel.com".to_string(),
        "localhost".to_string(),
    ])
    .expect("cert");
    let ca_pem = certified.cert.pem().into_bytes();
    let (edge_addr, edge_task) = MockEdge::start(&certified).await;

    let conn = tokio::time::timeout(
        Duration::from_secs(15),
        QuicConnection::connect(edge_addr, Some(&ca_pem)),
    )
    .await
    .expect("client handshake timeout")
    .expect("client should complete the quic handshake");
    let edge = edge_task.await.expect("mock edge task").expect("mock edge");
    edge.wait_established().await;

    let edge_control = Arc::new(edge);
    let control_task = tokio::spawn({
        let e = edge_control.clone();
        async move { e.serve_control().await }
    });
    let tunnel = Tunnel::quick(make_tunnel());
    let opts = RegistrationOptions::default();
    let (_details, _client) = tokio::time::timeout(
        Duration::from_secs(15),
        control::register(&conn, &tunnel, &opts, br#"{"ingress":[]}"#),
    )
    .await
    .expect("registration timeout")
    .expect("client should register with the mock edge");

    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
    let origin = Origin::http(move |mut request: Request| {
        let seen_tx = seen_tx.clone();
        async move {
            let mut body = request.body.collect().await.expect("body read");
            let mut headers = http::HeaderMap::new();
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("text/plain"),
            );
            let _ = seen_tx.send((
                request.method.as_str().to_string(),
                request.uri.to_string(),
                std::mem::take(&mut body),
            ));
            Ok(Response::new(
                http::StatusCode::OK,
                headers,
                Body::from_bytes(b"pong".to_vec()),
            ))
        }
    });

    let conn = Arc::new(conn);
    let serve_task = tokio::spawn(serve::serve_requests(conn.clone(), Arc::new(origin)));

    let (response, body) = edge_control
        .request_and_read()
        .await
        .expect("mock edge should get a response");
    assert_eq!(body, b"pong");
    let status = response
        .metadata
        .iter()
        .find(|(k, _)| k == "HttpStatus")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    assert_eq!(status, "200");

    let (method, uri, request_body) = seen_rx
        .recv()
        .await
        .expect("origin handler should observe the request");
    assert_eq!(method, "GET");
    assert_eq!(uri, "http://example.com/hello");
    assert_eq!(request_body, b"ping");

    serve_task.abort();
    control_task.abort();
    conn.close();
}

/// A websocket origin that answers with 101 and echoes the raw stream
/// through a loopback TCP connection to `addr`.
struct EchoWsOrigin {
    addr: std::net::SocketAddr,
}

impl crate::origin::WebSocketOrigin for EchoWsOrigin {
    async fn connect(&self, request: Request) -> Result<crate::origin::WebSocketConnection> {
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
            http::HeaderValue::from_str(&crate::origin::websocket_accept(key)).unwrap(),
        );
        Ok(crate::origin::WebSocketConnection {
            response: Response::new(
                http::StatusCode::SWITCHING_PROTOCOLS,
                headers,
                Body::empty(),
            ),
            origin: crate::origin::Duplex::new(r.compat(), w.compat_write()),
        })
    }
}

/// A TCP origin that echoes the raw stream through a loopback connection.
struct EchoTcpOrigin {
    addr: std::net::SocketAddr,
}

impl crate::origin::TcpOrigin for EchoTcpOrigin {
    async fn connect(&self, _request: Request) -> Result<crate::origin::Duplex> {
        let sock = tokio::net::TcpStream::connect(self.addr).await?;
        let (r, w) = sock.into_split();
        Ok(crate::origin::Duplex::new(r.compat(), w.compat_write()))
    }
}

async fn echo_server() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
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
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn quic_websocket_tcp_round_trip() {
    let certified = rcgen::generate_simple_self_signed(vec![
        "quic.cftunnel.com".to_string(),
        "localhost".to_string(),
    ])
    .expect("cert");
    let ca_pem = certified.cert.pem().into_bytes();
    let (edge_addr, edge_task) = MockEdge::start(&certified).await;
    let echo_addr = echo_server().await;

    let conn = tokio::time::timeout(
        Duration::from_secs(15),
        QuicConnection::connect(edge_addr, Some(&ca_pem)),
    )
    .await
    .expect("client handshake timeout")
    .expect("client should complete the quic handshake");
    let edge = edge_task.await.expect("mock edge task").expect("mock edge");
    edge.wait_established().await;

    let edge_control = Arc::new(edge);
    let control_task = tokio::spawn({
        let e = edge_control.clone();
        async move { e.serve_control().await }
    });
    let tunnel = Tunnel::quick(make_tunnel());
    let opts = RegistrationOptions::default();
    let (_details, _client) = tokio::time::timeout(
        Duration::from_secs(15),
        control::register(&conn, &tunnel, &opts, br#"{"ingress":[]}"#),
    )
    .await
    .expect("registration timeout")
    .expect("client should register with the mock edge");

    let origin = Origin::http(|_request: Request| async move {
        Ok(Response::new(
            http::StatusCode::NOT_FOUND,
            http::HeaderMap::new(),
            Body::empty(),
        ))
    })
    .with_websocket(EchoWsOrigin { addr: echo_addr })
    .with_tcp(EchoTcpOrigin { addr: echo_addr });

    let conn = Arc::new(conn);
    let serve_task = tokio::spawn(serve::serve_requests(conn.clone(), Arc::new(origin)));

    // Websocket stream (server-initiated id 5).
    let (ws_response, ws_echo) = edge_control
        .raw_stream_exchange(
            5,
            ConnectRequest {
                dest: "http://example.com/ws".into(),
                conn_type: ConnectionType::Websocket,
                metadata: vec![
                    ("HttpMethod".into(), "GET".into()),
                    ("HttpHost".into(), "example.com".into()),
                    (
                        "HttpHeader:sec-websocket-key".into(),
                        "dGhlIHNhbXBsZSBub25jZQ==".into(),
                    ),
                ],
            },
            b"ping-ws",
        )
        .await
        .expect("mock edge should get a websocket response");
    assert_eq!(ws_echo, b"ping-ws");
    let ws_status = ws_response
        .metadata
        .iter()
        .find(|(k, _)| k == "HttpStatus")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    assert_eq!(ws_status, "101");
    assert!(ws_response.metadata.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("HttpHeader:Sec-WebSocket-Accept")
            && v == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    }));

    // TCP stream (server-initiated id 5).
    let (tcp_response, tcp_echo) = edge_control
        .raw_stream_exchange(
            9,
            ConnectRequest {
                dest: "10.0.0.1:8080".into(),
                conn_type: ConnectionType::Tcp,
                metadata: vec![],
            },
            b"ping-tcp",
        )
        .await
        .expect("mock edge should get a tcp response");
    assert_eq!(tcp_echo, b"ping-tcp");
    assert!(tcp_response.error.is_empty());
    assert!(!tcp_response.metadata.iter().any(|(k, _)| k == "HttpStatus"));

    serve_task.abort();
    control_task.abort();
    conn.close();
}
