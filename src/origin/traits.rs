//! The origin handler traits consumers implement.

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;
use crate::origin::body::{Request, Response};
use crate::origin::duplex::{Duplex, WebSocketConnection};

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
