//! Origin handlers used by the live tests.
//!
//! The HTTP handler echoes the request path with a label; the websocket and
//! TCP handlers echo raw bytes through an in-process duplex stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use libcfd::{
    Body, HttpOrigin, HttpResponder, Request, Response, Stream, StreamOrigin, TcpResponder,
    WebSocketConnection, WebSocketResponder, websocket_accept,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::compat::TokioAsyncReadCompatExt;

/// Responds with `{label}:{request path}` and counts invocations.
#[derive(Clone)]
pub struct PathEchoOrigin {
    label: &'static str,
    served: Arc<AtomicUsize>,
}

impl PathEchoOrigin {
    /// Creates the handler with the given response label.
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            served: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// How many requests the handler has served.
    pub fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

impl HttpOrigin for PathEchoOrigin {
    fn handle(&self, request: Request, respond: HttpResponder) {
        self.served.fetch_add(1, Ordering::SeqCst);
        let body = format!("{}:{}", self.label, request.uri.path());
        respond.send(Response::new(
            http::StatusCode::OK,
            http::HeaderMap::new(),
            Body::from_bytes(body.into_bytes()),
        ));
    }
}

/// Echoes raw bytes through an in-process duplex: `app_end` loops the bytes
/// back into the returned stream, which the transport pumps with the edge.
fn echo_stream() -> Stream {
    let (mut app_end, libcfd_end) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut buffer = [0u8; 8192];
        loop {
            match app_end.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if app_end.write_all(&buffer[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    Stream::from_io(libcfd_end.compat())
}

/// Answers websocket upgrades with the RFC 6455 handshake and echoes the
/// raw frame bytes back.
#[derive(Clone)]
pub struct WebSocketEchoOrigin;

impl StreamOrigin<WebSocketResponder> for WebSocketEchoOrigin {
    fn connect(&self, request: Request, respond: WebSocketResponder) {
        let origin = echo_stream();
        let mut headers = http::HeaderMap::new();
        let key = request
            .headers
            .get("sec-websocket-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        headers.insert(
            "Sec-WebSocket-Accept",
            websocket_accept(key).parse().expect("valid header value"),
        );
        respond.upgrade(WebSocketConnection {
            response: Response::new(
                http::StatusCode::SWITCHING_PROTOCOLS,
                headers,
                Body::empty(),
            ),
            origin,
        });
    }
}

/// Answers raw TCP streams with an in-process echo.
#[derive(Clone)]
pub struct TcpEchoOrigin;

impl StreamOrigin<TcpResponder> for TcpEchoOrigin {
    fn connect(&self, _request: Request, respond: TcpResponder) {
        let origin = echo_stream();
        respond.stream(origin);
    }
}
