//! Top-level orchestration: run a tunnel end to end.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

use crate::control::{self, RegistrationOptions};
use crate::edge::{self, EdgeAddr};
use crate::error::{Error, Result};
use crate::origin::{HttpOrigin, HttpOriginDyn};
use crate::quic::QuicConnection;
use crate::serve;
use crate::tunnel::QuickTunnel;

/// Options controlling how a tunnel connects to the edge.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Edge region override (`--region`); `None` uses the default SRV lookup.
    pub region: Option<String>,
    /// PEM-encoded CA certificates trusted instead of the system store
    /// (mirrors cloudflared's `--ca-cert`).
    pub ca_cert_pem: Option<Vec<u8>>,
    /// JSON configuration pushed to the edge via `updateLocalConfiguration`
    /// for locally-managed tunnels.
    pub config_json: Vec<u8>,
    /// Per-connection establishment timeout.
    pub connect_timeout: Duration,
    /// Base reconnect delay between failed attempts (exponential backoff).
    pub backoff: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            region: None,
            ca_cert_pem: None,
            config_json: default_config_json().into(),
            connect_timeout: Duration::from_secs(15),
            backoff: Duration::from_secs(1),
        }
    }
}

/// The default local configuration payload, matching the shape cloudflared
/// sends for a quick tunnel (a single catch-all ingress rule).
pub fn default_config_json() -> &'static str {
    r#"{"ingress":[{"hostname":"","service":"http://127.0.0.1:8080"}],"warp-routing":{}}"#
}

/// A shutdown signal shared across the connection attempts.
struct Shutdown {
    fired: AtomicBool,
    notify: Notify,
}

impl Shutdown {
    fn new() -> Arc<Shutdown> {
        Arc::new(Shutdown {
            fired: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    fn fire(&self) {
        self.fired.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

/// Runs a quick tunnel: discover the edge, establish a QUIC connection,
/// register the tunnel, and serve HTTP requests to `origin` until `shutdown`
/// resolves or the connection cannot be re-established.
///
/// On connection loss the tunnel reconnects with exponential backoff, trying
/// each discovered edge address in turn.
pub async fn run_quick_tunnel<O>(
    tunnel: QuickTunnel,
    origin: O,
    shutdown: impl Future<Output = ()> + Send + 'static,
    options: &RunOptions,
) -> Result<()>
where
    O: HttpOrigin + Send + Sync + 'static,
{
    let origin: Arc<dyn HttpOriginDyn> = Arc::new(origin);
    let shutdown_flag = Shutdown::new();
    tokio::task::spawn({
        let flag = shutdown_flag.clone();
        async move {
            shutdown.await;
            flag.fire();
        }
    });
    let reg_opts = RegistrationOptions::default();
    let mut attempt: u32 = 0;

    loop {
        let edges = edge::discover_edges(options.region.as_deref()).await?;
        for edge in &edges {
            let result = tokio::select! {
                _ = shutdown_flag.notify.notified() => return Ok(()),
                result = run_on_edge(&tunnel, origin.clone(), edge, options, &reg_opts, &shutdown_flag) => result,
            };
            match result {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(addr = %edge.addr, "edge connection failed: {e}");
                }
            }
        }

        attempt = attempt.saturating_add(1);
        let delay = backoff_delay(attempt, options.backoff);
        tracing::debug!(attempt, ?delay, "reconnecting after edge failure");
        tokio::select! {
            _ = shutdown_flag.notify.notified() => return Ok(()),
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

fn backoff_delay(attempt: u32, base: Duration) -> Duration {
    let exp = 1u64 << attempt.min(6);
    let mut jitter_buf = [0u8; 8];
    let _ = boring::rand::rand_bytes(&mut jitter_buf);
    let jitter = u64::from_le_bytes(jitter_buf) % 500;
    let millis = base.as_millis() as u64 * exp + jitter;
    Duration::from_millis(millis.min(60_000))
}

async fn run_on_edge(
    tunnel: &QuickTunnel,
    origin: Arc<dyn HttpOriginDyn>,
    edge: &EdgeAddr,
    options: &RunOptions,
    reg_opts: &RegistrationOptions,
    shutdown: &Shutdown,
) -> Result<()> {
    let conn = tokio::time::timeout(
        options.connect_timeout,
        QuicConnection::connect(edge.addr, options.ca_cert_pem.as_deref()),
    )
    .await
    .map_err(|_| Error::Quic("edge connection timed out".into()))??;

    let (_details, client) = tokio::time::timeout(
        options.connect_timeout,
        control::register(&conn, tunnel, reg_opts, &options.config_json),
    )
    .await
    .map_err(|_| Error::Quic("registration timed out".into()))??;

    let conn = Arc::new(conn);
    let mut serve_handle = tokio::spawn(serve::serve_requests(conn.clone(), origin));

    let serve_result = tokio::select! {
        _ = shutdown.notify.notified() => None,
        result = &mut serve_handle => Some(result),
    };
    let _ = control::unregister(client).await;
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
