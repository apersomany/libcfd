//! A quiche server that plays the edge role on a loopback socket.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use libcfd_rpc::quic::{
    ConnectRequest, ConnectResponse, ConnectionType, DATA_STREAM_PROTOCOL_SIGNATURE, PROTOCOL_V1,
    read_connect_response, write_connect_request,
};
use tokio::net::UdpSocket;
use tokio::sync::{Notify, watch};

use crate::error::{Error, Result};
use crate::quic::{Inner, QuicStream, drive};

pub(crate) struct MockEdge {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    seq_tx: watch::Sender<u64>,
}

impl MockEdge {
    /// Binds the loopback socket and spawns the accept+driver task. Returns
    /// the address the client should dial and a handle to the edge once the
    /// handshake is underway.
    pub(crate) async fn start(
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

    pub(crate) async fn wait_established(&self) {
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
    pub(crate) async fn serve_control(&self) -> Result<()> {
        super::serve_control(self.stream(0)).await
    }

    /// Opens a raw stream (websocket/tcp), sends a ConnectRequest, reads the
    /// ConnectResponse, then exchanges a payload with the origin.
    pub(crate) async fn raw_stream_exchange(
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
    pub(crate) async fn request_and_read(&self) -> Result<(ConnectResponse, Vec<u8>)> {
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
