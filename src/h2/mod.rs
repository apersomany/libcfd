//! HTTP/2 edge connection.
//!
//! Mirrors cloudflared's `connection/http2.go`: libcfd dials the edge over
//! TLS (SNI `h2.cftunnel.com`, no ALPN) and acts as the HTTP/2 server; the
//! edge opens streams toward us. The first edge stream carrying
//! `Cf-Cloudflared-Proxy-Connection-Upgrade: control-stream` hosts the
//! registration RPC in its body.

pub(crate) mod headers;
mod stream;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use h2::RecvStream;
use h2::server::SendResponse;
use rustls_pki_types::ServerName;
use tokio::net::TcpStream;

use crate::control::{self, RegistrationOptions};
use crate::error::{Error, Result};
use crate::event::Event;
use crate::origin::{Body, Origin, Request, Response, pump};
use crate::roots;
use crate::tunnel::Tunnel;

pub(crate) use crate::origin::websocket_accept;
pub(crate) use headers::{
    CONFIGURATION_UPDATE, CONTROL_STREAM_UPGRADE, INTERNAL_TCP_SRC_HEADER, INTERNAL_UPGRADE_HEADER,
    WEBSOCKET_UPGRADE, encode_response_headers,
};
pub(crate) use stream::{H2Bidi, RecvStreamReader, SendStreamWriter};

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
}

impl H2EdgeConnection {
    /// Dials the edge and completes the TLS + HTTP/2 handshakes. Returns the
    /// connection and the local socket IP (for `originLocalIp`).
    pub(crate) async fn connect(
        peer: SocketAddr,
        ca_cert_pem: Option<&[u8]>,
    ) -> Result<(H2EdgeConnection, Vec<u8>)> {
        let config = tls_client_config(ca_cert_pem)?;
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

        let tcp = TcpStream::connect(peer).await?;
        let local_ip = match tcp.local_addr()?.ip() {
            std::net::IpAddr::V4(ip) => ip.octets().to_vec(),
            std::net::IpAddr::V6(ip) => ip.octets().to_vec(),
        };
        let server_name = ServerName::try_from(EDGE_H2_SNI.to_string())
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
        Ok((H2EdgeConnection { conn }, local_ip))
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
                                        handle_control_stream(request, respond, shared, reg_tx).await
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
                                    handle_stream(request, respond, shared).await
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

/// Runs the registration RPC on the control-stream request, then blocks
/// until shutdown and unregisters.
async fn handle_control_stream(
    request: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
    reg_tx: tokio::sync::oneshot::Sender<Result<()>>,
) -> Result<()> {
    let send = match respond.send_response(http::Response::new(()), false) {
        Ok(send) => send,
        Err(e) => {
            let error = Error::H2(format!("control stream response failed: {e}"));
            let _ = reg_tx.send(Err(error));
            return Ok(());
        }
    };
    let body = request.into_body();
    let bidi = H2Bidi::new(body, send);
    let result =
        control::register_on_stream(bidi, &shared.tunnel, &shared.reg_opts, &shared.config_json)
            .await;
    let client = match result {
        Ok((_details, client)) => client,
        Err(e) => {
            let _ = reg_tx.send(Err(e));
            return Ok(());
        }
    };
    shared.registered.fire();
    let _ = reg_tx.send(Ok(()));
    shared.control_shutdown.notified().await;
    let _ = control::unregister(client, shared.grace_period).await;
    Ok(())
}

async fn handle_stream(
    request: http::Request<RecvStream>,
    respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    match classify(request.headers()) {
        StreamType::Http => handle_h2_http(request, respond, shared).await,
        StreamType::Websocket => handle_h2_websocket(request, respond, shared).await,
        StreamType::Tcp => handle_h2_tcp(request, respond, shared).await,
        StreamType::Configuration => handle_h2_configuration(request, respond).await,
        StreamType::Control => Ok(()),
    }
}

async fn handle_h2_http(
    request: http::Request<RecvStream>,
    respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers;
    headers.remove(INTERNAL_UPGRADE_HEADER);
    let request = Request::new(
        parts.method,
        parts.uri,
        headers,
        Body::from_reader(RecvStreamReader::new(body)),
    );
    let response = match shared.origin.http.handle_boxed(request).await {
        Ok(response) => response,
        Err(e) => {
            tracing::warn!("origin handler failed: {e}");
            Response::bad_gateway()
        }
    };
    write_h2_response(respond, response).await
}

async fn handle_h2_websocket(
    request: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    let Some(websocket) = &shared.origin.websocket else {
        return write_h2_error(respond, "no websocket origin handler").await;
    };
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers;
    headers.remove(INTERNAL_UPGRADE_HEADER);
    let request = Request::new(parts.method, parts.uri, headers, Body::empty());
    let connection = match websocket.connect_boxed(request).await {
        Ok(connection) => connection,
        Err(e) => return write_h2_error(respond, &format!("{e}")).await,
    };
    let send = write_h2_headers(&mut respond, &connection.response)?;
    pump(
        connection.origin,
        RecvStreamReader::new(body),
        SendStreamWriter::new(send),
    )
    .await
}

async fn handle_h2_tcp(
    request: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    let Some(tcp) = &shared.origin.tcp else {
        return write_h2_error(respond, "no tcp origin handler").await;
    };
    let (parts, body) = request.into_parts();
    let host = request_host(&parts);
    let request = build_tcp_request(&host);
    let duplex = match tcp.connect_boxed(request).await {
        Ok(duplex) => duplex,
        Err(e) => return write_h2_error(respond, &format!("{e}")).await,
    };
    let mut ack_headers = http::HeaderMap::new();
    if let Some(key) = parts.headers.get("sec-websocket-key")
        && let Ok(key) = key.to_str()
    {
        ack_headers.insert("connection", "Upgrade".parse().unwrap());
        ack_headers.insert("upgrade", "websocket".parse().unwrap());
        ack_headers.insert(
            "sec-websocket-accept",
            websocket_accept(key).parse().unwrap(),
        );
    }
    let ack = Response::new(
        http::StatusCode::SWITCHING_PROTOCOLS,
        ack_headers,
        Body::empty(),
    );
    let send = write_h2_headers(&mut respond, &ack)?;
    pump(
        duplex,
        RecvStreamReader::new(body),
        SendStreamWriter::new(send),
    )
    .await
}

async fn handle_h2_configuration(
    request: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
) -> Result<()> {
    let (_, body) = request.into_parts();
    let mut reader = RecvStreamReader::new(body);
    let mut data = Vec::new();
    reader.read_to_end(&mut data).await?;
    let version = serde_json::from_slice::<serde_json::Value>(&data)
        .ok()
        .and_then(|v| v.get("version").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    // libcfd tunnels are locally managed; acknowledge without applying.
    let reply = format!(r#"{{"lastAppliedVersion":{version},"err":null}}"#);
    let send = respond.send_response(http::Response::new(()), false)?;
    let mut writer = SendStreamWriter::new(send);
    writer.write_all(reply.as_bytes()).await?;
    writer.close().await?;
    Ok(())
}

/// Writes an HTTP/2 response and streams the response body.
async fn write_h2_response(mut respond: SendResponse<Bytes>, response: Response) -> Result<()> {
    if response.body.size_hint() == Some(0) {
        let empty = response;
        let mut http_response = http::Response::builder()
            .status(remap_status(empty.status))
            .body(())
            .unwrap();
        *http_response.headers_mut() = encode_response_headers(&empty);
        respond.send_response(http_response, true)?;
        return Ok(());
    }
    let send = write_h2_headers(&mut respond, &response)?;
    let mut writer = SendStreamWriter::new(send);
    let mut body = response.body;
    futures_util::io::copy(&mut body, &mut writer).await?;
    writer.close().await?;
    Ok(())
}

/// Writes the response headers for a streaming exchange (websocket, TCP,
/// control) and returns the write side of the stream.
fn write_h2_headers(
    respond: &mut SendResponse<Bytes>,
    response: &Response,
) -> Result<h2::SendStream<Bytes>> {
    let mut http_response = http::Response::builder()
        .status(remap_status(response.status))
        .body(())
        .unwrap();
    *http_response.headers_mut() = encode_response_headers(response);
    respond
        .send_response(http_response, false)
        .map_err(|e| Error::H2(format!("failed to send response headers: {e}")))
}

async fn write_h2_error(respond: SendResponse<Bytes>, message: &str) -> Result<()> {
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "text/plain".parse().unwrap());
    headers.insert("content-length", message.len().to_string().parse().unwrap());
    let response = Response::new(
        http::StatusCode::BAD_GATEWAY,
        headers,
        Body::from_bytes(message.as_bytes().to_vec()),
    );
    write_h2_response(respond, response).await
}

/// HTTP/2 has no 101; cloudflared remaps it to 200.
fn remap_status(status: http::StatusCode) -> http::StatusCode {
    if status == http::StatusCode::SWITCHING_PROTOCOLS {
        http::StatusCode::OK
    } else {
        status
    }
}

fn request_host(parts: &http::request::Parts) -> String {
    if let Some(host) = parts.headers.get(http::header::HOST) {
        return host.to_str().unwrap_or("").to_string();
    }
    parts.uri.host().unwrap_or("").to_string()
}

fn build_tcp_request(host: &str) -> Request {
    let uri = http::Uri::try_from(format!("http://{host}")).unwrap_or_default();
    Request::new(
        http::Method::GET,
        uri,
        http::HeaderMap::new(),
        Body::empty(),
    )
}

fn tls_client_config(ca_cert_pem: Option<&[u8]>) -> Result<rustls::ClientConfig> {
    let mut store = rustls::RootCertStore::empty();
    for pem in roots::root_pems(ca_cert_pem) {
        for cert in rustls_pki_types::pem::PemObject::pem_slice_iter(&pem).flatten() {
            let _ = store.add(cert);
        }
    }
    Ok(rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth())
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

    #[test]
    fn remaps_switching_protocols() {
        assert_eq!(
            remap_status(http::StatusCode::SWITCHING_PROTOCOLS),
            http::StatusCode::OK
        );
        assert_eq!(remap_status(http::StatusCode::OK), http::StatusCode::OK);
    }
}
