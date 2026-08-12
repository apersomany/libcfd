//! The [`TcpOrigin`] trait consumers implement for raw TCP proxying.

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;
use crate::origin::duplex::Duplex;
use crate::origin::http::body::Request;

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
