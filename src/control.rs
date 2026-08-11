//! Control stream handling: registration, configuration push, unregister.

use libcfd_rpc::RpcClient;
use libcfd_rpc::tunnel::{
    ClientInfo, ConnectionOptions, ConnectionResponse, TunnelAuth, TunnelClient,
};

use crate::error::{Error, Result};
use crate::quic::{QuicConnection, QuicStream};
use crate::tunnel::QuickTunnel;

/// Options that go into `ConnectionOptions` at registration.
#[derive(Debug, Clone, Default)]
pub(crate) struct RegistrationOptions {
    pub features: Vec<String>,
    pub num_previous_attempts: u8,
}

fn build_connection_options(opts: &RegistrationOptions) -> ConnectionOptions {
    let mut client_id = [0u8; 16];
    let _ = boring::rand::rand_bytes(&mut client_id);
    ConnectionOptions {
        client: ClientInfo {
            client_id: client_id.to_vec(),
            features: opts.features.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            arch: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        },
        origin_local_ip: Vec::new(),
        replace_existing: false,
        compression_quality: 0,
        num_previous_attempts: opts.num_previous_attempts,
    }
}

/// Registers the tunnel with the edge on the control stream and pushes the
/// local configuration when the tunnel is not remotely managed.
///
/// Returns the registration details and the still-open client so the caller
/// can unregister later.
pub(crate) async fn register(
    conn: &QuicConnection,
    tunnel: &QuickTunnel,
    opts: &RegistrationOptions,
    config_json: &[u8],
) -> Result<(
    libcfd_rpc::tunnel::ConnectionDetails,
    TunnelClient<QuicStream>,
)> {
    let stream = conn.open_control_stream();
    let rpc = RpcClient::new(stream);
    let mut client = TunnelClient::new(rpc);
    client.bootstrap().await?;

    let tunnel_id = tunnel.tunnel_id_bytes()?;
    let auth = TunnelAuth {
        account_tag: tunnel.account_tag.clone(),
        tunnel_secret: tunnel.secret.clone(),
    };
    let options = build_connection_options(opts);
    let response = client
        .register_connection(auth, &tunnel_id, 0, &options)
        .await?;

    let details = match response {
        ConnectionResponse::Details(details) => details,
        ConnectionResponse::Error(e) => {
            let _ = client.close().await;
            return Err(Error::Registration(e.into()));
        }
    };

    if !details.tunnel_is_remotely_managed
        && let Err(e) = client.update_local_configuration(config_json).await
    {
        tracing::debug!("unable to push local configuration: {e}");
    }

    Ok((details, client))
}

/// Unregisters the connection gracefully.
pub(crate) async fn unregister(mut client: TunnelClient<QuicStream>) -> Result<()> {
    match client.unregister_connection().await {
        Ok(()) => {}
        Err(e) => tracing::debug!("unregister failed: {e}"),
    }
    let rpc = client.into_inner();
    let _ = rpc.close().await;
    Ok(())
}
