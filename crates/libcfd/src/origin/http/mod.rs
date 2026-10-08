//! The [`HttpOrigin`] trait consumers implement for HTTP requests.

pub mod body;

use crate::origin::http::body::Request;
use crate::origin::responder::HttpResponder;

/// Handles HTTP requests from the edge.
///
/// Implementations must be `Send + Sync` so requests can be handled
/// concurrently. `handle` is synchronous: the outcome is written into
/// `respond` and the transport delivers it to the edge. Handlers that need
/// to await origin I/O (e.g. proxying to an origin server) spawn a task
/// that calls [`send`](HttpResponder::send) or [`fail`](HttpResponder::fail)
/// when the work completes.
///
/// The request body streams from the edge: handlers read it incrementally
/// through `request.body` (`futures_util::io::AsyncRead`) and may respond
/// before it is fully consumed.
pub trait HttpOrigin: Send + Sync {
    /// Handles one HTTP request from the edge and writes the response (or
    /// failure) into `respond`.
    fn handle(&self, request: Request, respond: HttpResponder);
}

impl<F> HttpOrigin for F
where
    F: Fn(Request, HttpResponder) + Send + Sync + 'static,
{
    fn handle(&self, request: Request, respond: HttpResponder) {
        (self)(request, respond)
    }
}

#[cfg(all(test, edge_conn))]
mod tests {
    use futures_util::io::AsyncReadExt;

    use super::*;
    use crate::origin::http::body::{Body, Response};
    use crate::origin::wait_outcome;

    /// Reads the request body in 3-byte chunks and echoes the bytes back,
    /// proving incremental streaming reads work through the responder.
    struct EchoBody;

    impl HttpOrigin for EchoBody {
        fn handle(&self, mut request: Request, respond: HttpResponder) {
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 3];
                loop {
                    match request.body.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                        Err(e) => return respond.fail(format!("read failed: {e}")),
                    }
                }
                respond.send(Response::new(
                    http::StatusCode::OK,
                    http::HeaderMap::new(),
                    Body::from_bytes(bytes),
                ));
            });
        }
    }

    #[tokio::test]
    async fn handler_streams_request_body() {
        let payload = vec![0xABu8; 100_000];
        let request = Request::new(
            http::Method::GET,
            http::Uri::from_static("http://example.com/upload"),
            http::HeaderMap::new(),
            Body::from_bytes(payload.clone()),
        );
        let (respond, receiver) = HttpResponder::channel();
        EchoBody.handle(request, respond);
        let response = wait_outcome(receiver).await.expect("origin failed");
        let mut body = response.body;
        let echoed = body.collect().await.expect("response body read failed");
        assert_eq!(echoed, payload);
    }
}
