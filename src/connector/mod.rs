//! EdgeConnector: edge discovery, connection establishment, retries, and
//! transport selection.

mod backoff;
mod options;
mod runtime;

use std::future::Future;
use std::sync::Arc;

use crate::edge;
use crate::error::{Error, Result};
use crate::event::Event;
#[cfg(feature = "h2-edge")]
use crate::h2::H2EdgeConnection;
use crate::origin::Origin;
#[cfg(feature = "quic-edge")]
use crate::quic::QuicConnection;
use crate::tunnel::Tunnel;

use options::select_transport;
pub use options::{EdgeOptions, Transport, default_config_json};

use backoff::retry_delay;
use runtime::{EdgeConnection, EdgeRunParams, ServeAttempt};

/// Orchestrates edge discovery, connection establishment, retries, and
/// transport selection for a tunnel.
#[derive(Debug, Clone)]
pub struct EdgeConnector {
    options: EdgeOptions,
}

impl EdgeConnector {
    /// Creates a connector from the given options.
    pub fn new(options: EdgeOptions) -> Self {
        Self { options }
    }

    /// The options this connector was created with.
    pub fn options(&self) -> &EdgeOptions {
        &self.options
    }

    /// Runs the tunnel until `shutdown` resolves or a permanent error
    /// occurs, reconnecting with exponential backoff on connection loss.
    pub async fn run(
        &self,
        tunnel: Tunnel,
        origin: Origin,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let shutdown_flag = Arc::new(Event::new());
        tokio::task::spawn({
            let flag = shutdown_flag.clone();
            async move {
                shutdown.await;
                flag.fire();
            }
        });
        let tunnel = Arc::new(tunnel);
        let origin = Arc::new(origin);
        #[cfg(feature = "quic-edge")]
        let mut quic_failures: u8 = 0;
        #[cfg(not(feature = "quic-edge"))]
        let quic_failures: u8 = 0;
        let mut attempt: u32 = 0;

        loop {
            let transport = select_transport(
                self.options.transport,
                quic_failures,
                self.options.max_quic_failures,
            );
            // A named tunnel's credentials can carry an `Endpoint` that acts
            // as the region (cloudflared treats region and endpoint as
            // interchangeable).
            let region = self
                .options
                .region
                .clone()
                .or_else(|| tunnel.region_override());
            let edges = match edge::discover_edges(region.as_deref()).await {
                Ok(edges) => edges,
                Err(e) => {
                    // Discovery failure is retryable: cloudflared keeps
                    // retrying rather than aborting the run.
                    tracing::warn!("edge discovery failed, retrying: {e}");
                    attempt = attempt.saturating_add(1);
                    let delay = retry_delay(attempt, self.options.backoff);
                    tracing::debug!(attempt, ?delay, "retrying edge discovery");
                    tokio::select! {
                        _ = shutdown_flag.notified() => return Ok(()),
                        _ = tokio::time::sleep(delay) => {}
                    }
                    continue;
                }
            };
            #[cfg(feature = "quic-edge")]
            let mut quic_broken = false;
            #[cfg(not(feature = "quic-edge"))]
            let _quic_broken = false;
            for edge in &edges {
                let attempt_result = tokio::select! {
                    _ = shutdown_flag.notified() => return Ok(()),
                    result = async {
                        let connection = match build_connection(
                            transport,
                            edge.addr,
                            self.options.ca_cert_pem.as_deref(),
                            self.options.connect_timeout,
                        )
                        .await
                        {
                            Ok(connection) => connection,
                            Err(e) => return ServeAttempt::failed(e),
                        };
                        connection
                            .run(EdgeRunParams {
                                edge: edge.addr,
                                tunnel: tunnel.clone(),
                                origin: origin.clone(),
                                shutdown: shutdown_flag.clone(),
                                config_json: self.options.config_json.clone(),
                                grace_period: self.options.grace_period,
                                attempt,
                            })
                            .await
                    } => result,
                };
                let ServeAttempt {
                    result,
                    registered_at,
                    quic_timed_out,
                } = attempt_result;
                #[cfg(not(feature = "quic-edge"))]
                let _ = quic_timed_out;
                match result {
                    Ok(()) => return Ok(()),
                    Err(Error::DuplicateConnection(_)) => {
                        tracing::warn!(addr = %edge.addr, "duplicate connection, trying next edge");
                        continue;
                    }
                    Err(e) if e.is_permanent() => return Err(e),
                    Err(e) => {
                        tracing::warn!(addr = %edge.addr, ?transport, "edge connection failed: {e}");
                    }
                }
                #[cfg(feature = "quic-edge")]
                if quic_timed_out && transport == Transport::Quic {
                    quic_broken = true;
                }
                if let Some(registered_at) = registered_at
                    && registered_at.elapsed() >= self.options.grace_period
                {
                    attempt = 0;
                    tracing::debug!("reconnect backoff reset after a healthy connection period");
                }
            }
            #[cfg(feature = "quic-edge")]
            if transport == Transport::Quic {
                if quic_broken {
                    // An idle-timeout QUIC failure falls back immediately
                    // (cloudflared's `isQuicBroken` path).
                    quic_failures = quic_failures
                        .saturating_add(1)
                        .max(self.options.max_quic_failures);
                } else {
                    quic_failures = quic_failures.saturating_add(1);
                }
            }
            attempt = attempt.saturating_add(1);
            let delay = retry_delay(attempt, self.options.backoff);
            tracing::debug!(attempt, ?delay, "reconnecting after edge failure");
            tokio::select! {
                _ = shutdown_flag.notified() => return Ok(()),
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }
}

/// Establishes a connection for a transport and boxes it behind the
/// [`EdgeConnection`] abstraction.
async fn build_connection(
    transport: Transport,
    edge: std::net::SocketAddr,
    ca_cert_pem: Option<&[u8]>,
    connect_timeout: std::time::Duration,
) -> Result<Box<dyn EdgeConnection>> {
    match transport {
        #[cfg(feature = "quic-edge")]
        Transport::Quic => {
            let conn =
                tokio::time::timeout(connect_timeout, QuicConnection::connect(edge, ca_cert_pem))
                    .await
                    .map_err(|_| Error::Quic("edge connection timed out".into()))??;
            Ok(Box::new(conn))
        }
        #[cfg(feature = "h2-edge")]
        Transport::H2 => {
            let (conn, _) = tokio::time::timeout(
                connect_timeout,
                H2EdgeConnection::connect(edge, ca_cert_pem),
            )
            .await
            .map_err(|_| Error::H2("edge connection timed out".into()))??;
            Ok(Box::new(conn))
        }
        #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
        Transport::Auto => unreachable!("transport selection resolves auto before connecting"),
    }
}
