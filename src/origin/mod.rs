//! Consumer-provided origin handling.
//!
//! A [`HttpOrigin`] receives every HTTP request that arrives from the
//! Cloudflare edge and produces the response that is sent back. The request
//! and response types are transport-neutral and runtime-agnostic.

mod duplex;
mod error;
pub use error::Error;
mod pump;

#[cfg(feature = "axum-origin")]
pub mod axum;
pub mod http;
pub mod tcp;
pub mod websocket;

pub use self::http::body::{Body, Request, Response};
pub use duplex::{Duplex, ReadHalf, WebSocketConnection, WriteHalf};
pub use http::{HttpOrigin, HttpOriginDyn};
#[cfg(edge_conn)]
pub(crate) use pump::pump;
pub use pump::websocket_accept;
pub use tcp::{TcpOrigin, TcpOriginDyn};
pub use websocket::{WebSocketOrigin, WebSocketOriginDyn};

use std::sync::Arc;

/// The set of origin handlers a tunnel run dispatches to.
///
/// Every run needs an [`HttpOrigin`]; websocket and TCP handlers are
/// optional and enabled with [`Origin::with_websocket`] and
/// [`Origin::with_tcp`].
#[cfg_attr(not(edge_conn), allow(dead_code))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_websocket_accept() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(websocket_accept(key), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
