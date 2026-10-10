//! Edge connectivity: discovery, connections, registration, and serving.
//!
//! [`EdgeConnector`] orchestrates edge discovery, connection establishment,
//! retries, and transport selection. QUIC is enabled by `quic-edge`,
//! `quic-edge-quinn`, or `quic-edge-quiche`; HTTP/2 by `h2-edge`. Edge APIs
//! also require a tunnel feature. Network execution requires Tokio with
//! I/O and time enabled, without exposing its concrete types publicly.

pub(crate) mod configuration;
mod connector;
pub(crate) mod control;
mod discovery;
mod error;
pub use error::Error;
pub(crate) mod event;
pub(crate) mod transport;

pub(crate) use discovery::discover_edges;

pub use configuration::RemoteConfiguration;
pub use connector::{EdgeConnector, EdgeOptions, Transport, default_configuration_json};

/// An owned library task must never detach when its owner is cancelled.
#[cfg(any(feature = "h2-edge", quic_quiche))]
pub(crate) struct OwnedTask<T>(tokio::task::JoinHandle<T>);

#[cfg(any(feature = "h2-edge", quic_quiche))]
impl<T: Send + 'static> OwnedTask<T> {
    pub(crate) fn spawn(future: impl std::future::Future<Output = T> + Send + 'static) -> Self {
        Self(tokio::spawn(future))
    }

    pub(crate) fn abort(&self) {
        self.0.abort();
    }
}

#[cfg(any(feature = "h2-edge", quic_quiche))]
impl<T> std::future::Future for OwnedTask<T> {
    type Output = std::result::Result<T, tokio::task::JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0).poll(cx)
    }
}

#[cfg(any(feature = "h2-edge", quic_quiche))]
impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(all(test, any(feature = "h2-edge", quic_quiche)))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_owned_task_releases_its_resources() {
        let (released_tx, released_rx) = tokio::sync::oneshot::channel::<()>();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = OwnedTask::spawn(async move {
            let _resource = released_tx;
            started_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started_rx.await.unwrap();
        drop(task);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), released_rx)
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn timed_out_owned_task_is_aborted_and_joined() {
        let (released_tx, released_rx) = tokio::sync::oneshot::channel::<()>();
        let mut task = OwnedTask::spawn(async move {
            let _resource = released_tx;
            std::future::pending::<()>().await;
        });
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, &mut task)
                .await
                .is_err()
        );
        task.abort();
        assert!((&mut task).await.unwrap_err().is_cancelled());
        assert!(released_rx.await.is_err());
    }
}
