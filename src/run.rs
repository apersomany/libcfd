//! Top-level orchestration entry points.
//!
//! [`run_quick_tunnel`] is the Phase A convenience API: a quick tunnel with
//! an HTTP-only origin over QUIC. The full API is [`EdgeConnector`], which
//! adds named tunnels, websocket/TCP origins, and HTTP/2 transport.

use std::time::Duration;

use crate::connector::{EdgeConnector, EdgeOptions, Transport, default_config_json};
use crate::error::Result;
use crate::origin::{HttpOrigin, Origin};
use crate::tunnel::{QuickTunnel, Tunnel};

/// Options controlling how a tunnel connects to the edge.
///
/// Quick tunnels always use QUIC (as cloudflared forces for them); use
/// [`EdgeOptions`] for transport selection.
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
            backoff: Duration::from_secs(10),
        }
    }
}

impl From<&RunOptions> for EdgeOptions {
    fn from(options: &RunOptions) -> Self {
        Self {
            transport: Transport::Quic,
            region: options.region.clone(),
            ca_cert_pem: options.ca_cert_pem.clone(),
            config_json: options.config_json.clone(),
            connect_timeout: options.connect_timeout,
            backoff: options.backoff,
            ..Default::default()
        }
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
    let connector = EdgeConnector::new(EdgeOptions::from(options));
    connector
        .run(Tunnel::quick(tunnel), Origin::http(origin), shutdown)
        .await
}
