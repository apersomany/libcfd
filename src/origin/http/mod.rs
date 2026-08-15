//! The [`HttpOrigin`] trait consumers implement for HTTP requests.

pub mod body;

use crate::origin::http::body::Request;
use crate::origin::responder::Responder;

/// Handles HTTP requests from the edge.
///
/// Implementations must be `Send + Sync` so requests can be handled
/// concurrently. `handle` is synchronous: the outcome is written into
/// `respond` and the transport delivers it to the edge. Handlers that need
/// to await origin I/O (e.g. proxying to an origin server) spawn a task
/// that calls [`send`](Responder::send) or [`fail`](Responder::fail) when
/// the work completes.
pub trait HttpOrigin: Send + Sync {
    /// Handles one HTTP request from the edge and writes the response (or
    /// failure) into `respond`.
    fn handle(&self, request: Request, respond: Responder);
}

impl<F> HttpOrigin for F
where
    F: Fn(Request, Responder) + Send + Sync + 'static,
{
    fn handle(&self, request: Request, respond: Responder) {
        (self)(request, respond)
    }
}
