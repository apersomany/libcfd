//! Consumer-provided origin handling.
//!
//! A [`HttpOrigin`] receives every HTTP request that arrives from the
//! Cloudflare edge and produces the response that is sent back. The request
//! and response types are transport-neutral and runtime-agnostic.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::Result;

#[cfg(feature = "axum-origin")]
pub mod axum;

/// A read-only half of a [`Duplex`].
pub type ReadHalf = Pin<Box<dyn AsyncRead + Send>>;
/// A write-only half of a [`Duplex`].
pub type WriteHalf = Pin<Box<dyn AsyncWrite + Send>>;

/// A raw bidirectional byte stream between the edge and an origin handler.
///
/// Used for websocket and TCP connections once the transport switches to
/// raw streaming. The halves are runtime-agnostic (`futures_io` traits);
/// consumers typically split their own socket and pass the halves here.
pub struct Duplex {
    read: ReadHalf,
    write: WriteHalf,
}

impl Duplex {
    /// Builds a duplex from separate read and write halves.
    pub fn new<R, W>(read: R, write: W) -> Self
    where
        R: AsyncRead + Send + 'static,
        W: AsyncWrite + Send + 'static,
    {
        Self {
            read: Box::pin(read),
            write: Box::pin(write),
        }
    }

    /// Splits the duplex back into its read and write halves.
    pub fn into_parts(self) -> (ReadHalf, WriteHalf) {
        (self.read, self.write)
    }
}

impl AsyncRead for Duplex {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        self.read.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for Duplex {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.write.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.write.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.write.as_mut().poll_close(cx)
    }
}

/// An upgrade accepted by a [`WebSocketOrigin`]: the response headers to
/// send to the edge and the origin-side byte stream to pump.
pub struct WebSocketConnection {
    /// The response headers to send to the edge (e.g. `101` with
    /// `Sec-WebSocket-Accept`).
    pub response: Response,
    /// The origin-side byte stream to pump with the edge.
    pub origin: Duplex,
}

/// Handles websocket upgrades from the edge.
///
/// `connect` runs the origin-side handshake (the consumer owns all origin
/// I/O) and returns the response the edge should see, plus the origin byte
/// stream. The transport sends the response and then pumps bytes in both
/// directions between the edge stream and `origin`.
pub trait WebSocketOrigin: Send + Sync {
    /// Runs the origin-side websocket handshake and returns the response
    /// headers plus the origin byte stream to pump.
    fn connect(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<WebSocketConnection>> + Send + '_;
}

/// Object-safe version of [`WebSocketOrigin`] for boxed/dyn use.
pub trait WebSocketOriginDyn: Send + Sync {
    /// Object-safe variant of [`WebSocketOrigin::connect`].
    fn connect_boxed(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<WebSocketConnection>> + Send + '_>>;
}

impl<T: WebSocketOrigin> WebSocketOriginDyn for T {
    fn connect_boxed(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<WebSocketConnection>> + Send + '_>> {
        Box::pin(self.connect(request))
    }
}

/// Handles raw TCP connections from the edge.
///
/// `connect` establishes the consumer-side connection (consumers own origin
/// I/O) and returns the byte stream to pump with the edge. The destination
/// host is carried in `request.uri` (`http://<host>[:port]`).
pub trait TcpOrigin: Send + Sync {
    /// Establishes the consumer-side connection and returns the byte stream
    /// to pump with the edge.
    fn connect(&self, request: Request) -> impl Future<Output = Result<Duplex>> + Send + '_;
}

/// Object-safe version of [`TcpOrigin`] for boxed/dyn use.
pub trait TcpOriginDyn: Send + Sync {
    /// Object-safe variant of [`TcpOrigin::connect`].
    fn connect_boxed(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<Duplex>> + Send + '_>>;
}

impl<T: TcpOrigin> TcpOriginDyn for T {
    fn connect_boxed(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<Duplex>> + Send + '_>> {
        Box::pin(self.connect(request))
    }
}

/// The set of origin handlers a tunnel run dispatches to.
///
/// Every run needs an [`HttpOrigin`]; websocket and TCP handlers are
/// optional and enabled with [`Origin::with_websocket`] and
/// [`Origin::with_tcp`].
#[cfg_attr(
    not(all(
        any(feature = "quic-edge", feature = "h2-edge"),
        any(feature = "quick-tunnel", feature = "named-tunnel")
    )),
    allow(dead_code)
)]
pub struct Origin {
    pub(crate) http: Arc<dyn HttpOriginDyn>,
    pub(crate) websocket: Option<Arc<dyn WebSocketOriginDyn>>,
    pub(crate) tcp: Option<Arc<dyn TcpOriginDyn>>,
}

impl Origin {
    /// Creates an origin with an HTTP handler.
    pub fn http<O>(http: O) -> Self
    where
        O: HttpOrigin + Send + Sync + 'static,
    {
        Self {
            http: Arc::new(http),
            websocket: None,
            tcp: None,
        }
    }

    /// Adds a websocket handler.
    pub fn with_websocket<O>(mut self, websocket: O) -> Self
    where
        O: WebSocketOrigin + Send + Sync + 'static,
    {
        self.websocket = Some(Arc::new(websocket));
        self
    }

    /// Adds a raw TCP handler.
    pub fn with_tcp<O>(mut self, tcp: O) -> Self
    where
        O: TcpOrigin + Send + Sync + 'static,
    {
        self.tcp = Some(Arc::new(tcp));
        self
    }
}

/// An incoming HTTP request from the edge.
#[derive(Debug)]
pub struct Request {
    /// The request method.
    pub method: http::Method,
    /// The request target.
    pub uri: http::Uri,
    /// The request headers.
    pub headers: http::HeaderMap,
    /// The request body.
    pub body: Body,
}

impl Request {
    /// Builds a request from its parts.
    pub fn new(method: http::Method, uri: http::Uri, headers: http::HeaderMap, body: Body) -> Self {
        Self {
            method,
            uri,
            headers,
            body,
        }
    }
}

/// An HTTP response to send back to the edge.
#[derive(Debug)]
pub struct Response {
    /// The response status.
    pub status: http::StatusCode,
    /// The response headers.
    pub headers: http::HeaderMap,
    /// The response body.
    pub body: Body,
}

impl Response {
    /// Builds a response from its parts.
    pub fn new(status: http::StatusCode, headers: http::HeaderMap, body: Body) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// A 502 response, used when the origin handler fails or the request is
    /// malformed (cloudflared uses the same status for write errors).
    pub fn bad_gateway() -> Self {
        Self::new(
            http::StatusCode::BAD_GATEWAY,
            http::HeaderMap::new(),
            Body::empty(),
        )
    }
}

/// A streaming byte body.
///
/// Backed either by bytes owned in memory or by a reader that produces
/// chunks on demand. `Body` implements `futures_util::io::AsyncRead`.
pub struct Body {
    inner: BodyInner,
    size_hint: Option<u64>,
}

enum BodyInner {
    Empty,
    Bytes(futures_util::io::Cursor<Vec<u8>>),
    Reader(Pin<Box<dyn AsyncRead + Send>>),
}

impl Body {
    /// An empty body.
    pub fn empty() -> Self {
        Self {
            inner: BodyInner::Empty,
            size_hint: Some(0),
        }
    }

    /// A body backed by bytes owned in memory.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let size_hint = Some(bytes.len() as u64);
        Self {
            inner: BodyInner::Bytes(futures_util::io::Cursor::new(bytes)),
            size_hint,
        }
    }

    /// A streaming body backed by a reader that produces chunks on demand.
    pub fn from_reader(reader: impl AsyncRead + Send + 'static) -> Self {
        Self {
            inner: BodyInner::Reader(Box::pin(reader)),
            size_hint: None,
        }
    }

    /// The expected body length, when known.
    pub fn size_hint(&self) -> Option<u64> {
        self.size_hint
    }

    /// Reads the whole body into memory.
    pub async fn collect(&mut self) -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        io::copy(self, &mut buf).await?;
        Ok(buf)
    }
}

impl AsyncRead for Body {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut self.inner {
            BodyInner::Empty => Poll::Ready(Ok(0)),
            BodyInner::Bytes(cursor) => {
                futures_util::io::AsyncRead::poll_read(Pin::new(&mut *cursor), cx, buf)
            }
            BodyInner::Reader(reader) => reader.as_mut().poll_read(cx, buf),
        }
    }
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body")
            .field("size_hint", &self.size_hint)
            .finish()
    }
}

/// Computes the RFC 6455 `Sec-WebSocket-Accept` value for a challenge key.
///
/// Consumers implementing [`WebSocketOrigin`] use this to answer the
/// handshake in their `connect` method.
#[cfg_attr(
    not(all(
        feature = "h2-edge",
        any(feature = "quick-tunnel", feature = "named-tunnel")
    )),
    allow(dead_code)
)]
pub fn websocket_accept(challenge_key: &str) -> String {
    use base64::Engine as _;
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(challenge_key.as_bytes());
    hasher.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

/// Pumps bytes in both directions between an origin duplex and the edge
/// stream until both directions reach the end.
///
/// Mirrors cloudflared's `PipeBidirectional`: each direction closes only
/// its own destination write side when the source ends, and the other
/// direction keeps pumping until it ends as well.
#[cfg_attr(
    not(all(
        any(feature = "quic-edge", feature = "h2-edge"),
        any(feature = "quick-tunnel", feature = "named-tunnel")
    )),
    allow(dead_code)
)]
pub(crate) async fn pump<R, W>(origin: Duplex, edge_read: R, edge_write: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (mut origin_read, mut origin_write) = origin.into_parts();
    let mut edge_read = edge_read;
    let mut edge_write = edge_write;
    let mut edge_done = false;
    let mut origin_done = false;
    let mut e_buf = [0u8; 8192];
    let mut o_buf = [0u8; 8192];
    loop {
        if edge_done && origin_done {
            break;
        }
        tokio::select! {
            read = edge_read.read(&mut e_buf), if !edge_done => {
                match read {
                    Ok(0) => {
                        edge_done = true;
                        let _ = origin_write.close().await;
                    }
                    Ok(n) => {
                        if let Err(e) = origin_write.write_all(&e_buf[..n]).await {
                            tracing::debug!("origin write failed: {e}");
                            edge_done = true;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("edge read failed: {e}");
                        edge_done = true;
                    }
                }
            }
            read = origin_read.read(&mut o_buf), if !origin_done => {
                match read {
                    Ok(0) => {
                        origin_done = true;
                        let _ = edge_write.close().await;
                    }
                    Ok(n) => {
                        if let Err(e) = edge_write.write_all(&o_buf[..n]).await {
                            tracing::debug!("edge write failed: {e}");
                            origin_done = true;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("origin read failed: {e}");
                        origin_done = true;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Handles HTTP requests from the edge.
///
/// Implementations must be `Send + Sync` so requests can be handled
/// concurrently. The returned future is `Send`; wrap with [`HttpOriginDyn`]
/// when object safety is needed.
pub trait HttpOrigin: Send + Sync {
    /// Handles one HTTP request from the edge and produces the response.
    fn handle(&self, request: Request) -> impl Future<Output = Result<Response>> + Send + '_;
}

/// Object-safe version of [`HttpOrigin`] for boxed/dyn use.
pub trait HttpOriginDyn: Send + Sync {
    /// Object-safe variant of [`HttpOrigin::handle`].
    fn handle_boxed(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response>> + Send + '_>>;
}

impl<T: HttpOrigin> HttpOriginDyn for T {
    fn handle_boxed(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response>> + Send + '_>> {
        Box::pin(self.handle(request))
    }
}

impl<F, Fut> HttpOrigin for F
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Response>> + Send + 'static,
{
    fn handle(&self, request: Request) -> impl Future<Output = Result<Response>> + Send + '_ {
        (self)(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_websocket_accept() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(websocket_accept(key), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
