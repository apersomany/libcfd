//! EdgeConnector: edge discovery, connection establishment, retries, and
//! transport selection.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::control::{self, RegistrationOptions};
use crate::edge::{self, EdgeAddr};
use crate::error::{Error, Result};
use crate::h2::{H2EdgeConnection, H2Shared};
use crate::origin::Origin;
use crate::quic::QuicConnection;
use crate::serve;
use crate::shutdown::Shutdown;
use crate::tunnel::Tunnel;

/// The transport used for a tunnel connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// QUIC only.
    Quic,
    /// HTTP/2 only.
    H2,
    /// Start with QUIC and fall back to HTTP/2 after repeated QUIC failures.
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
    pub backoff: Duration,
    /// Bounded time to wait for a graceful unregister.
    pub grace_period: Duration,
    /// QUIC failures before `Transport::Auto` falls back to HTTP/2.
    pub max_quic_failures: u8,
}

impl Default for EdgeOptions {
    fn default() -> Self {
        Self {
            transport: Transport::Auto,
            region: None,
            ca_cert_pem: None,
            config_json: default_config_json().into(),
            connect_timeout: Duration::from_secs(15),
            backoff: Duration::from_secs(10),
            grace_period: Duration::from_secs(30),
            max_quic_failures: 3,
        }
    }
}

/// The default local configuration payload, matching the shape cloudflared
/// sends for a quick tunnel (a single catch-all ingress rule).
pub fn default_config_json() -> &'static str {
    r#"{"ingress":[{"hostname":"","service":"http://127.0.0.1:8080"}],"warp-routing":{}}"#
}

/// Orchestrates edge discovery, connection establishment, retries, and
/// transport selection for a tunnel.
#[derive(Debug, Clone)]
pub struct EdgeConnector {
    options: EdgeOptions,
}

impl EdgeConnector {
    pub fn new(options: EdgeOptions) -> Self {
        Self { options }
    }

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
        let shutdown_flag = Shutdown::new();
        tokio::task::spawn({
            let flag = shutdown_flag.clone();
            async move {
                shutdown.await;
                flag.fire();
            }
        });
        let tunnel = Arc::new(tunnel);
        let origin = Arc::new(origin);
        let mut quic_failures: u8 = 0;
        let mut attempt: u32 = 0;

        loop {
            let transport = select_transport(
                self.options.transport,
                quic_failures,
                self.options.max_quic_failures,
            );
            let edges = edge::discover_edges(self.options.region.as_deref()).await?;
            for edge in &edges {
                let result = tokio::select! {
                    _ = shutdown_flag.notified() => return Ok(()),
                    result = connect_and_serve(
                        transport,
                        &tunnel,
                        &origin,
                        edge,
                        &self.options,
                        &shutdown_flag,
                        attempt,
                    ) => result,
                };
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
            }
            if transport == Transport::Quic {
                quic_failures = quic_failures.saturating_add(1);
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
fn select_transport(requested: Transport, quic_failures: u8, max_quic_failures: u8) -> Transport {
    match requested {
        Transport::Quic => Transport::Quic,
        Transport::H2 => Transport::H2,
        Transport::Auto => {
            if quic_failures >= max_quic_failures {
                Transport::H2
            } else {
                Transport::Quic
            }
        }
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

async fn connect_and_serve(
    transport: Transport,
    tunnel: &Arc<Tunnel>,
    origin: &Arc<Origin>,
    edge: &EdgeAddr,
    options: &EdgeOptions,
    shutdown: &Arc<Shutdown>,
    attempt: u32,
) -> Result<()> {
    match transport {
        Transport::Quic => run_on_edge_quic(tunnel, origin, edge, options, shutdown, attempt).await,
        Transport::H2 => run_on_edge_h2(tunnel, origin, edge, options, shutdown, attempt).await,
        Transport::Auto => unreachable!("transport selection resolves auto before connecting"),
    }
}

async fn run_on_edge_quic(
    tunnel: &Arc<Tunnel>,
    origin: &Arc<Origin>,
    edge: &EdgeAddr,
    options: &EdgeOptions,
    shutdown: &Arc<Shutdown>,
    attempt: u32,
) -> Result<()> {
    let conn = tokio::time::timeout(
        options.connect_timeout,
        QuicConnection::connect(edge.addr, options.ca_cert_pem.as_deref()),
    )
    .await
    .map_err(|_| Error::Quic("edge connection timed out".into()))??;

    let reg_opts = RegistrationOptions {
        origin_local_ip: conn.local_ip(),
        num_previous_attempts: attempt.min(u8::MAX as u32) as u8,
        ..Default::default()
    };
    let (_details, client) = tokio::time::timeout(
        options.connect_timeout,
        control::register(&conn, tunnel, &reg_opts, &options.config_json),
    )
    .await
    .map_err(|_| Error::Quic("registration timed out".into()))??;

    let conn = Arc::new(conn);
    let mut serve_handle = tokio::spawn(serve::serve_requests(conn.clone(), origin.clone()));

    let serve_result = tokio::select! {
        _ = shutdown.notified() => None,
        result = &mut serve_handle => Some(result),
    };
    let _ = control::unregister(client, options.grace_period).await;
    conn.close();
    if !shutdown.is_fired() {
        serve_handle.abort();
    }
    match serve_result {
        None => Ok(()),
        Some(Ok(Ok(()))) => Err(Error::Quic("serve loop ended unexpectedly".into())),
        Some(Ok(Err(e))) => Err(e),
        Some(Err(e)) => Err(Error::Quic(format!("serve task failed: {e}"))),
    }
}

async fn run_on_edge_h2(
    tunnel: &Arc<Tunnel>,
    origin: &Arc<Origin>,
    edge: &EdgeAddr,
    options: &EdgeOptions,
    shutdown: &Arc<Shutdown>,
    attempt: u32,
) -> Result<()> {
    let (conn, local_ip) = tokio::time::timeout(
        options.connect_timeout,
        H2EdgeConnection::connect(edge.addr, options.ca_cert_pem.as_deref()),
    )
    .await
    .map_err(|_| Error::H2("edge connection timed out".into()))??;

    let reg_opts = RegistrationOptions {
        origin_local_ip: local_ip,
        num_previous_attempts: attempt.min(u8::MAX as u32) as u8,
        ..Default::default()
    };
    let shared = Arc::new(H2Shared {
        tunnel: tunnel.clone(),
        origin: origin.clone(),
        reg_opts: Arc::new(reg_opts),
        config_json: Arc::new(options.config_json.clone()),
        shutdown: shutdown.clone(),
        control_shutdown: Arc::new(Notify::new()),
        connect_timeout: options.connect_timeout,
        grace_period: options.grace_period,
    });
    let mut serve_handle = tokio::spawn(conn.serve(shared));

    let serve_result = tokio::select! {
        _ = shutdown.notified() => {
            // serve() breaks on shutdown and waits for the unregister RPC;
            // give it the grace period to finish.
            match tokio::time::timeout(options.grace_period, &mut serve_handle).await {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err(e))) => return Err(e),
                Ok(Err(e)) => return Err(Error::H2(format!("serve task failed: {e}"))),
                Err(_) => {
                    serve_handle.abort();
                    None
                }
            }
        }
        result = &mut serve_handle => Some(result),
    };
    match serve_result {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quic_only_transport_never_falls_back() {
        assert_eq!(select_transport(Transport::Quic, 100, 3), Transport::Quic);
    }

    #[test]
    fn auto_falls_back_after_max_failures() {
        assert_eq!(select_transport(Transport::Auto, 0, 3), Transport::Quic);
        assert_eq!(select_transport(Transport::Auto, 2, 3), Transport::Quic);
        assert_eq!(select_transport(Transport::Auto, 3, 3), Transport::H2);
        assert_eq!(select_transport(Transport::Auto, 9, 3), Transport::H2);
    }

    #[test]
    fn h2_stays_h2() {
        assert_eq!(select_transport(Transport::H2, 0, 3), Transport::H2);
    }

    #[test]
    fn backoff_is_bounded_by_base_times_two_pow() {
        let base = Duration::from_secs(10);
        for retries in 0..10 {
            let delay = retry_delay(retries, base);
            let max = Duration::from_secs(10).saturating_mul(1 << retries.min(30));
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
}
