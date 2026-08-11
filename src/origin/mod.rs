//! Consumer-provided origin handling.
//!
//! A [`HttpOrigin`] receives every HTTP request that arrives from the
//! Cloudflare edge and produces the response that is sent back. The request
//! and response types are transport-neutral and runtime-agnostic.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::io::{self, AsyncRead};

use crate::error::Result;

/// An incoming HTTP request from the edge.
#[derive(Debug)]
pub struct Request {
    pub method: http::Method,
    pub uri: http::Uri,
    pub headers: http::HeaderMap,
    pub body: Body,
}

impl Request {
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
    pub status: http::StatusCode,
    pub headers: http::HeaderMap,
    pub body: Body,
}

impl Response {
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
    pub fn empty() -> Self {
        Self {
            inner: BodyInner::Empty,
            size_hint: Some(0),
        }
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let size_hint = Some(bytes.len() as u64);
        Self {
            inner: BodyInner::Bytes(futures_util::io::Cursor::new(bytes)),
            size_hint,
        }
    }

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

/// Handles HTTP requests from the edge.
///
/// Implementations must be `Send + Sync` so requests can be handled
/// concurrently. The returned future is `Send`; wrap with [`HttpOriginDyn`]
/// when object safety is needed.
pub trait HttpOrigin: Send + Sync {
    fn handle(&self, request: Request) -> impl Future<Output = Result<Response>> + Send + '_;
}

/// Object-safe version of [`HttpOrigin`] for boxed/dyn use.
pub trait HttpOriginDyn: Send + Sync {
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
