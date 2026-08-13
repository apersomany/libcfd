//! Control stream handling: registration, configuration push, unregister.

use std::time::Duration;

use libcfd_rpc::AsyncStream;
use libcfd_rpc::tunnel::{
    ClientInformation, ConnectionOptions, ConnectionResponse, TunnelAuth, TunnelClient,
};

#[cfg(feature = "quic-edge")]
use crate::edge::quic::{QuicConnection, QuicStream};
use crate::error::{Error, Result};
use crate::tunnel::Tunnel;

/// The duplicate-connection marker the edge returns, mirroring
/// cloudflared's `DuplicateConnectionError`.
pub(crate) const DUPLICATE_CONNECTION_CAUSE: &str = "EDUPCONN";

/// Bound for a single registration RPC exchange, matching cloudflared's
/// default `--rpc-timeout` of 5 seconds.
pub(crate) const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Default feature list advertised at registration, matching
/// cloudflared's `features/defaultFeatures`.
const DEFAULT_FEATURES: &[&str] = &[
    "allow_remote_config",
    "serialized_headers",
    "support_datagram_v2",
    "support_quic_eof",
    "management_logs",
];

/// Options that go into `ConnectionOptions` at registration.
#[derive(Debug, Clone)]
pub(crate) struct RegistrationOptions {
    pub features: Vec<String>,
    pub number_previous_attempts: u8,
    pub origin_local_ip: Vec<u8>,
}

impl Default for RegistrationOptions {
    fn default() -> Self {
        Self {
            features: DEFAULT_FEATURES.iter().map(|f| (*f).to_string()).collect(),
            number_previous_attempts: 0,
            origin_local_ip: Vec::new(),
        }
    }
}

fn build_connection_options(options: &RegistrationOptions) -> ConnectionOptions {
    ConnectionOptions {
        client: ClientInformation {
            // Cloudflared keeps one connector UUID per process and reuses it for every new connection.
            client_identifier: connector_client_identifier().to_vec(),
            features: options.features.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            arch: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        },
        origin_local_ip: options.origin_local_ip.clone(),
        replace_existing: false,
        compression_quality: 0,
        number_previous_attempts: options.number_previous_attempts,
    }
}

fn connector_client_identifier() -> &'static [u8; 16] {
    static CLIENT_IDENTIFIER: std::sync::OnceLock<[u8; 16]> = std::sync::OnceLock::new();
    CLIENT_IDENTIFIER.get_or_init(|| {
        let mut identifier = [0u8; 16];
        let _ = getrandom::fill(&mut identifier);
        identifier
    })
}

/// The local socket IP as 4 or 16 bytes, sent as the registration
/// `originLocalIp` (cloudflared does the same).
pub(crate) fn peer_ip_bytes(address: &std::net::SocketAddr) -> Vec<u8> {
    match address.ip() {
        std::net::IpAddr::V4(ip) => ip.octets().to_vec(),
        std::net::IpAddr::V6(ip) => ip.octets().to_vec(),
    }
}

/// Registers the tunnel with the edge on an open control stream and pushes
/// the local configuration when the tunnel is not remotely managed.
///
/// Returns the registration details and the still-open client so the caller
/// can unregister later.
#[cfg(feature = "quic-edge")]
pub(crate) async fn register(
    connection: &QuicConnection,
    tunnel: &Tunnel,
    options: &RegistrationOptions,
    configuration_json: &[u8],
) -> Result<(
    libcfd_rpc::tunnel::ConnectionDetails,
    TunnelClient<QuicStream>,
)> {
    let stream = connection.open_control_stream();
    register_on_stream(stream, tunnel, options, configuration_json).await
}

/// Registers the tunnel over any control stream (QUIC stream 0 or the HTTP/2
/// control-stream request body).
pub(crate) async fn register_on_stream<S: AsyncStream + Unpin>(
    stream: S,
    tunnel: &Tunnel,
    options: &RegistrationOptions,
    configuration_json: &[u8],
) -> Result<(libcfd_rpc::tunnel::ConnectionDetails, TunnelClient<S>)> {
    let rpc = libcfd_rpc::RpcClient::new(stream);
    let mut client = TunnelClient::new(rpc);
    client.bootstrap().await?;

    let tunnel_identifier = tunnel.tunnel_identifier_bytes()?;
    let auth = TunnelAuth {
        account_tag: tunnel.account_tag().to_string(),
        tunnel_secret: tunnel.tunnel_secret().to_vec(),
    };
    let options = build_connection_options(options);
    let response = client
        .register_connection(auth, &tunnel_identifier, 0, &options)
        .await?;

    let details = match response {
        ConnectionResponse::Details(details) => details,
        ConnectionResponse::Error(e) => {
            if e.cause == DUPLICATE_CONNECTION_CAUSE {
                let _ = client.close().await;
                return Err(Error::duplicate_connection(e.cause));
            }
            let _ = client.close().await;
            return Err(Error::registration(e.into()));
        }
    };

    if !details.tunnel_is_remotely_managed
        && let Err(e) = client.update_local_configuration(configuration_json).await
    {
        tracing::debug!("unable to push local configuration: {e}");
    }

    Ok((details, client))
}

/// Unregisters the connection gracefully, bounded by the grace period so a
/// dead edge cannot hang shutdown (cloudflared uses the same bound).
pub(crate) async fn unregister<S: AsyncStream + Unpin>(
    client: TunnelClient<S>,
    grace_period: Duration,
) -> Result<()> {
    let result = tokio::time::timeout(grace_period, async {
        let mut client = client;
        match client.unregister_connection().await {
            Ok(()) => {}
            Err(e) => tracing::debug!("unregister failed: {e}"),
        }
        let rpc = client.into_inner();
        let _ = rpc.close().await;
    })
    .await;
    match result {
        Ok(()) => Ok(()),
        Err(_) => {
            tracing::debug!("unregister timed out after {grace_period:?}");
            Ok(())
        }
    }
}
