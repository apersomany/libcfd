//! EdgeConnector: edge discovery, connection establishment, retries, and
//! transport selection.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "h2-edge")]
use tokio::sync::Notify;

use std::net::SocketAddr;

use crate::control::{self, RegistrationOptions};
use crate::edge;
use crate::error::{Error, Result};
use crate::event::Event;
#[cfg(feature = "h2-edge")]
use crate::h2::{H2EdgeConnection, H2Shared};
use crate::origin::Origin;
#[cfg(feature = "quic-edge")]
use crate::quic::QuicConnection;
#[cfg(feature = "quic-edge")]
use crate::serve;
use crate::tunnel::Tunnel;

/// The transport used for a tunnel connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// QUIC only.
    #[cfg(feature = "quic-edge")]
    Quic,
    /// HTTP/2 only.
    #[cfg(feature = "h2-edge")]
    H2,
    /// Start with QUIC and fall back to HTTP/2 after repeated QUIC failures.
    #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
    Auto,
}

/// Options controlling how a tunnel connects to the edge.
#[derive(Debug, Clone)]
pub struct EdgeOptions {
    /// Transport selection policy.
    pub transport: Transport,
    /// Edge region override (`--region`); `None` uses the default SRV lookup.
    pub region: Option<String>,
    /// PEM-encoded CA certificates trusted in addition to the system store
    /// (mirrors cloudflared's `--ca-cert`).
    pub ca_cert_pem: Option<Vec<u8>>,
    /// JSON configuration pushed to the edge via `updateLocalConfiguration`
    /// for locally-managed tunnels.
    pub config_json: Vec<u8>,
    /// Per-connection establishment timeout.
    pub connect_timeout: Duration,
    /// Base reconnect delay between failed attempts (exponential backoff).
    /// Cloudflared's base is 1 second.
    pub backoff: Duration,
    /// Bounded time to wait for a graceful unregister and for in-flight
    /// requests to drain after shutdown.
    pub grace_period: Duration,
    /// QUIC failures before `Transport::Auto` falls back to HTTP/2.
    /// Cloudflared's default retry count is 5.
    pub max_quic_failures: u8,
}

impl Default for EdgeOptions {
    fn default() -> Self {
        Self {
            transport: default_transport(),
            region: None,
            ca_cert_pem: None,
            config_json: default_config_json().into(),
            connect_timeout: Duration::from_secs(15),
            backoff: Duration::from_secs(1),
            grace_period: Duration::from_secs(30),
            max_quic_failures: 5,
        }
    }
}

/// The default transport depends on which edge transports are enabled: auto
/// when both are, otherwise the only enabled one.
#[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
fn default_transport() -> Transport {
    Transport::Auto
}

#[cfg(all(feature = "quic-edge", not(feature = "h2-edge")))]
fn default_transport() -> Transport {
    Transport::Quic
}

#[cfg(all(not(feature = "quic-edge"), feature = "h2-edge"))]
fn default_transport() -> Transport {
    Transport::H2
}

/// The default local configuration payload, matching the shape cloudflared
/// sends for a quick tunnel (a single catch-all ingress rule).
pub fn default_config_json() -> &'static str {
    r#"{"ingress":[{"hostname":"","service":"http://127.0.0.1:8080"}],"warp-routing":{}}"#
}

/// The outcome of a single connection-and-serve attempt.
struct ServeAttempt {
    result: Result<()>,
    /// When the connection registered successfully, used to reset the
    /// reconnect backoff after a healthy connection period.
    registered_at: Option<std::time::Instant>,
    /// Whether the QUIC connection ended with an idle timeout, which
    /// cloudflared treats as an immediate reason to fall back to HTTP/2.
    quic_timed_out: bool,
}

impl ServeAttempt {
    fn failed(err: Error) -> Self {
        Self {
            result: Err(err),
            registered_at: None,
            quic_timed_out: false,
        }
    }
}

/// Parameters an established edge connection needs to register, serve, and
/// shut down.
struct EdgeRunParams {
    /// The edge address (used as the QUIC `originLocalIp`).
    #[cfg_attr(not(feature = "quic-edge"), allow(dead_code))]
    edge: SocketAddr,
    tunnel: Arc<Tunnel>,
    origin: Arc<Origin>,
    shutdown: Arc<Event>,
    config_json: Vec<u8>,
    grace_period: Duration,
    attempt: u32,
}

/// A transport-agnostic edge connection.
///
/// Implementations register via the libcfd-rpc control stream, dispatch
/// request streams to the shared [`Origin`], keep the connection alive, and
/// drain in-flight work bounded by the grace period on shutdown. The
/// [`EdgeConnector`] drives discovery, connection establishment and retries
/// over this abstraction.
trait EdgeConnection: Send {
    fn run(
        self: Box<Self>,
        params: EdgeRunParams,
    ) -> Pin<Box<dyn Future<Output = ServeAttempt> + Send + 'static>>;
}

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

/// Applies the transport selection policy after `quic_failures` failures.
#[cfg(feature = "quic-edge")]
#[cfg_attr(not(feature = "h2-edge"), allow(unused_variables))]
fn select_transport(requested: Transport, quic_failures: u8, max_quic_failures: u8) -> Transport {
    match requested {
        #[cfg(feature = "quic-edge")]
        Transport::Quic => Transport::Quic,
        #[cfg(feature = "h2-edge")]
        Transport::H2 => Transport::H2,
        #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
        Transport::Auto => {
            if quic_failures >= max_quic_failures {
                Transport::H2
            } else {
                Transport::Quic
            }
        }
    }
}

/// Without QUIC there is only the HTTP/2 transport to select.
#[cfg(not(feature = "quic-edge"))]
fn select_transport(requested: Transport, _quic_failures: u8, _max_quic_failures: u8) -> Transport {
    match requested {
        Transport::H2 => Transport::H2,
    }
}

/// The reconnect delay for a failed attempt, mirroring cloudflared's
/// `retry.BackoffHandler`: a random value in `[0, base * 2^retries)`.
fn retry_delay(retries: u32, base: Duration) -> Duration {
    let exponent = retries.min(30);
    let max_nanos = (base.as_nanos() as u64)
        .saturating_mul(1u64 << exponent)
        .min(1u64 << 62);
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    let mut buf = [0u8; 8];
    let _ = boring::rand::rand_bytes(&mut buf);
    let nanos = u64::from_le_bytes(buf) % max_nanos;
    Duration::from_nanos(nanos)
}

/// Establishes a connection for a transport and boxes it behind the
/// [`EdgeConnection`] abstraction.
async fn build_connection(
    transport: Transport,
    edge: SocketAddr,
    ca_cert_pem: Option<&[u8]>,
    connect_timeout: Duration,
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
        origin_local_ip: peer_ip_bytes(&edge),
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

#[cfg(feature = "quic-edge")]
fn peer_ip_bytes(addr: &SocketAddr) -> Vec<u8> {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets().to_vec(),
        std::net::IpAddr::V6(ip) => ip.octets().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "quic-edge")]
    #[test]
    fn quic_only_transport_never_falls_back() {
        assert_eq!(select_transport(Transport::Quic, 100, 5), Transport::Quic);
    }

    #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
    #[test]
    fn auto_falls_back_after_max_failures() {
        assert_eq!(select_transport(Transport::Auto, 0, 5), Transport::Quic);
        assert_eq!(select_transport(Transport::Auto, 4, 5), Transport::Quic);
        assert_eq!(select_transport(Transport::Auto, 5, 5), Transport::H2);
        assert_eq!(select_transport(Transport::Auto, 9, 5), Transport::H2);
    }

    #[cfg(feature = "h2-edge")]
    #[test]
    fn h2_stays_h2() {
        assert_eq!(select_transport(Transport::H2, 0, 5), Transport::H2);
    }

    #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
    #[test]
    fn default_transport_is_auto_when_both_edges_enabled() {
        assert_eq!(EdgeOptions::default().transport, Transport::Auto);
    }

    #[cfg(all(feature = "quic-edge", not(feature = "h2-edge")))]
    #[test]
    fn default_transport_is_quic_without_h2() {
        assert_eq!(EdgeOptions::default().transport, Transport::Quic);
    }

    #[cfg(all(not(feature = "quic-edge"), feature = "h2-edge"))]
    #[test]
    fn default_transport_is_h2_without_quic() {
        assert_eq!(EdgeOptions::default().transport, Transport::H2);
    }

    #[test]
    fn backoff_is_bounded_by_base_times_two_pow() {
        let base = Duration::from_secs(1);
        for retries in 0..10 {
            let delay = retry_delay(retries, base);
            let max = Duration::from_secs(1).saturating_mul(1 << retries.min(30));
            assert!(
                delay <= max,
                "retries={retries} delay={delay:?} max={max:?}"
            );
        }
    }

    #[test]
    fn zero_base_backoff_is_instant() {
        assert_eq!(retry_delay(5, Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn default_backoff_matches_cloudflared() {
        assert_eq!(EdgeOptions::default().backoff, Duration::from_secs(1));
        assert_eq!(EdgeOptions::default().max_quic_failures, 5);
    }
}
