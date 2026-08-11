//! An [`HttpOrigin`] adapter that serves HTTP requests through an
//! [`axum::Router`].
//!
//! Only the HTTP path is bridged. Axum's websocket support cannot be
//! adapted to libcfd's [`WebSocketOrigin`](crate::WebSocketOrigin):
//! [`WebSocketUpgrade::on_upgrade`](axum::extract::ws::WebSocketUpgrade::on_upgrade)
//! hands the upgraded connection to a closure that axum drives, so the raw
//! byte stream is never exposed to the caller, and axum's `WebSocket` type
//! is message-framed rather than a raw duplex. Bridging it would require
//! reimplementing RFC 6455 framing on top of the message API. Raw TCP
//! proxying is likewise outside axum's HTTP-only model, so [`TcpOrigin`]
//! (crate::TcpOrigin) has no axum adapter.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body as AxumBody, BodyDataStream};
use bytes::Bytes;
use futures_util::Stream;
use futures_util::io::AsyncRead;

use crate::error::{Error, Result};
use crate::origin::{Body, HttpOrigin, Request, Response};

/// Serves HTTP requests through an axum [`Router`](axum::Router).
///
/// The router is cloned per request (axum `Router` is cheaply cloneable).
/// The `axum-origin` feature gates this adapter so default builds stay
/// dependency-lean.
pub struct AxumOrigin {
    router: axum::Router,
}

impl AxumOrigin {
    pub fn new(router: axum::Router) -> Self {
        Self { router }
    }

    /// The wrapped router.
    pub fn router(&self) -> &axum::Router {
        &self.router
    }
}

impl HttpOrigin for AxumOrigin {
    async fn handle(&self, request: Request) -> Result<Response> {
        let Request {
            method,
            uri,
            mut headers,
            body,
        } = request;
        if let Some(size) = body.size_hint() {
            headers.insert(
                http::header::CONTENT_LENGTH,
                http::HeaderValue::from_str(&size.to_string())
                    .map_err(|e| Error::Origin(format!("bad content length: {e}")))?,
            );
        }
        let mut axum_request = http::Request::builder()
            .method(method)
            .uri(uri)
            .body(AxumBody::from_stream(BodyReadStream::new(body)))
            .map_err(|e| Error::Origin(format!("failed to build axum request: {e}")))?;
        *axum_request.headers_mut() = headers;

        let mut service = self.router.clone();
        // Router::call is infallible; axum converts handler errors into
        // responses.
        let axum_response = tower::Service::call(&mut service, axum_request)
            .await
            .map_err(|e| Error::Origin(format!("axum router failed: {e}")))?;

        let (parts, body) = axum_response.into_parts();
        let body = Body::from_reader(AxumBodyReader::new(body.into_data_stream()));
        Ok(Response::new(parts.status, parts.headers, body))
    }
}

/// Streams a libcfd [`Body`] as `Result<Bytes, _>` chunks for axum.
struct BodyReadStream {
    body: Option<Body>,
    buf: [u8; 8192],
    len: usize,
}

impl BodyReadStream {
    fn new(body: Body) -> Self {
        Self {
            body: Some(body),
            buf: [0u8; 8192],
            len: 0,
        }
    }
}

impl Stream for BodyReadStream {
    type Item = std::result::Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.len == 0 {
            let Self { body, buf, len } = self.as_mut().get_mut();
            let body = body.as_mut().expect("polled after stream end");
            match Pin::new(body).poll_read(cx, buf) {
                Poll::Ready(Ok(0)) => return Poll::Ready(None),
                Poll::Ready(Ok(n)) => *len = n,
                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(axum::Error::new(e)))),
                Poll::Pending => return Poll::Pending,
            }
        }
        let chunk = Bytes::copy_from_slice(&self.buf[..self.len]);
        self.len = 0;
        Poll::Ready(Some(Ok(chunk)))
    }
}

/// Reads an axum response body stream as a libcfd [`Body`].
struct AxumBodyReader {
    stream: Pin<Box<BodyDataStream>>,
    chunk: Option<Bytes>,
    pos: usize,
    eof: bool,
}

impl AxumBodyReader {
    fn new(stream: BodyDataStream) -> Self {
        Self {
            stream: Box::pin(stream),
            chunk: None,
            pos: 0,
            eof: false,
        }
    }
}

impl AsyncRead for AxumBodyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        loop {
            if self.eof {
                return Poll::Ready(Ok(0));
            }
            if let Some(chunk) = self.chunk.take() {
                if self.pos < chunk.len() {
                    let n = std::cmp::min(chunk.len() - self.pos, buf.len());
                    buf[..n].copy_from_slice(&chunk[self.pos..self.pos + n]);
                    self.pos += n;
                    if self.pos < chunk.len() {
                        self.chunk = Some(chunk);
                    }
                    return Poll::Ready(Ok(n));
                }
                self.pos = 0;
            }
            match self.stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => self.chunk = Some(chunk),
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(std::io::Error::other(e)));
                }
                Poll::Ready(None) => {
                    self.eof = true;
                    return Poll::Ready(Ok(0));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::origin::HttpOrigin;

    #[tokio::test]
    async fn axum_router_serves_through_the_adapter() {
        use axum::Router;
        use axum::routing::{get, post};

        let app = Router::new()
            .route("/hello", get(|| async { "hello from axum" }))
            .route(
                "/echo",
                post(|body: String| async move { format!("echo:{body}") }),
            );

        let origin = AxumOrigin::new(app);

        let request = Request::new(
            http::Method::GET,
            "http://example.com/hello".parse().unwrap(),
            http::HeaderMap::new(),
            Body::empty(),
        );
        let mut response = origin.handle(request).await.unwrap();
        assert_eq!(response.status, http::StatusCode::OK);
        assert_eq!(response.body.collect().await.unwrap(), b"hello from axum");

        let request = Request::new(
            http::Method::POST,
            "http://example.com/echo".parse().unwrap(),
            http::HeaderMap::new(),
            Body::from_bytes(b"payload".to_vec()),
        );
        let mut response = origin.handle(request).await.unwrap();
        assert_eq!(response.status, http::StatusCode::OK);
        assert_eq!(response.body.collect().await.unwrap(), b"echo:payload");
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        use axum::Router;
        use axum::routing::get;

        let origin = AxumOrigin::new(Router::new().route("/", get(|| async {})));
        let request = Request::new(
            http::Method::GET,
            "http://example.com/missing".parse().unwrap(),
            http::HeaderMap::new(),
            Body::empty(),
        );
        let response = origin.handle(request).await.unwrap();
        assert_eq!(response.status, http::StatusCode::NOT_FOUND);
    }
}
