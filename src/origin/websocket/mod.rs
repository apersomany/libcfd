//! The [`WebSocketOrigin`] trait consumers implement for websocket upgrades.

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;
use crate::origin::duplex::WebSocketConnection;
use crate::origin::http::body::Request;

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
