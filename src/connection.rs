use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};

use anyhow::{anyhow, Result};
use capnp::message::{ReaderOptions, TypedBuilder};
use capnp_futures::serialize::{read_message, write_message};
use capnp_rpc::{rpc_twoparty_capnp::Side, twoparty::VatNetwork, RpcSystem};
use quinn::{
    ClientConfig as QuinnClientConfig, Connection as QuinnConnection, Endpoint as QuinnEndpoint,
};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore};
use tokio::{
    net::{lookup_host, ToSocketAddrs},
    task::{spawn_local, LocalSet},
};
use uuid::Uuid;

use crate::{
    generated::{
        cloudflare_ca, connect_request, connect_response, connection_response, registration_server,
    },
    tunnel_config::TunnelConfig,
};

pub use quinn::{RecvStream as QuinnRecvStream, SendStream as QuinnSendStream};

pub use crate::generated::ConnectionType;

#[derive(Debug)]
pub struct Connection {
    connection: QuinnConnection,
}

impl Connection {
    pub async fn new(
        tunn_cfg: TunnelConfig,
        conn_idx: u8,
        src_addr: SrcAddr,
        dst_addr: DstAddr,
    ) -> Result<(Self, ConnectionDetails)> {
        let endpoint = match src_addr {
            SrcAddr::Some(src_addr) => Self::create_client_endpoint(src_addr).await?,
            SrcAddr::Default => Self::create_client_endpoint((Ipv4Addr::UNSPECIFIED, 0)).await?,
        };
        let connection = match dst_addr {
            DstAddr::Some(dst_addr) => Self::initiate_connection(&endpoint, dst_addr).await?,
            DstAddr::Region1 => {
                Self::initiate_connection(&endpoint, "region1.argotunnel.com:7844").await?
            }
            DstAddr::Region2 => {
                Self::initiate_connection(&endpoint, "region2.argotunnel.com:7844").await?
            }
            DstAddr::Default => {
                Self::initiate_connection(&endpoint, "region2.argotunnel.com:7844").await?
            }
        };
        let connection_details = LocalSet::new()
            .run_until(Self::register_connection(&connection, tunn_cfg, conn_idx))
            .await?;
        Ok((Self { connection }, connection_details))
    }

    async fn create_client_endpoint(src_addr: impl ToSocketAddrs) -> Result<QuinnEndpoint> {
        let mut last_error = None;
        for src_addr in lookup_host(src_addr).await? {
            match QuinnEndpoint::client(src_addr) {
                Ok(result) => return Ok(result),
                Err(error) => last_error = Some(anyhow!(error)),
            }
        }
        if let Some(last_error) = last_error {
            Err(last_error)
        } else {
            Err(anyhow!("could not resolve src_addr to any address"))
        }
    }

    async fn initiate_connection(
        endpoint: &QuinnEndpoint,
        dst_addr: impl ToSocketAddrs,
    ) -> Result<QuinnConnection> {
        let mut last_error = None;
        for dst_addr in lookup_host(dst_addr).await? {
            let mut root_cert_store = RootCertStore::empty();
            root_cert_store.add(&cloudflare_ca())?;
            match endpoint.connect_with(
                QuinnClientConfig::new(Arc::new(
                    RustlsClientConfig::builder()
                        .with_safe_defaults()
                        .with_root_certificates(root_cert_store)
                        .with_no_client_auth(),
                )),
                dst_addr,
                "quic.cftunnel.com",
            ) {
                Ok(result) => match result.await {
                    Ok(result) => return Ok(result),
                    Err(error) => last_error = Some(anyhow!(error)),
                },
                Err(error) => last_error = Some(anyhow!(error)),
            }
        }
        if let Some(last_error) = last_error {
            Err(last_error)
        } else {
            Err(anyhow!("could not resolve dst_addr to any address"))
        }
    }

    async fn register_connection(
        connection: &QuinnConnection,
        tunn_cfg: TunnelConfig,
        conn_idx: u8,
    ) -> Result<ConnectionDetails> {
        let (send_stream, recv_stream) = connection.open_bi().await?;
        let mut rpc_system = RpcSystem::new(
            Box::new(VatNetwork::new(
                recv_stream,
                send_stream,
                Side::Client,
                ReaderOptions::new(),
            )),
            None,
        );
        let registration_client = rpc_system.bootstrap::<registration_server::Client>(Side::Server);
        let disconnector = rpc_system.get_disconnector();
        let local_driver = spawn_local(rpc_system);
        let mut register_connection_request = registration_client.register_connection_request();
        let mut auth_builder = register_connection_request.get().init_auth();
        auth_builder.set_account_tag(&tunn_cfg.account_tag);
        auth_builder.set_tunnel_secret(&tunn_cfg.tunnel_secret);
        register_connection_request
            .get()
            .set_tunnel_id(&tunn_cfg.tunnel_id);
        register_connection_request.get().set_conn_index(conn_idx);
        register_connection_request
            .get()
            .init_options()
            .init_client()
            .set_client_id(&[0; 16]);
        let register_connection_response = register_connection_request.send().promise.await?;
        let connection_details = match register_connection_response
            .get()?
            .get_result()?
            .get_result()
            .which()?
        {
            connection_response::result::Which::ConnectionDetails(connection_details) => {
                let connection_details = connection_details?;
                ConnectionDetails {
                    uuid: Uuid::from_slice(connection_details.get_uuid()?)?.to_string(),
                    location_name: connection_details.get_location_name()?.to_string()?,
                    tunnel_is_remotely_managed: connection_details.get_tunnel_is_remotely_managed(),
                }
            }
            connection_response::result::Which::Error(error) => {
                let error = error?;
                return Err(anyhow!(
                    "{} (should_retry = {}, retry_after = {})",
                    error.get_cause()?.to_string()?,
                    error.get_should_retry(),
                    error.get_retry_after()
                ));
            }
        };
        disconnector.await?;
        local_driver.await??;
        Ok(connection_details)
    }
}

pub enum SrcAddr<T: ToSocketAddrs = SocketAddr> {
    Some(T),
    Default,
}

pub enum DstAddr<T: ToSocketAddrs = SocketAddr> {
    Some(T),
    Region1,
    Region2,
    Default,
}

#[derive(Debug)]
pub struct ConnectionDetails {
    pub uuid: String,
    pub location_name: String,
    pub tunnel_is_remotely_managed: bool,
}

impl Connection {
    pub async fn accept(&self) -> Result<ConnectRequest> {
        let (send_stream, mut recv_stream) = self.connection.accept_bi().await?;
        let mut signature = [0; 8];
        recv_stream.read_exact(&mut signature).await?;
        if signature != SIGNATURE {
            return Err(anyhow!("unknown signature: {:02X?}", signature));
        }
        let connect_request_reader = read_message(&mut recv_stream, ReaderOptions::new()).await?;
        let root_reader = connect_request_reader.get_root::<connect_request::Reader>()?;
        let request_dest = root_reader.get_dest()?.to_string()?;
        let request_type = root_reader.get_type()?;
        let metadata_reader = root_reader.get_metadata()?;
        let mut metadata = HashMap::with_capacity(metadata_reader.len() as usize);
        for metadata_reader in metadata_reader.into_iter() {
            metadata.insert(
                metadata_reader.get_key()?.to_string()?,
                metadata_reader.get_val()?.to_string()?,
            );
        }
        Ok(ConnectRequest {
            request_dest,
            request_type,
            metadata,
            send_stream,
            recv_stream,
        })
    }
}

#[derive(Debug)]
pub struct ConnectRequest {
    pub request_dest: String,
    pub request_type: ConnectionType,
    pub metadata: HashMap<String, String>,
    send_stream: QuinnSendStream,
    pub recv_stream: QuinnRecvStream,
}

impl ConnectRequest {
    pub async fn respond_with(
        mut self,
        response: ConnectResponse,
    ) -> Result<(QuinnSendStream, QuinnRecvStream)> {
        let mut connect_response_builder = TypedBuilder::<connect_response::Owned>::new_default();
        let mut root_builder = connect_response_builder.init_root();
        match response {
            ConnectResponse::Metadata(metadata) => {
                let mut metadata_builder = root_builder.init_metadata(metadata.len() as u32);
                for (i, (key, val)) in metadata.into_iter().enumerate() {
                    let mut metadata_builder = metadata_builder.reborrow().get(i as u32);
                    metadata_builder.set_key(key);
                    metadata_builder.set_val(val);
                }
            }
            ConnectResponse::Error(error) => root_builder.set_error(error),
        }
        self.send_stream.write_all(&SIGNATURE).await?;
        write_message(
            &mut self.send_stream,
            &connect_response_builder.into_inner(),
        )
        .await?;
        Ok((self.send_stream, self.recv_stream))
    }
}

#[derive(Debug)]
pub enum ConnectResponse {
    Metadata(HashMap<String, String>),
    Error(String),
}

// Let's just treat the version b"01" as part of the signature for the sake of simplicity
const SIGNATURE: [u8; 8] = [0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E, b'0', b'1'];
