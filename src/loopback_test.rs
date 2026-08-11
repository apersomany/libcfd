//! End-to-end loopback test: a mock edge (quiche server) speaking the
//! registration RPC and data-stream protocols, exercised through the real
//! client path over loopback UDP.

#![cfg(test)]

use std::sync::Arc;
#[cfg(feature = "quic-edge")]
use std::sync::Mutex;
use std::time::Duration;

#[cfg(feature = "quic-edge")]
use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use libcfd_rpc::Incoming;
#[cfg(feature = "quic-edge")]
use libcfd_rpc::quic::{
    ConnectRequest, ConnectResponse, ConnectionType, DATA_STREAM_PROTOCOL_SIGNATURE, PROTOCOL_V1,
    read_connect_response, write_connect_request,
};
#[cfg(feature = "quic-edge")]
use tokio::net::UdpSocket;
use tokio::sync::Notify;
#[cfg(feature = "quic-edge")]
use tokio::sync::watch;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::control::RegistrationOptions;
#[cfg(feature = "quic-edge")]
use crate::control::{self};
#[cfg(feature = "quic-edge")]
use crate::error::Error;
use crate::error::Result;
#[cfg(feature = "h2-edge")]
use crate::event::Event;
use crate::origin::{Body, Origin, Request, Response};
#[cfg(feature = "quic-edge")]
use crate::quic::{Inner, QuicConnection, QuicStream, drive};
#[cfg(feature = "quic-edge")]
use crate::serve;
use crate::tunnel::{QuickTunnel, Tunnel};

// Genuine capnp-go replies, byte-identical to libcfd-rpc's verified goldens.
const BOOTSTRAP_RETURN: &str = "000000000b00000000000000010001000300000000000000000000000200010000000000010000000000000000000000000000000000020003000000000000000100000017000000040000000100010001000000000000000000000000000000";
const REGISTER_RETURN: &str = "0000000012000000000000000100010003000000000000000000000002000100010000000100000000000000000000000000000000000200040000000000010025000000070000000000000001000100010000000000000000000000010002000000000000000000050000008200000009000000220000000102030405060708090a0b0c0d0e0f106c687200000000000000000001000100";
const EMPTY_RETURN: &str = "0000000009000000000000000100010003000000000000000000000002000100020000000100000000000000000000000000000000000200000000000000000001000000070000000000000001000100";

fn hex(encoded: &str) -> Vec<u8> {
    (0..encoded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16).unwrap())
        .collect()
}

fn make_tunnel() -> QuickTunnel {
    QuickTunnel {
        tunnel_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        name: String::new(),
        hostname: "test.trycloudflare.com".into(),
        account_tag: "test-account".into(),
        secret: (1..=16).collect(),
    }
}

/// A quiche server that plays the edge role on a loopback socket.
#[cfg(feature = "quic-edge")]
struct MockEdge {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    seq_tx: watch::Sender<u64>,
}

#[cfg(feature = "quic-edge")]
impl MockEdge {
    /// Binds the loopback socket and spawns the accept+driver task. Returns
    /// the address the client should dial and a handle to the edge once the
    /// handshake is underway.
    async fn start(
        certified: &rcgen::CertifiedKey<rcgen::KeyPair>,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<MockEdge>>,
    ) {
        let cert = boring::x509::X509::from_der(certified.cert.der().as_ref()).expect("cert parse");
        let key = boring::pkey::PKey::private_key_from_der(&certified.signing_key.serialize_der())
            .expect("key parse");
        let mut builder =
            boring::ssl::SslContextBuilder::new(boring::ssl::SslMethod::tls_server()).expect("ctx");
        builder.set_certificate(&cert).expect("set cert");
        builder.set_private_key(&key).expect("set key");
        let mut config =
            quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, builder)
                .expect("quiche config");
        config
            .set_application_protos(&[b"argotunnel"])
            .expect("alpn");
        config.set_max_idle_timeout(5_000);
        config.set_max_recv_udp_payload_size(1350);
        config.set_max_send_udp_payload_size(1350);
        config.set_initial_max_data(30 << 20);
        config.set_initial_max_stream_data_bidi_local(6 << 20);
        config.set_initial_max_stream_data_bidi_remote(6 << 20);
        config.set_initial_max_stream_data_uni(6 << 20);
        config.set_initial_max_streams_bidi(1 << 60);

        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let local = socket.local_addr().expect("local addr");

        let handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            let (len, from) = socket.recv_from(&mut buf).await?;
            let mut scid = [0u8; quiche::MAX_CONN_ID_LEN];
            boring::rand::rand_bytes(&mut scid)?;
            let scid = quiche::ConnectionId::from_ref(&scid);
            let mut conn = quiche::accept(&scid, None, local, from, &mut config)?;
            let recv_info = quiche::RecvInfo { to: local, from };
            conn.recv(&mut buf[..len], recv_info)?;
            let inner = Arc::new(Mutex::new(Inner {
                conn,
                read_wakers: Default::default(),
                write_wakers: Default::default(),
                established: false,
                closed: false,
                timed_out: false,
                close_reason: None,
            }));
            let notify = Arc::new(Notify::new());
            let (seq_tx, _) = watch::channel(0u64);
            tokio::spawn(drive(socket, inner.clone(), notify.clone(), seq_tx.clone()));
            Ok(MockEdge {
                inner,
                notify,
                seq_tx,
            })
        });
        (local, handle)
    }

    async fn wait_established(&self) {
        let mut rx = self.seq_tx.subscribe();
        loop {
            if self.inner.lock().unwrap().conn.is_established() {
                return;
            }
            if self.inner.lock().unwrap().closed {
                panic!("mock edge connection closed during handshake");
            }
            let _ = rx.changed().await;
        }
    }

    fn stream(&self, id: u64) -> QuicStream {
        QuicStream::new(self.inner.clone(), self.notify.clone(), id)
    }

    /// Serves the registration RPC on the control stream (id 0) until the
    /// configuration push completes.
    async fn serve_control(&self) -> Result<()> {
        let mut stream = self.stream(0);
        loop {
            let incoming = match libcfd_rpc::read_incoming(&mut stream).await? {
                Some(m) => m,
                None => return Ok(()),
            };
            match incoming {
                Incoming::Bootstrap { .. } => {
                    libcfd_rpc::io::write_raw(&mut stream, &hex(BOOTSTRAP_RETURN)).await?;
                }
                Incoming::Call { method_id: 0, .. } => {
                    libcfd_rpc::io::write_raw(&mut stream, &hex(REGISTER_RETURN)).await?;
                }
                Incoming::Call { method_id: 2, .. } => {
                    libcfd_rpc::io::write_raw(&mut stream, &hex(EMPTY_RETURN)).await?;
                    return Ok(());
                }
                Incoming::Call { method_id: 1, .. } => {
                    return Ok(());
                }
                Incoming::Finish { .. } => {}
                Incoming::Release => return Ok(()),
                other => {
                    return Err(Error::Quic(format!(
                        "mock edge: unexpected control message {other:?}"
                    )));
                }
            }
        }
    }

    /// Opens a raw stream (websocket/tcp), sends a ConnectRequest, reads the
    /// ConnectResponse, then exchanges a payload with the origin.
    async fn raw_stream_exchange(
        &self,
        stream_id: u64,
        connect: ConnectRequest,
        payload: &[u8],
    ) -> Result<(ConnectResponse, Vec<u8>)> {
        let mut stream = self.stream(stream_id);
        stream.write_all(&DATA_STREAM_PROTOCOL_SIGNATURE).await?;
        stream.write_all(PROTOCOL_V1).await?;
        write_connect_request(&mut stream, &connect).await?;

        let mut header = [0u8; 8];
        stream.read_exact(&mut header).await?;
        assert_eq!(&header[..6], &DATA_STREAM_PROTOCOL_SIGNATURE);
        assert_eq!(&header[6..], PROTOCOL_V1);
        let response = read_connect_response(&mut stream).await?;

        stream.write_all(payload).await?;
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
            .await
            .map_err(|_| Error::Quic("raw stream exchange timed out".into()))??;
        stream.finish();
        Ok((response, buf[..n].to_vec()))
    }

    /// Opens a request stream (server-initiated id 1), sends an HTTP request,
    /// and returns the decoded response metadata and body.
    async fn request_and_read(&self) -> Result<(ConnectResponse, Vec<u8>)> {
        let mut stream = self.stream(1);
        stream.write_all(&DATA_STREAM_PROTOCOL_SIGNATURE).await?;
        stream.write_all(PROTOCOL_V1).await?;
        let request = ConnectRequest {
            dest: "http://example.com/hello".into(),
            conn_type: ConnectionType::Http,
            metadata: vec![
                ("HttpMethod".into(), "GET".into()),
                ("HttpHost".into(), "example.com".into()),
                ("HttpHeader:user-agent".into(), "mock-edge".into()),
            ],
        };
        write_connect_request(&mut stream, &request).await?;
        stream.write_all(b"ping").await?;
        stream.finish();

        let mut header = [0u8; 8];
        stream.read_exact(&mut header).await?;
        assert_eq!(&header[..6], &DATA_STREAM_PROTOCOL_SIGNATURE);
        assert_eq!(&header[6..], PROTOCOL_V1);
        let response = read_connect_response(&mut stream).await?;
        let mut body = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut body))
            .await
            .map_err(|_| Error::Quic("response body timed out".into()))??;
        Ok((response, body))
    }
}

#[cfg(feature = "quic-edge")]
#[tokio::test(flavor = "multi_thread")]
async fn quic_tunnel_end_to_end() {
    let certified = rcgen::generate_simple_self_signed(vec![
        "quic.cftunnel.com".to_string(),
        "localhost".to_string(),
    ])
    .expect("cert");
    let ca_pem = certified.cert.pem().into_bytes();
    let (edge_addr, edge_task) = MockEdge::start(&certified).await;

    let conn = tokio::time::timeout(
        Duration::from_secs(15),
        QuicConnection::connect(edge_addr, Some(&ca_pem)),
    )
    .await
    .expect("client handshake timeout")
    .expect("client should complete the quic handshake");
    let edge = edge_task.await.expect("mock edge task").expect("mock edge");
    edge.wait_established().await;

    let edge_control = Arc::new(edge);
    let control_task = tokio::spawn({
        let e = edge_control.clone();
        async move { e.serve_control().await }
    });
    let tunnel = Tunnel::quick(make_tunnel());
    let opts = RegistrationOptions::default();
    let (_details, _client) = tokio::time::timeout(
        Duration::from_secs(15),
        control::register(&conn, &tunnel, &opts, br#"{"ingress":[]}"#),
    )
    .await
    .expect("registration timeout")
    .expect("client should register with the mock edge");

    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
    let origin = Origin::http(move |mut request: Request| {
        let seen_tx = seen_tx.clone();
        async move {
            let mut body = request.body.collect().await.expect("body read");
            let mut headers = http::HeaderMap::new();
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("text/plain"),
            );
            let _ = seen_tx.send((
                request.method.as_str().to_string(),
                request.uri.to_string(),
                std::mem::take(&mut body),
            ));
            Ok(Response::new(
                http::StatusCode::OK,
                headers,
                Body::from_bytes(b"pong".to_vec()),
            ))
        }
    });

    let conn = Arc::new(conn);
    let serve_task = tokio::spawn(serve::serve_requests(conn.clone(), Arc::new(origin)));

    let (response, body) = edge_control
        .request_and_read()
        .await
        .expect("mock edge should get a response");
    assert_eq!(body, b"pong");
    let status = response
        .metadata
        .iter()
        .find(|(k, _)| k == "HttpStatus")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    assert_eq!(status, "200");

    let (method, uri, request_body) = seen_rx
        .recv()
        .await
        .expect("origin handler should observe the request");
    assert_eq!(method, "GET");
    assert_eq!(uri, "http://example.com/hello");
    assert_eq!(request_body, b"ping");

    serve_task.abort();
    control_task.abort();
    conn.close();
}

/// A websocket origin that answers with 101 and echoes the raw stream
/// through a loopback TCP connection to `addr`.
#[cfg(feature = "quic-edge")]
struct EchoWsOrigin {
    addr: std::net::SocketAddr,
}

#[cfg(feature = "quic-edge")]
impl crate::origin::WebSocketOrigin for EchoWsOrigin {
    async fn connect(&self, request: Request) -> Result<crate::origin::WebSocketConnection> {
        let sock = tokio::net::TcpStream::connect(self.addr).await?;
        let (r, w) = sock.into_split();
        let mut headers = http::HeaderMap::new();
        let key = request
            .headers
            .get("sec-websocket-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        headers.insert(
            "Sec-WebSocket-Accept",
            http::HeaderValue::from_str(&crate::origin::websocket_accept(key)).unwrap(),
        );
        Ok(crate::origin::WebSocketConnection {
            response: Response::new(
                http::StatusCode::SWITCHING_PROTOCOLS,
                headers,
                Body::empty(),
            ),
            origin: crate::origin::Duplex::new(r.compat(), w.compat_write()),
        })
    }
}

/// A TCP origin that echoes the raw stream through a loopback connection.
#[cfg(feature = "quic-edge")]
struct EchoTcpOrigin {
    addr: std::net::SocketAddr,
}

#[cfg(feature = "quic-edge")]
impl crate::origin::TcpOrigin for EchoTcpOrigin {
    async fn connect(&self, _request: Request) -> Result<crate::origin::Duplex> {
        let sock = tokio::net::TcpStream::connect(self.addr).await?;
        let (r, w) = sock.into_split();
        Ok(crate::origin::Duplex::new(r.compat(), w.compat_write()))
    }
}

#[cfg(feature = "quic-edge")]
async fn echo_server() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

#[cfg(feature = "quic-edge")]
#[tokio::test(flavor = "multi_thread")]
async fn quic_websocket_tcp_round_trip() {
    let certified = rcgen::generate_simple_self_signed(vec![
        "quic.cftunnel.com".to_string(),
        "localhost".to_string(),
    ])
    .expect("cert");
    let ca_pem = certified.cert.pem().into_bytes();
    let (edge_addr, edge_task) = MockEdge::start(&certified).await;
    let echo_addr = echo_server().await;

    let conn = tokio::time::timeout(
        Duration::from_secs(15),
        QuicConnection::connect(edge_addr, Some(&ca_pem)),
    )
    .await
    .expect("client handshake timeout")
    .expect("client should complete the quic handshake");
    let edge = edge_task.await.expect("mock edge task").expect("mock edge");
    edge.wait_established().await;

    let edge_control = Arc::new(edge);
    let control_task = tokio::spawn({
        let e = edge_control.clone();
        async move { e.serve_control().await }
    });
    let tunnel = Tunnel::quick(make_tunnel());
    let opts = RegistrationOptions::default();
    let (_details, _client) = tokio::time::timeout(
        Duration::from_secs(15),
        control::register(&conn, &tunnel, &opts, br#"{"ingress":[]}"#),
    )
    .await
    .expect("registration timeout")
    .expect("client should register with the mock edge");

    let origin = Origin::http(|_request: Request| async move {
        Ok(Response::new(
            http::StatusCode::NOT_FOUND,
            http::HeaderMap::new(),
            Body::empty(),
        ))
    })
    .with_websocket(EchoWsOrigin { addr: echo_addr })
    .with_tcp(EchoTcpOrigin { addr: echo_addr });

    let conn = Arc::new(conn);
    let serve_task = tokio::spawn(serve::serve_requests(conn.clone(), Arc::new(origin)));

    // Websocket stream (server-initiated id 5).
    let (ws_response, ws_echo) = edge_control
        .raw_stream_exchange(
            5,
            ConnectRequest {
                dest: "http://example.com/ws".into(),
                conn_type: ConnectionType::Websocket,
                metadata: vec![
                    ("HttpMethod".into(), "GET".into()),
                    ("HttpHost".into(), "example.com".into()),
                    (
                        "HttpHeader:sec-websocket-key".into(),
                        "dGhlIHNhbXBsZSBub25jZQ==".into(),
                    ),
                ],
            },
            b"ping-ws",
        )
        .await
        .expect("mock edge should get a websocket response");
    assert_eq!(ws_echo, b"ping-ws");
    let ws_status = ws_response
        .metadata
        .iter()
        .find(|(k, _)| k == "HttpStatus")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    assert_eq!(ws_status, "101");
    assert!(ws_response.metadata.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("HttpHeader:Sec-WebSocket-Accept")
            && v == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    }));

    // TCP stream (server-initiated id 5).
    let (tcp_response, tcp_echo) = edge_control
        .raw_stream_exchange(
            9,
            ConnectRequest {
                dest: "10.0.0.1:8080".into(),
                conn_type: ConnectionType::Tcp,
                metadata: vec![],
            },
            b"ping-tcp",
        )
        .await
        .expect("mock edge should get a tcp response");
    assert_eq!(tcp_echo, b"ping-tcp");
    assert!(tcp_response.error.is_empty());
    assert!(!tcp_response.metadata.iter().any(|(k, _)| k == "HttpStatus"));

    serve_task.abort();
    control_task.abort();
    conn.close();
}

#[cfg(feature = "h2-edge")]
#[tokio::test(flavor = "multi_thread")]
async fn h2_tunnel_end_to_end() {
    use bytes::Bytes;

    let certified = rcgen::generate_simple_self_signed(vec![
        "h2.cftunnel.com".to_string(),
        "localhost".to_string(),
    ])
    .expect("cert");
    let ca_pem = certified.cert.pem().into_bytes();
    let cert = rustls_pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key =
        rustls_pki_types::PrivateKeyDer::try_from(certified.signing_key.serialize_der().to_vec())
            .expect("key der");
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("server config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let edge_addr = listener.local_addr().unwrap();

    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();

    let edge_task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let tls = acceptor.accept(tcp).await.expect("tls accept");
        let (mut client, conn) = h2::client::handshake(tls)
            .await
            .expect("h2 client handshake");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        client = client.ready().await.expect("client ready");

        let control_request = http::Request::builder()
            .method("POST")
            .uri("https://example.com/")
            .header("Cf-Cloudflared-Proxy-Connection-Upgrade", "control-stream")
            .body(())
            .unwrap();
        let (response_future, send) = client
            .send_request(control_request, false)
            .expect("send control request");
        let response = response_future.await.expect("control response");
        assert_eq!(response.status(), http::StatusCode::OK);
        let (_, recv) = response.into_parts();
        let mut bidi = crate::h2::H2Bidi::new(recv, send);
        loop {
            match libcfd_rpc::read_incoming(&mut bidi)
                .await
                .expect("read incoming")
            {
                Some(Incoming::Bootstrap { .. }) => {
                    libcfd_rpc::io::write_raw(&mut bidi, &hex(BOOTSTRAP_RETURN))
                        .await
                        .unwrap();
                }
                Some(Incoming::Call { method_id: 0, .. }) => {
                    libcfd_rpc::io::write_raw(&mut bidi, &hex(REGISTER_RETURN))
                        .await
                        .unwrap();
                }
                Some(Incoming::Call { method_id: 2, .. }) => {
                    libcfd_rpc::io::write_raw(&mut bidi, &hex(EMPTY_RETURN))
                        .await
                        .unwrap();
                    break;
                }
                Some(Incoming::Call { method_id: 1, .. }) => {
                    libcfd_rpc::io::write_raw(&mut bidi, &hex(EMPTY_RETURN))
                        .await
                        .unwrap();
                    break;
                }
                Some(_) => {}
                None => panic!("control stream ended during registration"),
            }
        }
        drop(bidi);

        let request = http::Request::builder()
            .method("GET")
            .uri("http://example.com/hello")
            .header("host", "example.com")
            .body(())
            .unwrap();
        let (response_future, mut send) = client
            .send_request(request, false)
            .expect("send data request");
        send.send_data(Bytes::from_static(b"ping"), true).unwrap();
        let response = response_future.await.expect("data response");
        let status = response.status();
        let headers = response.headers().clone();
        let (_, mut body) = response.into_parts();
        let mut resp_body = Vec::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk.expect("body chunk");
            resp_body.extend_from_slice(&chunk);
        }

        // Configuration update stream: acknowledge the version without
        // applying the config (locally managed).
        let config_request = http::Request::builder()
            .method("POST")
            .uri("https://example.com/")
            .header(
                "Cf-Cloudflared-Proxy-Connection-Upgrade",
                "update-configuration",
            )
            .header("content-type", "application/json")
            .body(())
            .unwrap();
        let (response_future, mut send) = client
            .send_request(config_request, false)
            .expect("send config request");
        send.send_data(Bytes::from_static(br#"{"version":7,"config":{}}"#), true)
            .unwrap();
        let response = response_future.await.expect("config response");
        let config_status = response.status();
        let (_, mut body) = response.into_parts();
        let mut config_body = Vec::new();
        while let Some(chunk) = body.data().await {
            config_body.extend_from_slice(&chunk.expect("body chunk"));
        }
        (status, headers, resp_body, config_status, config_body)
    });

    let (conn, _local_ip) = crate::h2::H2EdgeConnection::connect(edge_addr, Some(&ca_pem))
        .await
        .expect("h2 edge connect");
    let shutdown = Arc::new(Event::new());
    let tunnel = Arc::new(Tunnel::quick(make_tunnel()));
    let origin = Arc::new(Origin::http(move |mut request: Request| {
        let seen_tx = seen_tx.clone();
        async move {
            let body = request.body.collect().await.expect("body read");
            let _ = seen_tx.send((
                request.method.as_str().to_string(),
                request.uri.to_string(),
                body,
            ));
            Ok(Response::new(
                http::StatusCode::OK,
                http::HeaderMap::new(),
                Body::from_bytes(b"pong".to_vec()),
            ))
        }
    }));
    let shared = Arc::new(crate::h2::H2Shared {
        tunnel: tunnel.clone(),
        origin: origin.clone(),
        reg_opts: Arc::new(RegistrationOptions::default()),
        config_json: Arc::new(br#"{"ingress":[]}"#.to_vec()),
        shutdown: shutdown.clone(),
        control_shutdown: Arc::new(Notify::new()),
        registered: Event::new(),
        grace_period: Duration::from_secs(30),
    });
    let serve_task = tokio::spawn(conn.serve(shared));

    let (status, headers, resp_body, config_status, config_body) =
        edge_task.await.expect("edge task");
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(resp_body, b"pong");
    assert!(headers.contains_key("cf-cloudflared-response-meta"));
    assert_eq!(config_status, http::StatusCode::OK);
    assert_eq!(
        String::from_utf8(config_body).unwrap(),
        r#"{"lastAppliedVersion":7,"err":null}"#
    );

    let (method, uri, request_body) = seen_rx
        .recv()
        .await
        .expect("origin handler should observe the request");
    assert_eq!(method, "GET");
    assert_eq!(uri, "http://example.com/hello");
    assert_eq!(request_body, b"ping");

    shutdown.fire();
    serve_task.abort();
}

/// A duplex that echoes everything written to it back to its reader, then
/// closes once the write half is closed. Self-terminating so the response
/// side of a websocket/TCP exchange can end.
#[cfg(feature = "h2-edge")]
fn duplex_echo() -> crate::origin::Duplex {
    let (lib_read, mut echo_write) = tokio::io::duplex(16384);
    let (mut echo_read, lib_write) = tokio::io::duplex(16384);
    tokio::spawn(async move {
        let _ = tokio::io::copy(&mut echo_read, &mut echo_write).await;
    });
    crate::origin::Duplex::new(lib_read.compat(), lib_write.compat_write())
}

#[cfg(feature = "h2-edge")]
struct OneShotWsOrigin;

#[cfg(feature = "h2-edge")]
impl crate::origin::WebSocketOrigin for OneShotWsOrigin {
    async fn connect(&self, request: Request) -> Result<crate::origin::WebSocketConnection> {
        let mut headers = http::HeaderMap::new();
        let key = request
            .headers
            .get("sec-websocket-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        headers.insert(
            "Sec-WebSocket-Accept",
            http::HeaderValue::from_str(&crate::origin::websocket_accept(key)).unwrap(),
        );
        Ok(crate::origin::WebSocketConnection {
            response: Response::new(
                http::StatusCode::SWITCHING_PROTOCOLS,
                headers,
                Body::empty(),
            ),
            origin: duplex_echo(),
        })
    }
}

#[cfg(feature = "h2-edge")]
struct OneShotTcpOrigin;

#[cfg(feature = "h2-edge")]
impl crate::origin::TcpOrigin for OneShotTcpOrigin {
    async fn connect(&self, _request: Request) -> Result<crate::origin::Duplex> {
        Ok(duplex_echo())
    }
}

#[cfg(feature = "h2-edge")]
#[tokio::test(flavor = "multi_thread")]
async fn h2_websocket_tcp_round_trip() {
    use bytes::Bytes;

    let certified = rcgen::generate_simple_self_signed(vec![
        "h2.cftunnel.com".to_string(),
        "localhost".to_string(),
    ])
    .expect("cert");
    let ca_pem = certified.cert.pem().into_bytes();
    let cert = rustls_pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key =
        rustls_pki_types::PrivateKeyDer::try_from(certified.signing_key.serialize_der().to_vec())
            .expect("key der");
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("server config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let edge_addr = listener.local_addr().unwrap();

    let edge_task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let tls = acceptor.accept(tcp).await.expect("tls accept");
        let (mut client, conn) = h2::client::handshake(tls)
            .await
            .expect("h2 client handshake");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        client = client.ready().await.expect("client ready");

        let control_request = http::Request::builder()
            .method("POST")
            .uri("https://example.com/")
            .header("Cf-Cloudflared-Proxy-Connection-Upgrade", "control-stream")
            .body(())
            .unwrap();
        let (response_future, send) = client
            .send_request(control_request, false)
            .expect("send control request");
        let response = response_future.await.expect("control response");
        assert_eq!(response.status(), http::StatusCode::OK);
        let (_, recv) = response.into_parts();
        let mut bidi = crate::h2::H2Bidi::new(recv, send);
        loop {
            match libcfd_rpc::read_incoming(&mut bidi)
                .await
                .expect("read incoming")
            {
                Some(Incoming::Bootstrap { .. }) => {
                    libcfd_rpc::io::write_raw(&mut bidi, &hex(BOOTSTRAP_RETURN))
                        .await
                        .unwrap();
                }
                Some(Incoming::Call { method_id: 0, .. }) => {
                    libcfd_rpc::io::write_raw(&mut bidi, &hex(REGISTER_RETURN))
                        .await
                        .unwrap();
                }
                Some(Incoming::Call { method_id: 2, .. }) => {
                    libcfd_rpc::io::write_raw(&mut bidi, &hex(EMPTY_RETURN))
                        .await
                        .unwrap();
                    break;
                }
                Some(Incoming::Call { method_id: 1, .. }) => {
                    libcfd_rpc::io::write_raw(&mut bidi, &hex(EMPTY_RETURN))
                        .await
                        .unwrap();
                    break;
                }
                Some(_) => {}
                None => panic!("control stream ended during registration"),
            }
        }
        drop(bidi);

        // Websocket upgrade stream: 101 is remapped to 200 and the origin's
        // Sec-WebSocket-Accept travels in the serialized user headers. The
        // request body is half-closed after the payload; the response side
        // stays open until the origin closes.
        let ws_request = http::Request::builder()
            .method("GET")
            .uri("http://example.com/ws")
            .header("host", "example.com")
            .header("Cf-Cloudflared-Proxy-Connection-Upgrade", "websocket")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap();
        let (response_future, mut send) = client
            .send_request(ws_request, false)
            .expect("send websocket request");
        send.send_data(Bytes::from_static(b"ping-ws"), false)
            .unwrap();
        send.send_data(Bytes::new(), true).unwrap();
        let response = response_future.await.expect("websocket response");
        let ws_status = response.status();
        let ws_headers = response.headers().clone();
        let (_, mut body) = response.into_parts();
        let mut ws_body = Vec::new();
        while let Some(chunk) = body.data().await {
            ws_body.extend_from_slice(&chunk.expect("body chunk"));
        }

        // Raw TCP proxy stream: the ack is a bare 101 (remapped to 200).
        let tcp_request = http::Request::builder()
            .method("GET")
            .uri("http://10.0.0.1:8080/")
            .header("host", "10.0.0.1:8080")
            .header("Cf-Cloudflared-Proxy-Src", "127.0.0.1")
            .body(())
            .unwrap();
        let (response_future, mut send) = client
            .send_request(tcp_request, false)
            .expect("send tcp request");
        send.send_data(Bytes::from_static(b"ping-tcp"), false)
            .unwrap();
        send.send_data(Bytes::new(), true).unwrap();
        let response = response_future.await.expect("tcp response");
        let tcp_status = response.status();
        let (_, mut body) = response.into_parts();
        let mut tcp_body = Vec::new();
        while let Some(chunk) = body.data().await {
            tcp_body.extend_from_slice(&chunk.expect("body chunk"));
        }

        (ws_status, ws_headers, ws_body, tcp_status, tcp_body)
    });

    let (conn, _local_ip) = crate::h2::H2EdgeConnection::connect(edge_addr, Some(&ca_pem))
        .await
        .expect("h2 edge connect");
    let shutdown = Arc::new(Event::new());
    let tunnel = Arc::new(Tunnel::quick(make_tunnel()));
    let origin = Arc::new(
        Origin::http(|_request: Request| async move {
            Ok(Response::new(
                http::StatusCode::NOT_FOUND,
                http::HeaderMap::new(),
                Body::empty(),
            ))
        })
        .with_websocket(OneShotWsOrigin)
        .with_tcp(OneShotTcpOrigin),
    );
    let shared = Arc::new(crate::h2::H2Shared {
        tunnel: tunnel.clone(),
        origin: origin.clone(),
        reg_opts: Arc::new(RegistrationOptions::default()),
        config_json: Arc::new(br#"{"ingress":[]}"#.to_vec()),
        shutdown: shutdown.clone(),
        control_shutdown: Arc::new(Notify::new()),
        registered: Event::new(),
        grace_period: Duration::from_secs(30),
    });
    let serve_task = tokio::spawn(conn.serve(shared));

    let (ws_status, ws_headers, ws_body, tcp_status, tcp_body) =
        edge_task.await.expect("edge task");
    assert_eq!(ws_status, http::StatusCode::OK);
    assert_eq!(ws_body, b"ping-ws");
    let serialized = ws_headers
        .get("cf-cloudflared-response-headers")
        .expect("websocket accept in serialized user headers")
        .to_str()
        .unwrap();
    let user = crate::h2::headers::deserialize_headers(serialized);
    assert!(
        user.iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("Sec-WebSocket-Accept")
                && v == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
    );
    assert_eq!(tcp_status, http::StatusCode::OK);
    assert_eq!(tcp_body, b"ping-tcp");

    shutdown.fire();
    serve_task.abort();
}
