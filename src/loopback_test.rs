//! End-to-end loopback test: a mock edge (quiche server) speaking the
//! registration RPC and data-stream protocols, exercised through the real
//! client path over loopback UDP.

#![cfg(test)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use libcfd_rpc::Incoming;
use libcfd_rpc::quic::{
    ConnectRequest, ConnectResponse, ConnectionType, DATA_STREAM_PROTOCOL_SIGNATURE, PROTOCOL_V1,
    read_connect_response, write_connect_request,
};
use tokio::net::UdpSocket;
use tokio::sync::{Notify, watch};

use crate::control::{self, RegistrationOptions};
use crate::error::{Error, Result};
use crate::origin::{Body, Origin, Request, Response};
use crate::quic::{Inner, QuicConnection, QuicStream, drive};
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
struct MockEdge {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    seq_tx: watch::Sender<u64>,
}

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
