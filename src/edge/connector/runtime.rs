//! The transport-agnostic edge connection abstraction and its QUIC and
//! HTTP/2 implementations.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "h2-edge")]
use tokio::sync::Notify;

use crate::edge::configuration::EdgeConfigurationHandler;
use crate::edge::control::{self, RegistrationOptions};
use crate::edge::event::Event;
#[cfg(feature = "h2-edge")]
use crate::edge::h2::{H2EdgeConnection, H2Shared};
#[cfg(quic_any)]
use crate::edge::quic::QuicConnection;
#[cfg(quic_any)]
use crate::edge::serve;
use crate::error::Error;
use crate::error::Result;
use crate::origin::Origin;
use crate::tunnel::Tunnel;

/// The outcome of a single connection-and-serve attempt.
pub(crate) struct ServeAttempt {
    pub result: Result<()>,
    /// When the connection registered successfully, used to reset the
    /// reconnect backoff after a healthy connection period.
    pub registered_at: Option<std::time::Instant>,
    /// Whether the QUIC connection ended with an idle timeout, which
    /// cloudflared treats as an immediate reason to fall back to HTTP/2.
    pub quic_timed_out: bool,
}

impl ServeAttempt {
    pub(crate) fn failed(error: Error) -> Self {
        Self {
            result: Err(error),
            registered_at: None,
            quic_timed_out: false,
        }
    }
}

/// Parameters an established edge connection needs to register, serve, and
/// shut down.
pub(crate) struct EdgeRunParameters {
    /// The edge address (used as the QUIC `originLocalIp`).
    #[cfg_attr(not(quic_any), allow(dead_code))]
    pub edge: SocketAddr,
    pub tunnel: Arc<Tunnel>,
    pub origin: Arc<Origin>,
    pub shutdown: Arc<Event>,
    pub configuration_json: Vec<u8>,
    pub grace_period: Duration,
    pub attempt: u32,
    pub on_remote_configuration:
        Option<Arc<dyn Fn(crate::edge::RemoteConfiguration) + Send + Sync>>,
}

/// A transport-agnostic edge connection.
///
/// Implementations register via the libcfd-rpc control stream, dispatch
/// request streams to the shared [`Origin`], keep the connection alive, and
/// drain in-flight work bounded by the grace period on shutdown. The
/// [`EdgeConnector`](super::EdgeConnector) drives discovery, connection
/// establishment and retries over this abstraction.
pub(crate) trait EdgeConnection: Send {
    fn run(
        self: Box<Self>,
        parameters: EdgeRunParameters,
    ) -> Pin<Box<dyn Future<Output = ServeAttempt> + Send + 'static>>;
}

#[cfg(quic_any)]
impl EdgeConnection for QuicConnection {
    fn run(
        self: Box<Self>,
        parameters: EdgeRunParameters,
    ) -> Pin<Box<dyn Future<Output = ServeAttempt> + Send + 'static>> {
        Box::pin(run_quic(self, parameters))
    }
}

#[cfg(quic_any)]
async fn run_quic(connection: Box<QuicConnection>, parameters: EdgeRunParameters) -> ServeAttempt {
    let EdgeRunParameters {
        edge,
        tunnel,
        origin,
        shutdown,
        configuration_json,
        grace_period,
        attempt,
        on_remote_configuration,
    } = parameters;
    // cloudflared sends the edge address as the QUIC `originLocalIp`.
    let registration_options = RegistrationOptions {
        origin_local_ip: control::peer_ip_bytes(&edge),
        number_previous_attempts: attempt.min(u8::MAX as u32) as u8,
        ..Default::default()
    };
    let (_details, client) = match tokio::time::timeout(
        control::RPC_TIMEOUT,
        control::register(
            &connection,
            &tunnel,
            &registration_options,
            &configuration_json,
        ),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return ServeAttempt::failed(e),
        Err(_) => return ServeAttempt::failed(Error::quic("registration timed out")),
    };
    tracing::info!(
        tunnel_is_remotely_managed = _details.tunnel_is_remotely_managed,
        location = %_details.location_name,
        "registered with the edge"
    );
    let registered_at = Some(std::time::Instant::now());

    let connection = Arc::new(*connection);
    let configuration_handler = Arc::new(EdgeConfigurationHandler::new(on_remote_configuration));
    let mut serve_handle = tokio::spawn(serve::serve_requests(
        connection.clone(),
        origin,
        configuration_handler,
    ));

    let serve_result = tokio::select! {
        _ = shutdown.notified() => None,
        result = &mut serve_handle => Some(result),
    };
    let _ = control::unregister(client, grace_period).await;
    let shutdown_fired = shutdown.is_fired();
    if shutdown_fired {
        // cloudflared waits out the grace period after unregistration so in-flight requests finish before the connection closes.
        tokio::time::sleep(grace_period).await;
    }
    let quic_timed_out = connection.timed_out();
    connection.close();
    if !shutdown_fired {
        serve_handle.abort();
    }
    let result = match serve_result {
        None => Ok(()),
        Some(Ok(Ok(()))) => Err(Error::quic("serve loop ended unexpectedly")),
        Some(Ok(Err(e))) => Err(e),
        Some(Err(e)) => Err(Error::quic(format!("serve task failed: {e}"))),
    };
    ServeAttempt {
        result,
        registered_at,
        quic_timed_out,
    }
}

#[cfg(feature = "h2-edge")]
impl EdgeConnection for H2EdgeConnection {
    fn run(
        self: Box<Self>,
        parameters: EdgeRunParameters,
    ) -> Pin<Box<dyn Future<Output = ServeAttempt> + Send + 'static>> {
        Box::pin(run_h2(self, parameters))
    }
}

#[cfg(feature = "h2-edge")]
async fn run_h2(connection: Box<H2EdgeConnection>, parameters: EdgeRunParameters) -> ServeAttempt {
    let EdgeRunParameters {
        tunnel,
        origin,
        shutdown,
        configuration_json,
        grace_period,
        attempt,
        on_remote_configuration,
        ..
    } = parameters;
    let registration_options = RegistrationOptions {
        origin_local_ip: connection.local_ip.clone(),
        number_previous_attempts: attempt.min(u8::MAX as u32) as u8,
        ..Default::default()
    };
    let registered = Event::new();
    let registered_wait = registered.clone();
    let shared = Arc::new(H2Shared {
        tunnel,
        origin,
        registration_options: Arc::new(registration_options),
        configuration_json: Arc::new(configuration_json),
        configuration_handler: Arc::new(EdgeConfigurationHandler::new(on_remote_configuration)),
        shutdown: shutdown.clone(),
        control_shutdown: Arc::new(Notify::new()),
        registered,
        grace_period,
    });
    let mut serve_handle = tokio::spawn(connection.serve(shared));

    // Registration completes inside serve(); wait for it so the reconnect backoff resets on success.
    let registered_at = match tokio::time::timeout(control::RPC_TIMEOUT, async {
        let signal = registered_wait;
        signal.notified().await;
    })
    .await
    {
        Ok(()) => Some(std::time::Instant::now()),
        Err(_) => None,
    };

    let serve_result = tokio::select! {
        _ = shutdown.notified() => {
            // serve() breaks on shutdown and drains in-flight streams plus the unregister RPC; give it the grace period to finish.
            match tokio::time::timeout(grace_period, &mut serve_handle).await {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err(e))) => {
                    return ServeAttempt {
                        result: Err(e),
                        registered_at,
                        quic_timed_out: false,
                    }
                }
                Ok(Err(e)) => {
                    return ServeAttempt {
                        result: Err(Error::h2(format!("serve task failed: {e}"))),
                        registered_at,
                        quic_timed_out: false,
                    }
                }
                Err(_) => {
                    serve_handle.abort();
                    None
                }
            }
        }
        result = &mut serve_handle => Some(result),
    };
    let result = match serve_result {
        None => Ok(()),
        Some(Ok(Ok(()))) => {
            if shutdown.is_fired() {
                Ok(())
            } else {
                Err(Error::h2("edge closed the connection"))
            }
        }
        Some(Ok(Err(e))) => Err(e),
        Some(Err(e)) => Err(Error::h2(format!("serve task failed: {e}"))),
    };
    ServeAttempt {
        result,
        registered_at,
        quic_timed_out: false,
    }
}
