use crate::error::Result;
use crate::rpc::RpcClient;
use crate::tunnelrpc_capnp;

pub const REGISTRATION_SERVER_INTERFACE_ID: u64 = 0xf716_95ec_7fe8_5497;
pub const METHOD_REGISTER_CONNECTION: u16 = 0;
pub const METHOD_UNREGISTER_CONNECTION: u16 = 1;
pub const METHOD_UPDATE_LOCAL_CONFIGURATION: u16 = 2;

/// The client's connector identity, sent as `ConnectionOptions.client`.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    /// 16-byte connector UUID.
    pub client_id: Vec<u8>,
    pub features: Vec<String>,
    pub version: String,
    pub arch: String,
}

/// Parameters sent with `registerConnection`.
#[derive(Debug, Clone, Default)]
pub struct ConnectionOptions {
    pub client: ClientInfo,
    /// Raw IP bytes of the local edge-facing address.
    pub origin_local_ip: Vec<u8>,
    pub replace_existing: bool,
    pub compression_quality: u8,
    pub num_previous_attempts: u8,
}

/// Credentials proving ownership of the tunnel.
#[derive(Debug, Clone, Default)]
pub struct TunnelAuth {
    pub account_tag: String,
    pub tunnel_secret: Vec<u8>,
}

/// A rejected registration.
#[derive(Debug, Clone)]
pub struct ConnectionError {
    pub cause: String,
    /// Nanoseconds to wait before retrying.
    pub retry_after: i64,
    pub should_retry: bool,
}

/// A successful registration.
#[derive(Debug, Clone)]
pub struct ConnectionDetails {
    /// Per-connection UUID (16 bytes).
    pub uuid: Vec<u8>,
    /// Airport code of the edge colo.
    pub location_name: String,
    pub tunnel_is_remotely_managed: bool,
}

/// The `ConnectionResponse` union.
#[derive(Debug, Clone)]
pub enum ConnectionResponse {
    Error(ConnectionError),
    Details(ConnectionDetails),
}

/// A typed client for the tunnel registration interface.
///
/// Wraps an [`RpcClient`] and exposes only plain Rust types so the caller
/// never touches Cap'n Proto directly.
pub struct TunnelClient<S> {
    rpc: RpcClient<S>,
}

impl<S: crate::io::AsyncStream + Unpin> TunnelClient<S> {
    pub fn new(rpc: RpcClient<S>) -> Self {
        Self { rpc }
    }

    pub async fn bootstrap(&mut self) -> Result<()> {
        self.rpc.bootstrap().await.map(|_| ())
    }

    pub async fn register_connection(
        &mut self,
        auth: TunnelAuth,
        tunnel_id: &[u8],
        conn_index: u8,
        options: &ConnectionOptions,
    ) -> Result<ConnectionResponse> {
        let auth_ = auth;
        let tunnel_id = tunnel_id.to_vec();
        let options = options.clone();
        self.rpc
            .call(
                0,
                REGISTRATION_SERVER_INTERFACE_ID,
                METHOD_REGISTER_CONNECTION,
                |payload| {
                    let mut params = payload
                        .reborrow()
                        .init_content()
                        .init_as::<tunnelrpc_capnp::registration_server::register_connection_params::Builder>();
                    {
                        let mut a = params.reborrow().init_auth();
                        a.set_account_tag(&auth_.account_tag);
                        a.set_tunnel_secret(&auth_.tunnel_secret);
                    }
                    params.set_tunnel_id(&tunnel_id);
                    params.set_conn_index(conn_index);
                    {
                        let mut o = params.reborrow().init_options();
                        let mut c = o.reborrow().init_client();
                        c.set_client_id(&options.client.client_id);
                        let mut feats = c
                            .reborrow()
                            .init_features(options.client.features.len() as u32);
                        for (i, f) in options.client.features.iter().enumerate() {
                            feats.set(i as u32, f);
                        }
                        c.set_version(&options.client.version);
                        c.set_arch(&options.client.arch);
                        o.set_origin_local_ip(&options.origin_local_ip);
                        o.set_replace_existing(options.replace_existing);
                        o.set_compression_quality(options.compression_quality);
                        o.set_num_previous_attempts(options.num_previous_attempts);
                    }
                    payload.reborrow().init_cap_table(0);
                    Ok(())
                },
                |results| {
                    let rres = results
                        .reborrow()
                        .get_content()
                        .get_as::<tunnelrpc_capnp::registration_server::register_connection_results::Reader<'_>>()?;
                    let conn_resp = rres.reborrow().get_result()?;
                    match conn_resp.reborrow().get_result().which()? {                        tunnelrpc_capnp::connection_response::result::Error(e) => {
                            let e = e?;
                            Ok(ConnectionResponse::Error(ConnectionError {
                                cause: e.get_cause()?.to_str()?.to_string(),
                                retry_after: e.get_retry_after(),
                                should_retry: e.get_should_retry(),
                            }))
                        }
                        tunnelrpc_capnp::connection_response::result::ConnectionDetails(d) => {
                            let d = d?;
                            Ok(ConnectionResponse::Details(ConnectionDetails {
                                uuid: d.get_uuid()?.to_vec(),
                                location_name: d.get_location_name()?.to_str()?.to_string(),
                                tunnel_is_remotely_managed: d
                                    .get_tunnel_is_remotely_managed(),
                            }))
                        }
                    }
                },
            )
            .await
    }

    pub async fn unregister_connection(&mut self) -> Result<()> {
        self.rpc
            .call(
                0,
                REGISTRATION_SERVER_INTERFACE_ID,
                METHOD_UNREGISTER_CONNECTION,
                |payload| {
                    payload
                        .reborrow()
                        .init_content()
                        .init_as::<tunnelrpc_capnp::registration_server::unregister_connection_params::Builder>();
                    payload.reborrow().init_cap_table(0);
                    Ok(())
                },
                |_results| Ok(()),
            )
            .await
    }

    pub async fn update_local_configuration(&mut self, config: &[u8]) -> Result<()> {
        let config = config.to_vec();
        self.rpc
            .call(
                0,
                REGISTRATION_SERVER_INTERFACE_ID,
                METHOD_UPDATE_LOCAL_CONFIGURATION,
                |payload| {
                    let mut params = payload
                        .reborrow()
                        .init_content()
                        .init_as::<tunnelrpc_capnp::registration_server::update_local_configuration_params::Builder>();
                    params.set_config(&config);
                    payload.reborrow().init_cap_table(0);
                    Ok(())
                },
                |_results| Ok(()),
            )
            .await
    }

    pub fn into_inner(self) -> RpcClient<S> {
        self.rpc
    }

    /// Releases the registration capability and returns the underlying
    /// stream, mirroring capnp-go's client `Close()`.
    pub async fn close(self) -> Result<S> {
        self.rpc.close().await
    }
}

/// Convenience: `ConnectionResponse` with `should_retry` mapped to a typed
/// result so callers can distinguish retryable failures without parsing
/// strings.
impl ConnectionResponse {
    pub fn into_result(self) -> std::result::Result<ConnectionDetails, RegistrationFailure> {
        match self {
            Self::Details(d) => Ok(d),
            Self::Error(e) => Err(RegistrationFailure::from(e)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RegistrationFailure {
    Retryable { cause: String, retry_after: i64 },
    Permanent(String),
}

impl From<ConnectionError> for RegistrationFailure {
    fn from(e: ConnectionError) -> Self {
        if e.should_retry {
            Self::Retryable {
                cause: e.cause,
                retry_after: e.retry_after,
            }
        } else {
            Self::Permanent(e.cause)
        }
    }
}

impl std::fmt::Display for RegistrationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable { cause, retry_after } => {
                write!(
                    f,
                    "retryable registration failure ({cause}, retry after {retry_after}ns)"
                )
            }
            Self::Permanent(cause) => write!(f, "permanent registration failure: {cause}"),
        }
    }
}

impl std::error::Error for RegistrationFailure {}
