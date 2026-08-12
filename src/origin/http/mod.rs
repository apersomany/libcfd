//! The [`HttpOrigin`] trait consumers implement for HTTP requests.

pub mod body;

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;
use crate::origin::http::body::{Request, Response};

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
