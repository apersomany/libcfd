//! The transport-agnostic edge connection abstraction and its QUIC and
//! HTTP/2 implementations.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::edge::configuration::EdgeConfigurationHandler;
use crate::edge::control::{self, RegistrationOptions};
use crate::edge::event::Event;
#[cfg(feature = "h2-edge")]
use crate::edge::transport::h2::{H2EdgeConnection, H2Shared};
#[cfg(quic_any)]
use crate::edge::transport::quic::QuicConnection;
#[cfg(quic_any)]
use crate::edge::transport::quic::serve;
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
/// request streams to the shared [`Origin`], and keep the connection alive.
/// Shutdown retains ownership through grace-period-bounded cleanup attempts.
/// Dropping a run closes its transport and aborts library-owned tasks.
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
    let registration = tokio::select! {
        _ = shutdown.notified() => return ServeAttempt { result: Ok(()), registered_at: None, quic_timed_out: false },
        result = tokio::time::timeout(
        control::RPC_TIMEOUT,
        control::register(
            &connection,
            &tunnel,
            &registration_options,
            &configuration_json,
        ),
        ) => result,
    };
    let (_details, client) = match registration {
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
    let serve_result = {
        let serving = serve::serve_requests(
            connection.clone(),
            origin,
            configuration_handler,
            shutdown.clone(),
            grace_period,
        );
        tokio::pin!(serving);
        tokio::select! {
            _ = shutdown.notified() => {
                let _ = tokio::join!(control::unregister(client, grace_period), &mut serving);
                None
            }
            result = &mut serving => {
                let _ = control::unregister(client, grace_period).await;
                Some(result)
            }
        }
    };
    let quic_timed_out = connection.timed_out();
    let mut connection = Arc::try_unwrap(connection)
        .unwrap_or_else(|_| unreachable!("serving released the connection"));
    connection.close_and_wait(grace_period).await;
    let result = match serve_result {
        None => Ok(()),
        Some(Ok(())) if shutdown.is_fired() => Ok(()),
        Some(Ok(())) => Err(Error::quic("serve loop ended unexpectedly")),
        Some(Err(e)) => Err(e),
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
        control_shutdown: Arc::new(Event::new()),
        registered,
        grace_period,
    });
    let serving = connection.serve(shared);
    tokio::pin!(serving);
    let mut registered_at = None;
    let serve_result = tokio::select! {
        biased;
        _ = registered_wait.notified() => {
            registered_at = Some(std::time::Instant::now());
            serving.await
        }
        result = &mut serving => result,
    };
    let result = match serve_result {
        Ok(()) if shutdown.is_fired() => Ok(()),
        Ok(()) => Err(Error::h2("edge closed the connection")),
        Err(e) => Err(e),
    };
    ServeAttempt {
        result,
        registered_at,
        quic_timed_out: false,
    }
}
