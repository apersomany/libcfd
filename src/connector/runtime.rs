//! The transport-agnostic edge connection abstraction and its QUIC and
//! HTTP/2 implementations.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "h2-edge")]
use tokio::sync::Notify;

use crate::control::{self, RegistrationOptions};
use crate::error::Error;
use crate::error::Result;
use crate::event::Event;
#[cfg(feature = "h2-edge")]
use crate::h2::{H2EdgeConnection, H2Shared};
use crate::origin::Origin;
#[cfg(feature = "quic-edge")]
use crate::quic::QuicConnection;
#[cfg(feature = "quic-edge")]
use crate::serve;
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
    pub(crate) fn failed(err: Error) -> Self {
        Self {
            result: Err(err),
            registered_at: None,
            quic_timed_out: false,
        }
    }
}

/// Parameters an established edge connection needs to register, serve, and
/// shut down.
pub(crate) struct EdgeRunParams {
    /// The edge address (used as the QUIC `originLocalIp`).
    #[cfg_attr(not(feature = "quic-edge"), allow(dead_code))]
    pub edge: SocketAddr,
    pub tunnel: Arc<Tunnel>,
    pub origin: Arc<Origin>,
    pub shutdown: Arc<Event>,
    pub config_json: Vec<u8>,
    pub grace_period: Duration,
    pub attempt: u32,
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
        params: EdgeRunParams,
    ) -> Pin<Box<dyn Future<Output = ServeAttempt> + Send + 'static>>;
}

#[cfg(feature = "quic-edge")]
impl EdgeConnection for QuicConnection {
    fn run(
        self: Box<Self>,
        params: EdgeRunParams,
    ) -> Pin<Box<dyn Future<Output = ServeAttempt> + Send + 'static>> {
        Box::pin(run_quic(self, params))
    }
}

#[cfg(feature = "quic-edge")]
async fn run_quic(conn: Box<QuicConnection>, params: EdgeRunParams) -> ServeAttempt {
    let EdgeRunParams {
        edge,
        tunnel,
        origin,
        shutdown,
        config_json,
        grace_period,
        attempt,
    } = params;
    // cloudflared sends the edge address as the QUIC `originLocalIp`.
    let reg_opts = RegistrationOptions {
        origin_local_ip: control::peer_ip_bytes(&edge),
        num_previous_attempts: attempt.min(u8::MAX as u32) as u8,
        ..Default::default()
    };
    let (_details, client) = match tokio::time::timeout(
        control::RPC_TIMEOUT,
        control::register(&conn, &tunnel, &reg_opts, &config_json),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return ServeAttempt::failed(e),
        Err(_) => return ServeAttempt::failed(Error::Quic("registration timed out".into())),
    };
    let registered_at = Some(std::time::Instant::now());

    let conn = Arc::new(*conn);
    let mut serve_handle = tokio::spawn(serve::serve_requests(conn.clone(), origin));

    let serve_result = tokio::select! {
        _ = shutdown.notified() => None,
        result = &mut serve_handle => Some(result),
    };
    let _ = control::unregister(client, grace_period).await;
    let shutdown_fired = shutdown.is_fired();
    if shutdown_fired {
        // cloudflared waits out the grace period after unregistration so
        // in-flight requests can finish before the connection is closed.
        tokio::time::sleep(grace_period).await;
    }
    let quic_timed_out = conn.inner.lock().unwrap().timed_out;
    conn.close();
    if !shutdown_fired {
        serve_handle.abort();
    }
    let result = match serve_result {
        None => Ok(()),
        Some(Ok(Ok(()))) => Err(Error::Quic("serve loop ended unexpectedly".into())),
        Some(Ok(Err(e))) => Err(e),
        Some(Err(e)) => Err(Error::Quic(format!("serve task failed: {e}"))),
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
        params: EdgeRunParams,
    ) -> Pin<Box<dyn Future<Output = ServeAttempt> + Send + 'static>> {
        Box::pin(run_h2(self, params))
    }
}

#[cfg(feature = "h2-edge")]
async fn run_h2(conn: Box<H2EdgeConnection>, params: EdgeRunParams) -> ServeAttempt {
    let EdgeRunParams {
        tunnel,
        origin,
        shutdown,
        config_json,
        grace_period,
        attempt,
        ..
    } = params;
    let reg_opts = RegistrationOptions {
        origin_local_ip: conn.local_ip.clone(),
        num_previous_attempts: attempt.min(u8::MAX as u32) as u8,
        ..Default::default()
    };
    let registered = Event::new();
    let registered_wait = registered.clone();
    let shared = Arc::new(H2Shared {
        tunnel,
        origin,
        reg_opts: Arc::new(reg_opts),
        config_json: Arc::new(config_json),
        shutdown: shutdown.clone(),
        control_shutdown: Arc::new(Notify::new()),
        registered,
        grace_period,
    });
    let mut serve_handle = tokio::spawn(conn.serve(shared));

    // Registration completes on the control stream inside serve(); wait
    // for it so the reconnect backoff can be reset on success.
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
            // serve() breaks on shutdown and drains in-flight streams
            // and the unregister RPC; give it the grace period to finish.
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
                        result: Err(Error::H2(format!("serve task failed: {e}"))),
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
                Err(Error::H2("edge closed the connection".into()))
            }
        }
        Some(Ok(Err(e))) => Err(e),
        Some(Err(e)) => Err(Error::H2(format!("serve task failed: {e}"))),
    };
    ServeAttempt {
        result,
        registered_at,
        quic_timed_out: false,
    }
}
