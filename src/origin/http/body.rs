//! Transport-neutral request, response and body types.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::io::{self, AsyncRead};

/// An incoming HTTP request from the edge.
#[derive(Debug)]
pub struct Request {
    /// The request method.
    pub method: http::Method,
    /// The request target.
    pub uri: http::Uri,
    /// The request headers.
    pub headers: http::HeaderMap,
    /// The request body, streamed incrementally from the edge as it
    /// arrives. Nothing is pre-buffered: readers pull bytes as the edge
    /// sends them, and a handler may respond before the body is fully
    /// consumed (the transport drains any unread remainder).
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

    /// Builds the TCP-proxy request the transports hand to a
    /// [`StreamOrigin<TcpResponder>`](crate::StreamOrigin):
    /// the destination host rides in the URI (`http://<host>[:port]`).
    #[cfg(edge_conn)]
    pub(crate) fn tcp(host: &str) -> Self {
        let uri = http::Uri::try_from(format!("http://{host}")).unwrap_or_default();
        Self::new(
            http::Method::GET,
            uri,
            http::HeaderMap::new(),
            Body::empty(),
        )
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
        let mut buffer = Vec::new();
        io::copy(self, &mut buffer).await?;
        Ok(buffer)
    }
}

impl AsyncRead for Body {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut self.inner {
            BodyInner::Empty => Poll::Ready(Ok(0)),
            BodyInner::Bytes(cursor) => {
                futures_util::io::AsyncRead::poll_read(Pin::new(&mut *cursor), cx, buffer)
            }
            BodyInner::Reader(reader) => reader.as_mut().poll_read(cx, buffer),
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
