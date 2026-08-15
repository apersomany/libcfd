//! QUIC connection to the Cloudflare edge, built on quinn with the
//! pure-Rust rustls/ring crypto provider.
//!
//! quinn owns the UDP socket and driver tasks; this module adapts its
//! stream handles to the `futures_io` traits the RPC client and request
//! serving code consume. A `QuicStream` shares its `SendStream`/`RecvStream`
//! halves behind an `Arc` so multiple handles can read and write the same
//! edge stream, mirroring how the quiche backend references streams by id.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_io::{AsyncRead, AsyncWrite};
use quinn::{Connection as QuinnConnection, Endpoint, IdleTimeout, TransportConfig, VarInt};

use crate::edge::roots;
use crate::error::{Error, Result};

use super::{EDGE_ALPN, EDGE_SNI};

const MAXIMUM_DATAGRAM_SIZE: u16 = 1350;
const MAXIMUM_IDLE_TIMEOUT_MS: u32 = 5_000;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
const STREAM_RECEIVE_WINDOW: u64 = 6 * 1024 * 1024;
const CONNECTION_RECEIVE_WINDOW: u64 = 30 * 1024 * 1024;
const MAXIMUM_INCOMING_STREAMS: u64 = 1 << 60;

/// A QUIC connection to the edge.
pub(crate) struct QuicConnection {
    connection: QuinnConnection,
}

impl QuicConnection {
    /// Dials the edge over QUIC and returns once the handshake completes.
    pub(crate) async fn connect(
        peer: SocketAddr,
        ca_cert_pem: Option<&[u8]>,
    ) -> Result<QuicConnection> {
        let local = if peer.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let mut endpoint = Endpoint::client(local.parse().expect("static bind address"))?;
        let mut client_config = client_config(ca_cert_pem)?;
        client_config.transport_config(Arc::new(transport_config()));
        endpoint.set_default_client_config(client_config);
        let connecting = endpoint
            .connect(peer, EDGE_SNI)
            .map_err(|e| Error::quic(format!("connect failed: {e}")))?;
        let connection = connecting
            .await
            .map_err(|e| Error::quic(format!("handshake failed: {e}")))?;
        Ok(QuicConnection { connection })
    }

    /// Opens the control stream (the first client stream, id 0).
    pub(crate) async fn open_control_stream(&self) -> Result<QuicStream> {
        let (send, recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|e| Error::quic(format!("open control stream failed: {e}")))?;
        Ok(QuicStream::new(Some(send), Some(recv)))
    }

    /// Accepts the next data stream opened by the edge, or `None` once the
    /// connection closes.
    pub(crate) async fn accept_stream(&self) -> Result<Option<QuicStream>> {
        tokio::select! {
            result = self.connection.accept_bi() => {
                accept_outcome(result).map(|streams| {
                    streams.map(|(send, recv)| QuicStream::new(Some(send), Some(recv)))
                })
            }
            result = self.connection.accept_uni() => {
                accept_outcome(result).map(|stream| stream.map(|recv| QuicStream::new(None, Some(recv))))
            }
        }
    }

    /// The reason the connection closed, if it has.
    pub(crate) fn close_reason(&self) -> Option<String> {
        self.connection.close_reason().map(|e| e.to_string())
    }

    /// Whether the connection ended with an idle timeout.
    pub(crate) fn timed_out(&self) -> bool {
        matches!(
            self.connection.close_reason(),
            Some(quinn::ConnectionError::TimedOut)
        )
    }

    /// Gracefully closes the connection.
    pub(crate) fn close(&self) {
        self.connection.close(VarInt::from_u32(0), b"");
    }

    /// Frees a stream once its serve task completes. quinn hands each stream
    /// out exactly once, so this is a no-op.
    pub(crate) fn release(&self, _identifier: u64) {}
}

fn accept_outcome<T>(result: std::result::Result<T, quinn::ConnectionError>) -> Result<Option<T>> {
    match result {
        Ok(stream) => Ok(Some(stream)),
        Err(e) if is_connection_close(&e) => Ok(None),
        Err(e) => Err(Error::quic(format!("accept failed: {e}"))),
    }
}

fn is_connection_close(error: &quinn::ConnectionError) -> bool {
    matches!(
        error,
        quinn::ConnectionError::LocallyClosed
            | quinn::ConnectionError::ApplicationClosed(_)
            | quinn::ConnectionError::ConnectionClosed(_)
            | quinn::ConnectionError::Reset
            | quinn::ConnectionError::TimedOut
    )
}

/// Transport parameters mirroring the quiche configuration cloudflared uses.
fn transport_config() -> TransportConfig {
    let mut config = TransportConfig::default();
    config.max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(
        MAXIMUM_IDLE_TIMEOUT_MS,
    ))));
    config.keep_alive_interval(Some(KEEPALIVE_INTERVAL));
    config.initial_mtu(MAXIMUM_DATAGRAM_SIZE);
    config
        .receive_window(VarInt::from_u64(CONNECTION_RECEIVE_WINDOW).expect("receive window fits"));
    config.stream_receive_window(
        VarInt::from_u64(STREAM_RECEIVE_WINDOW).expect("stream window fits"),
    );
    config.max_concurrent_bidi_streams(
        VarInt::from_u64(MAXIMUM_INCOMING_STREAMS).expect("stream count fits"),
    );
    config.max_concurrent_uni_streams(
        VarInt::from_u64(MAXIMUM_INCOMING_STREAMS).expect("stream count fits"),
    );
    config
}

/// Builds a quinn client config trusting the system store plus the
/// Cloudflare origin roots, with an optional user CA appended.
fn client_config(ca_cert_pem: Option<&[u8]>) -> Result<quinn::ClientConfig> {
    let mut store = rustls::RootCertStore::empty();
    for pem in roots::root_pems(ca_cert_pem) {
        for cert in rustls_pki_types::pem::PemObject::pem_slice_iter(&pem).flatten() {
            let _ = store.add(cert);
        }
    }
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    tls.alpn_protocols = vec![EDGE_ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|e| Error::quic(format!("quic tls configuration failed: {e}")))?;
    let config = quinn::ClientConfig::new(Arc::new(crypto));
    Ok(config)
}

/// A duplex handle to a single QUIC stream, shareable across tasks.
#[derive(Clone)]
pub(crate) struct QuicStream {
    parts: Arc<Mutex<StreamParts>>,
}

struct StreamParts {
    send: Option<quinn::SendStream>,
    recv: Option<quinn::RecvStream>,
}

impl QuicStream {
    fn new(send: Option<quinn::SendStream>, recv: Option<quinn::RecvStream>) -> Self {
        Self {
            parts: Arc::new(Mutex::new(StreamParts { send, recv })),
        }
    }

    /// The QUIC stream identifier, for diagnostics.
    pub(crate) fn id(&self) -> u64 {
        let parts = self.parts.lock().unwrap();
        parts
            .send
            .as_ref()
            .map(|s| u64::from(s.id()))
            .or_else(|| parts.recv.as_ref().map(|r| u64::from(r.id())))
            .unwrap_or_default()
    }

    /// Sends a FIN for the write side.
    pub(crate) fn finish(&self) {
        let mut parts = self.parts.lock().unwrap();
        if let Some(send) = parts.send.as_mut() {
            let _ = send.finish();
        }
    }

    /// Resets the write side (the edge sees a stream reset instead of EOF).
    pub(crate) fn cancel_write(&self) {
        let mut parts = self.parts.lock().unwrap();
        if let Some(send) = parts.send.as_mut() {
            let _ = send.reset(VarInt::from_u32(0));
        }
    }

    /// Stops reading, releasing the flow-control window for abandoned data.
    pub(crate) fn stop_read(&self) {
        let mut parts = self.parts.lock().unwrap();
        if let Some(recv) = parts.recv.as_mut() {
            let _ = recv.stop(VarInt::from_u32(0));
        }
    }
}

impl AsyncRead for QuicStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        // Take the receive half out so it can be polled without holding the
        // lock, then restore it so other handles can keep reading.
        let mut recv = match self.parts.lock().unwrap().recv.take() {
            Some(recv) => recv,
            None => return Poll::Ready(Ok(0)),
        };
        let result = futures_io::AsyncRead::poll_read(Pin::new(&mut recv), cx, buffer);
        self.parts.lock().unwrap().recv = Some(recv);
        result
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut send = match self.parts.lock().unwrap().send.take() {
            Some(send) => send,
            None => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "write side is closed",
                )));
            }
        };
        let result = futures_io::AsyncWrite::poll_write(Pin::new(&mut send), cx, buffer);
        self.parts.lock().unwrap().send = Some(send);
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut send = match self.parts.lock().unwrap().send.take() {
            Some(send) => send,
            None => return Poll::Ready(Ok(())),
        };
        let result = futures_io::AsyncWrite::poll_flush(Pin::new(&mut send), cx);
        self.parts.lock().unwrap().send = Some(send);
        result
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.finish();
        Poll::Ready(Ok(()))
    }
}
