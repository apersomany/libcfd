//! HTTP/2 edge connection.
//!
//! Mirrors cloudflared's `connection/http2.go`: libcfd dials the edge over
//! TLS (SNI `h2.cftunnel.com`, no ALPN) and acts as the HTTP/2 server; the
//! edge opens streams toward us. The first edge stream carrying
//! `Cf-Cloudflared-Proxy-Connection-Upgrade: control-stream` hosts the
//! registration RPC in its body.

pub(crate) mod headers;
mod register;
pub(crate) mod stream;
mod streams;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::net::TcpStream;

use crate::edge::configuration::EdgeConfigurationHandler;
use crate::edge::control::{self, RegistrationOptions};
use crate::edge::event::Event;
use crate::edge::transport::tls;
use crate::error::{Error, Result};
use crate::origin::Origin;
use crate::tunnel::Tunnel;

pub(crate) use crate::origin::websocket_accept;
pub(crate) use headers::{
    CONFIGURATION_UPDATE, CONTROL_STREAM_UPGRADE, INTERNAL_TCP_SRC_HEADER, INTERNAL_UPGRADE_HEADER,
    WEBSOCKET_UPGRADE,
};

type TlsStream = tokio_rustls::client::TlsStream<TcpStream>;

/// The TLS server name cloudflared uses for HTTP/2 edge connections.
const EDGE_H2_SNI: &str = "h2.cftunnel.com";

/// State shared between the HTTP/2 connection task and per-stream tasks.
pub(crate) struct H2Shared {
    pub tunnel: Arc<Tunnel>,
    pub origin: Arc<Origin>,
    pub registration_options: Arc<RegistrationOptions>,
    pub configuration_json: Arc<Vec<u8>>,
    pub configuration_handler: Arc<EdgeConfigurationHandler>,
    pub shutdown: Arc<Event>,
    pub control_shutdown: Arc<Event>,
    /// Fires once registration completes on the control stream.
    pub registered: Event,
    pub grace_period: Duration,
}

/// An HTTP/2 connection to the edge.
pub(crate) struct H2EdgeConnection {
    connection: h2::server::Connection<TlsStream, Bytes>,
    /// The local socket IP (4 or 16 bytes), sent as `originLocalIp`.
    pub(crate) local_ip: Vec<u8>,
}

impl H2EdgeConnection {
    /// Dials the edge and completes the TLS + HTTP/2 handshakes. Returns the
    /// connection and the local socket IP (for `originLocalIp`).
    pub(crate) async fn connect(
        peer: SocketAddr,
        ca_cert_pem: Option<&[u8]>,
    ) -> Result<(H2EdgeConnection, Vec<u8>)> {
        let configuration = tls::tls_client_config(ca_cert_pem)?;
        let connector = tokio_rustls::TlsConnector::from(Arc::new(configuration));

        let tcp = TcpStream::connect(peer).await?;
        let local_ip = control::peer_ip_bytes(&tcp.local_addr()?);
        let server_name = rustls_pki_types::ServerName::try_from(EDGE_H2_SNI.to_string())
            .map_err(|e| Error::h2(format!("invalid edge sni: {e}")))?;
        let tls = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| Error::h2(format!("tls handshake failed: {e}")))?;

        let mut builder = h2::server::Builder::new();
        builder.max_concurrent_streams(u32::MAX);
        let connection = builder
            .handshake(tls)
            .await
            .map_err(|e| Error::h2(format!("http2 handshake failed: {e}")))?;
        Ok((
            H2EdgeConnection {
                connection,
                local_ip: local_ip.clone(),
            },
            local_ip,
        ))
    }

    /// Serves the connection until the edge closes it or shutdown fires:
    /// accepts edge streams, runs the registration RPC on the control
    /// stream, and dispatches request streams to the origin handlers.
    ///
    /// On shutdown, drives the connection while draining streams and the
    /// control task's unregister within a shared grace-period timeout, then
    /// aborts and joins remaining tasks. Dropping the future aborts its tasks
    /// and drops the underlying connection without graceful cleanup.
    pub(crate) async fn serve(mut self, shared: Arc<H2Shared>) -> Result<()> {
        let (registration_tx, mut registration_rx) = tokio::sync::oneshot::channel();
        let mut registration_tx = Some(registration_tx);
        let mut control_task: Option<crate::edge::OwnedTask<Result<()>>> = None;
        let mut registration_done = false;
        let mut stream_tasks: tokio::task::JoinSet<Result<()>> = tokio::task::JoinSet::new();
        let registration_timeout = tokio::time::sleep(control::RPC_TIMEOUT);
        tokio::pin!(registration_timeout);
        let result = loop {
            tokio::select! {
                request = self.connection.accept() => {
                    match request {
                        Some(Ok((request, mut respond))) => {
                            if classify(request.headers()) == StreamType::Control {
                                if control_task.is_none() {
                                    let shared = shared.clone();
                                    let registration_tx = registration_tx.take().expect("control stream handled once");
                                    control_task = Some(crate::edge::OwnedTask::spawn(async move {
                                        register::handle_control_stream(request, respond, shared, registration_tx).await
                                    }));
                                } else {
                                    let _ = respond.send_response(
                                        http::Response::builder().status(400).body(()).unwrap(),
                                        true,
                                    );
                                }
                            } else {
                                let shared = shared.clone();
                                stream_tasks.spawn(async move {
                                    streams::handle_stream(request, respond, shared).await
                                });
                            }
                        }
                        Some(Err(e)) => {
                            break Err(Error::h2(format!("connection error: {e}")));
                        }
                        None => {
                            break Ok(());
                        }
                    }
                }
                result = &mut registration_rx, if !registration_done => {
                    registration_done = true;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            break Err(e);
                        }
                        Err(_) => {
                            break Err(Error::h2("control stream ended before registration"));
                        }
                    }
                }
                _ = &mut registration_timeout, if !registration_done => {
                    break Err(Error::h2("registration timed out"));
                }
                _ = shared.shutdown.notified() => {
                    break Ok(());
                }
                joined = stream_tasks.join_next(), if !stream_tasks.is_empty() => {
                    if let Some(Ok(Err(error))) = joined {
                        tracing::debug!(%error, "request stream failed");
                    }
                }
            }
        };
        shared.control_shutdown.fire();
        let mut control_joined = false;
        if shared.shutdown.is_fired() {
            self.connection.graceful_shutdown();
            let drain = async {
                while stream_tasks.join_next().await.is_some() {}
                if let Some(task) = control_task.as_mut() {
                    let _ = task.await;
                    control_joined = true;
                }
            };
            tokio::pin!(drain);
            // The connection must still be driven while streams and unregister use it.
            let _ = tokio::time::timeout(shared.grace_period, async {
                tokio::select! {
                    _ = &mut drain => {}
                    _ = std::future::poll_fn(|cx| self.connection.poll_closed(cx)) => {}
                }
            })
            .await;
        }
        stream_tasks.abort_all();
        while stream_tasks.join_next().await.is_some() {}
        if let Some(mut task) = control_task {
            // A completed JoinHandle must not be polled twice.
            if !control_joined {
                task.abort();
                let _ = (&mut task).await;
            }
        }
        result
    }
}

/// Which kind of edge stream a request is, per cloudflared's
/// `determineHTTP2Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamType {
    Http,
    Websocket,
    Tcp,
    Control,
    Configuration,
}

fn classify(headers: &http::HeaderMap) -> StreamType {
    let upgrade = headers
        .get(INTERNAL_UPGRADE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match upgrade {
        CONFIGURATION_UPDATE => return StreamType::Configuration,
        WEBSOCKET_UPGRADE => return StreamType::Websocket,
        _ => {}
    }
    if headers.contains_key(INTERNAL_TCP_SRC_HEADER) {
        return StreamType::Tcp;
    }
    if upgrade == CONTROL_STREAM_UPGRADE {
        return StreamType::Control;
    }
    StreamType::Http
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> http::HeaderMap {
        http::HeaderMap::new()
    }

    #[test]
    fn classifies_http() {
        assert_eq!(classify(&headers()), StreamType::Http);
    }

    #[test]
    fn classifies_control_stream() {
        let mut headers = headers();
        headers.insert(
            INTERNAL_UPGRADE_HEADER,
            CONTROL_STREAM_UPGRADE.parse().unwrap(),
        );
        assert_eq!(classify(&headers), StreamType::Control);
    }

    #[test]
    fn classifies_websocket() {
        let mut headers = headers();
        headers.insert(INTERNAL_UPGRADE_HEADER, WEBSOCKET_UPGRADE.parse().unwrap());
        assert_eq!(classify(&headers), StreamType::Websocket);
    }

    #[test]
    fn classifies_tcp() {
        let mut headers = headers();
        headers.insert(INTERNAL_TCP_SRC_HEADER, "127.0.0.1".parse().unwrap());
        assert_eq!(classify(&headers), StreamType::Tcp);
    }

    #[test]
    fn classifies_configuration() {
        let mut headers = headers();
        headers.insert(
            INTERNAL_UPGRADE_HEADER,
            CONFIGURATION_UPDATE.parse().unwrap(),
        );
        assert_eq!(classify(&headers), StreamType::Configuration);
    }
}
