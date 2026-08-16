//! Consumer-provided origin handling.
//!
//! A [`HttpOrigin`] receives every HTTP request that arrives from the
//! Cloudflare edge and produces the response that is sent back. A
//! [`StreamOrigin`] handles websocket upgrades and raw TCP streams; the
//! responder type fixes which contract it satisfies. Outcomes form a
//! hierarchy: [`Response`] (headers plus a one-way body), then
//! [`WebSocketConnection`] (a `101` handshake plus a bidirectional
//! [`Stream`]), then the bare [`Stream`] the transport acknowledges itself.
//! The request and response types are transport-neutral and
//! runtime-agnostic.

mod error;
pub use error::Error;
mod pump;
mod responder;

#[cfg(feature = "axum-origin")]
pub mod axum;
pub mod http;
pub mod stream;

pub use self::http::body::{Body, Request, Response};
pub use http::HttpOrigin;
#[cfg(edge_conn)]
pub(crate) use pump::pump;
pub use pump::websocket_accept;
#[cfg(edge_conn)]
pub(crate) use responder::wait_outcome;
pub use responder::{HttpResponder, StreamResponder, TcpResponder, WebSocketResponder};
pub use stream::{ReadHalf, Stream, StreamOrigin, WebSocketConnection, WriteHalf};

use std::sync::Arc;

/// The set of origin handlers a tunnel run dispatches to.
///
/// Every run needs an [`HttpOrigin`]; websocket and TCP stream handlers are
/// optional and enabled with [`Origin::with_websocket`] and
/// [`Origin::with_tcp`].
#[cfg_attr(not(edge_conn), allow(dead_code))]
pub struct Origin {
    pub(crate) http: Arc<dyn HttpOrigin>,
    pub(crate) websocket: Option<Arc<dyn StreamOrigin<WebSocketResponder>>>,
    pub(crate) tcp: Option<Arc<dyn StreamOrigin<TcpResponder>>>,
}

impl Origin {
    /// Creates an origin with an HTTP handler.
    pub fn http<O>(http: O) -> Self
    where
        O: HttpOrigin + 'static,
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
        O: StreamOrigin<WebSocketResponder> + 'static,
    {
        self.websocket = Some(Arc::new(websocket));
        self
    }

    /// Adds a raw TCP handler.
    pub fn with_tcp<O>(mut self, tcp: O) -> Self
    where
        O: StreamOrigin<TcpResponder> + 'static,
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
