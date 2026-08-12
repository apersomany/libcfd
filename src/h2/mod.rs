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
mod tls;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::net::TcpStream;

use crate::control::{self, RegistrationOptions};
use crate::error::{Error, Result};
use crate::event::Event;
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
    pub reg_opts: Arc<RegistrationOptions>,
    pub config_json: Arc<Vec<u8>>,
    pub shutdown: Arc<Event>,
    pub control_shutdown: Arc<tokio::sync::Notify>,
    /// Fires once registration completes on the control stream.
    pub registered: Event,
    pub grace_period: Duration,
}

/// An HTTP/2 connection to the edge.
pub(crate) struct H2EdgeConnection {
    conn: h2::server::Connection<TlsStream, Bytes>,
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
        let config = tls::tls_client_config(ca_cert_pem)?;
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

        let tcp = TcpStream::connect(peer).await?;
        let local_ip = control::peer_ip_bytes(&tcp.local_addr()?);
        let server_name = rustls_pki_types::ServerName::try_from(EDGE_H2_SNI.to_string())
            .map_err(|e| Error::H2(format!("invalid edge sni: {e}")))?;
        let tls = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| Error::H2(format!("tls handshake failed: {e}")))?;

        let mut builder = h2::server::Builder::new();
        builder.max_concurrent_streams(u32::MAX);
        let conn = builder
            .handshake(tls)
            .await
            .map_err(|e| Error::H2(format!("http2 handshake failed: {e}")))?;
        Ok((
            H2EdgeConnection {
                conn,
                local_ip: local_ip.clone(),
            },
            local_ip,
        ))
    }

    /// Serves the connection until the edge closes it or shutdown fires:
    /// accepts edge streams, runs the registration RPC on the control
    /// stream, and dispatches request streams to the origin handlers.
    ///
    /// On shutdown the control task unregisters and in-flight streams are
    /// drained, both bounded by the grace period.
    pub(crate) async fn serve(mut self, shared: Arc<H2Shared>) -> Result<()> {
        let (reg_tx, mut reg_rx) = tokio::sync::oneshot::channel();
        let mut reg_tx = Some(reg_tx);
        let mut control_task: Option<tokio::task::JoinHandle<Result<()>>> = None;
        let mut reg_done = false;
        let mut stream_tasks: tokio::task::JoinSet<Result<()>> = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                request = self.conn.accept() => {
                    match request {
                        Some(Ok((request, mut respond))) => {
                            if classify(request.headers()) == StreamType::Control {
                                if control_task.is_none() {
                                    let shared = shared.clone();
                                    let reg_tx = reg_tx.take().expect("control stream handled once");
                                    control_task = Some(tokio::task::spawn(async move {
                                        register::handle_control_stream(request, respond, shared, reg_tx).await
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
                            shared.control_shutdown.notify_waiters();
                            stream_tasks.abort_all();
                            return Err(Error::H2(format!("connection error: {e}")));
                        }
                        None => {
                            shared.control_shutdown.notify_waiters();
                            stream_tasks.abort_all();
                            return Ok(());
                        }
                    }
                }
                result = &mut reg_rx, if !reg_done => {
                    reg_done = true;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            shared.control_shutdown.notify_waiters();
                            stream_tasks.abort_all();
                            return Err(e);
                        }
                        Err(_) => {
                            shared.control_shutdown.notify_waiters();
                            stream_tasks.abort_all();
                            return Err(Error::H2(
                                "control stream ended before registration".into(),
                            ));
                        }
                    }
                }
                _ = tokio::time::sleep(control::RPC_TIMEOUT), if !reg_done => {
                    shared.control_shutdown.notify_waiters();
                    stream_tasks.abort_all();
                    return Err(Error::H2("registration timed out".into()));
                }
                _ = shared.shutdown.notified() => {
                    shared.control_shutdown.notify_waiters();
                    break;
                }
            }
        }
        // Graceful shutdown: wait for in-flight request streams to drain
        // (cloudflared waits activeRequestsWG), then for the control task's
        // unregister RPC, both bounded by the grace period.
        let _ = tokio::time::timeout(shared.grace_period, async {
            while stream_tasks.join_next().await.is_some() {}
        })
        .await;
        if let Some(task) = control_task {
            let _ = tokio::time::timeout(shared.grace_period, task).await;
        }
        Ok(())
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
