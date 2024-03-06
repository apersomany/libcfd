use std::net::SocketAddr;

use anyhow::{Error, Result};
use hyper::{server::conn::http2, service::service_fn, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::{net::TcpSocket, spawn};
use tokio_rustls::TlsConnector;

use crate::tunnel::credential::{self, TunnelCredential};

use super::{body, rpc::RpcClient, tls};

pub async fn create_connection(
    credential: &TunnelCredential,
    connection_index: u8,
    bind_addr: SocketAddr,
    edge_addr: SocketAddr,
) -> Result<()> {
    let connector = TlsConnector::from(tls::client_config()?);
    let socket = match bind_addr {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    socket.bind(bind_addr)?;
    let connecting = connector.connect(
        "h2.cftunnel.com".try_into()?,
        socket.connect(edge_addr).await?,
    );
    let connection = connecting.await?;
    let credential = credential.clone();
    let connection = http2::Builder::new(TokioExecutor::new()).serve_connection(
        TokioIo::new(connection),
        service_fn(move |mut request| {
            let credential = credential.clone();
            async move {
                let (parts, body) = request.into_parts();
                let reader = body::reader(body);
                let (writer, body) = body::writer();
                let rpc_client = RpcClient::new(reader, writer);
                rpc_client
                    .register_connection(&credential.clone(), connection_index)
                    .await?;
                Ok::<_, Error>(Response::new(body))
            }
        }),
    );
    spawn(connection);
    Ok(())
}
