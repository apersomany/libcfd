//! QUIC connection to the Cloudflare edge, built on quiche.

mod stream;
mod tls;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{Notify, watch};

use crate::error::{Error, Result};

use super::{EDGE_ALPN, EDGE_SNI};

pub(crate) use stream::QuicStream;

const MAXIMUM_DATAGRAM_SIZE: usize = 1350;
const MAXIMUM_IDLE_TIMEOUT_MS: u64 = 5_000;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
const STREAM_RECEIVE_WINDOW: u64 = 6 * 1024 * 1024;
const CONNECTION_RECEIVE_WINDOW: u64 = 30 * 1024 * 1024;
const MAXIMUM_INCOMING_STREAMS: u64 = 1 << 60;
pub(crate) struct Inner {
    pub(crate) connection: quiche::Connection,
    pub(crate) read_wakers: HashMap<u64, Waker>,
    pub(crate) write_wakers: HashMap<u64, Waker>,
    /// Streams already handed to the serve loop; the control stream (id 0)
    /// is served by the RPC client, not the request path.
    pub(crate) accepted: HashSet<u64>,
    pub(crate) established: bool,
    pub(crate) closed: bool,
    pub(crate) timed_out: bool,
    pub(crate) close_reason: Option<String>,
}

/// A QUIC connection to the edge.
pub(crate) struct QuicConnection {
    pub(crate) inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    sequence_tx: watch::Sender<u64>,
    driver: crate::edge::OwnedTask<()>,
}

impl Drop for QuicConnection {
    fn drop(&mut self) {
        self.close();
    }
}

impl QuicConnection {
    /// Dials the edge over QUIC and returns once the handshake completes.
    pub(crate) async fn connect(
        peer: SocketAddr,
        ca_cert_pem: Option<&[u8]>,
    ) -> Result<QuicConnection> {
        let socket = match peer {
            SocketAddr::V4(_) => UdpSocket::bind("0.0.0.0:0").await?,
            SocketAddr::V6(_) => UdpSocket::bind("[::]:0").await?,
        };
        socket.connect(peer).await?;
        let local = socket.local_addr()?;

        let mut configuration = tls::client_config(ca_cert_pem)?;
        configuration.set_application_protos(&[EDGE_ALPN])?;
        configuration.set_max_idle_timeout(MAXIMUM_IDLE_TIMEOUT_MS);
        configuration.set_max_recv_udp_payload_size(MAXIMUM_DATAGRAM_SIZE);
        configuration.set_max_send_udp_payload_size(MAXIMUM_DATAGRAM_SIZE);
        configuration.set_initial_max_data(CONNECTION_RECEIVE_WINDOW);
        configuration.set_initial_max_stream_data_bidi_local(STREAM_RECEIVE_WINDOW);
        configuration.set_initial_max_stream_data_bidi_remote(STREAM_RECEIVE_WINDOW);
        configuration.set_initial_max_stream_data_uni(STREAM_RECEIVE_WINDOW);
        configuration.set_initial_max_streams_bidi(MAXIMUM_INCOMING_STREAMS);
        configuration.set_initial_max_streams_uni(MAXIMUM_INCOMING_STREAMS);
        configuration.set_disable_active_migration(true);

        let mut scid = [0u8; quiche::MAX_CONN_ID_LEN];
        boring::rand::rand_bytes(&mut scid)?;
        let scid = quiche::ConnectionId::from_ref(&scid);

        let connection = quiche::connect(Some(EDGE_SNI), &scid, local, peer, &mut configuration)?;

        let inner = Arc::new(Mutex::new(Inner {
            connection,
            read_wakers: HashMap::new(),
            write_wakers: HashMap::new(),
            accepted: HashSet::from([0]),
            established: false,
            closed: false,
            timed_out: false,
            close_reason: None,
        }));
        let notify = Arc::new(Notify::new());
        let (sequence_tx, _) = watch::channel(0u64);

        let driver = crate::edge::OwnedTask::spawn(drive(
            socket,
            inner.clone(),
            notify.clone(),
            sequence_tx.clone(),
        ));

        let connection = QuicConnection {
            inner,
            notify,
            sequence_tx,
            driver,
        };
        connection.wait_established().await?;
        Ok(connection)
    }

    async fn wait_established(&self) -> Result<()> {
        let mut rx = self.sequence_tx.subscribe();
        loop {
            let state = {
                let g = self.inner.lock().unwrap();
                (g.established, g.closed, g.close_reason.clone())
            };
            if state.1 {
                return Err(Error::quic(format!(
                    "connection closed during handshake: {}",
                    state.2.unwrap_or_else(|| "closed".into())
                )));
            }
            if state.0 {
                return Ok(());
            }
            let _ = rx.changed().await;
        }
    }

    /// Opens the control stream (the first client stream, id 0).
    pub(crate) async fn open_control_stream(&self) -> Result<QuicStream> {
        Ok(QuicStream::new(self.inner.clone(), self.notify.clone(), 0))
    }

    /// Accepts the next data stream opened by the edge, or `None` once the
    /// connection closes.
    pub(crate) async fn accept_stream(&self) -> Result<Option<QuicStream>> {
        let mut rx = self.sequence_tx.subscribe();
        loop {
            let identifier = {
                let mut g = self.inner.lock().unwrap();
                if g.closed {
                    return Ok(None);
                }
                g.connection
                    .readable()
                    .find(|identifier| g.accepted.insert(*identifier))
            };
            if let Some(identifier) = identifier {
                return Ok(Some(QuicStream::new(
                    self.inner.clone(),
                    self.notify.clone(),
                    identifier,
                )));
            }
            if rx.changed().await.is_err() {
                return Ok(None);
            }
        }
    }

    /// Frees a stream from the accepted set once its serve task completes, so
    /// the set stays bounded over the connection's lifetime.
    pub(crate) fn release(&self, identifier: u64) {
        self.inner.lock().unwrap().accepted.remove(&identifier);
    }

    /// The reason the connection closed, if it has.
    pub(crate) fn close_reason(&self) -> Option<String> {
        self.inner.lock().unwrap().close_reason.clone()
    }

    /// Whether the connection ended with an idle timeout.
    pub(crate) fn timed_out(&self) -> bool {
        self.inner.lock().unwrap().timed_out
    }

    pub(crate) async fn close_and_wait(&mut self, grace_period: Duration) {
        self.close();
        if tokio::time::timeout(grace_period, &mut self.driver)
            .await
            .is_err()
        {
            self.driver.abort();
            let _ = (&mut self.driver).await;
        }
    }

    /// Gracefully closes the connection.
    pub(crate) fn close(&self) {
        let mut g = self.inner.lock().unwrap();
        let _ = g.connection.close(true, 0x00, b"");
        g.closed = true;
        for w in g.read_wakers.values() {
            w.wake_by_ref();
        }
        for w in g.write_wakers.values() {
            w.wake_by_ref();
        }
        g.read_wakers.clear();
        g.write_wakers.clear();
        self.notify.notify_waiters();
        let cur = *self.sequence_tx.borrow();
        let _ = self.sequence_tx.send(cur.wrapping_add(1));
    }
}

pub(crate) async fn drive(
    socket: UdpSocket,
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    sequence_tx: watch::Sender<u64>,
) {
    let mut receive_buffer = vec![0u8; 65535];
    let mut send_buffer = vec![0u8; MAXIMUM_DATAGRAM_SIZE];
    let mut sequence: u64 = 0;
    let mut last_keepalive = std::time::Instant::now();
    loop {
        // Send a PING each keepalive interval so the edge does not treat the connection as idle (cloudflared's KeepAlivePeriod is 1s).
        if last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
            let mut g = inner.lock().unwrap();
            if g.connection.is_established() {
                let _ = g.connection.send_ack_eliciting();
            }
            last_keepalive = std::time::Instant::now();
        }
        // Flush anything quiche queued (including the initial flight).
        loop {
            let (written, send_information) = {
                let mut g = inner.lock().unwrap();
                match g.connection.send(&mut send_buffer) {
                    Ok(v) => v,
                    Err(quiche::Error::Done) => break,
                    Err(e) => {
                        tracing::debug!(?e, "quiche send error");
                        break;
                    }
                }
            };
            if let Err(e) = socket
                .send_to(&send_buffer[..written], send_information.to)
                .await
            {
                tracing::debug!(?e, "udp send error");
                break;
            }
        }

        let timeout = inner.lock().unwrap().connection.timeout();
        let notified = notify.notified();
        tokio::pin!(notified);
        let sleep = async {
            match timeout {
                Some(d) => tokio::time::sleep(d).await,
                None => futures_util::future::pending().await,
            }
        };
        tokio::pin!(sleep);
        // Periodic kick retries blocked writers and flushes buffered data even when the peer sends no packets.
        let kick = tokio::time::sleep(Duration::from_millis(50));
        tokio::pin!(kick);
        let mut read_packets = false;
        tokio::select! {
            _ = &mut notified => {}
            result = socket.readable() => {
                if result.is_err() {
                    break;
                }
                read_packets = true;
            }
            _ = &mut sleep => {
                inner.lock().unwrap().connection.on_timeout();
            }
            _ = &mut kick => {}
        }

        if read_packets {
            let to = socket
                .local_addr()
                .unwrap_or_else(|_| ([0, 0, 0, 0], 0).into());
            loop {
                match socket.try_recv_from(&mut receive_buffer) {
                    Ok((length, from)) => {
                        let receive_information = quiche::RecvInfo { to, from };
                        let mut g = inner.lock().unwrap();
                        if let Err(e) = g
                            .connection
                            .recv(&mut receive_buffer[..length], receive_information)
                        {
                            tracing::trace!(?e, "ignoring unprocessable packet");
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        tracing::debug!(?e, "udp recv error");
                        break;
                    }
                }
            }
        }

        // Wake tasks blocked on streams with fresh data or capacity.
        let mut wake_read = Vec::new();
        let mut wake_write = Vec::new();
        let closed = {
            let mut g = inner.lock().unwrap();
            if !g.established && g.connection.is_established() {
                g.established = true;
            }
            for identifier in g.connection.readable() {
                if let Some(w) = g.read_wakers.remove(&identifier) {
                    wake_read.push(w);
                }
            }
            for identifier in g.connection.writable() {
                if let Some(w) = g.write_wakers.remove(&identifier) {
                    wake_write.push(w);
                }
            }
            // Wake blocked writers periodically; they re-check under the lock.
            for w in g.write_wakers.values() {
                wake_write.push(w.clone());
            }
            g.write_wakers.clear();
            // The local stop signal must not skip quiche's close-frame flush and draining.
            let closed = g.connection.is_closed();
            if closed {
                g.closed = true;
                g.timed_out = g.connection.is_timed_out();
                g.close_reason = Some(
                    g.connection
                        .peer_error()
                        .map(|e| format!("{e:?}"))
                        .unwrap_or_else(|| {
                            if g.timed_out {
                                "idle timeout".into()
                            } else {
                                "connection closed".into()
                            }
                        }),
                );
            }
            closed
        };
        for w in wake_read {
            w.wake();
        }
        for w in wake_write {
            w.wake();
        }
        if closed {
            let mut g = inner.lock().unwrap();
            for w in g.read_wakers.values() {
                w.wake_by_ref();
            }
            for w in g.write_wakers.values() {
                w.wake_by_ref();
            }
            g.read_wakers.clear();
            g.write_wakers.clear();
            let _ = sequence_tx.send(sequence.wrapping_add(1));
            break;
        }
        // Bump sequence every loop so watch subscribers (wait_established, serve_requests) wake and re-check state.
        sequence = sequence.wrapping_add(1);
        let _ = sequence_tx.send(sequence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn close_and_wait_emits_connection_close_before_stopping_driver() {
        use boring::asn1::Asn1Time;
        use boring::hash::MessageDigest;
        use boring::pkey::PKey;
        use boring::rsa::Rsa;
        use boring::ssl::{SslContextBuilder, SslMethod};
        use boring::x509::{X509, X509NameBuilder};

        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "localhost").unwrap();
        let name = name.build();
        let mut certificate = X509::builder().unwrap();
        certificate.set_version(2).unwrap();
        certificate.set_subject_name(&name).unwrap();
        certificate.set_issuer_name(&name).unwrap();
        certificate.set_pubkey(&key).unwrap();
        certificate
            .set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        certificate
            .set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        certificate.sign(&key, MessageDigest::sha256()).unwrap();
        let mut server_tls = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
        server_tls.set_certificate(&certificate.build()).unwrap();
        server_tls.set_private_key(&key).unwrap();
        let mut server_config =
            quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, server_tls)
                .unwrap();
        server_config.set_application_protos(&[EDGE_ALPN]).unwrap();
        let mut client_config = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
        // This peer and its ephemeral certificate exist only inside this test.
        client_config.verify_peer(false);
        client_config.set_application_protos(&[EDGE_ALPN]).unwrap();
        client_config.set_initial_rtt(Duration::from_millis(1));

        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let local = socket.local_addr().unwrap();
        let peer = peer_socket.local_addr().unwrap();
        socket.connect(peer).await.unwrap();
        let mut client = quiche::connect(
            Some("localhost"),
            &quiche::ConnectionId::from_ref(&[1; 16]),
            local,
            peer,
            &mut client_config,
        )
        .unwrap();
        let mut server = quiche::accept(
            &quiche::ConnectionId::from_ref(&[2; 16]),
            None,
            peer,
            local,
            &mut server_config,
        )
        .unwrap();
        let mut packet = [0; 65535];
        for _ in 0..16 {
            while let Ok((length, info)) = client.send(&mut packet) {
                server
                    .recv(
                        &mut packet[..length],
                        quiche::RecvInfo {
                            from: local,
                            to: info.to,
                        },
                    )
                    .unwrap();
            }
            while let Ok((length, info)) = server.send(&mut packet) {
                client
                    .recv(
                        &mut packet[..length],
                        quiche::RecvInfo {
                            from: peer,
                            to: info.to,
                        },
                    )
                    .unwrap();
            }
            if client.is_established() && server.is_established() {
                break;
            }
        }
        assert!(client.is_established() && server.is_established());

        let inner = Arc::new(Mutex::new(Inner {
            connection: client,
            read_wakers: HashMap::new(),
            write_wakers: HashMap::new(),
            accepted: HashSet::from([0]),
            established: true,
            closed: false,
            timed_out: false,
            close_reason: None,
        }));
        let notify = Arc::new(Notify::new());
        let (sequence_tx, mut sequence_rx) = watch::channel(0);
        let driver = crate::edge::OwnedTask::spawn(drive(
            socket,
            inner.clone(),
            notify.clone(),
            sequence_tx.clone(),
        ));
        let mut connection = QuicConnection {
            inner,
            notify,
            sequence_tx,
            driver,
        };
        // Wait for the driver to loop and park before waking it with close().
        tokio::time::timeout(Duration::from_secs(1), sequence_rx.changed())
            .await
            .unwrap()
            .unwrap();
        let receive_close = async {
            loop {
                let (length, from) = peer_socket.recv_from(&mut packet).await.unwrap();
                server
                    .recv(&mut packet[..length], quiche::RecvInfo { from, to: peer })
                    .unwrap();
                if let Some(error) = server.peer_error() {
                    assert!(error.is_app);
                    assert_eq!(error.error_code, 0);
                    break;
                }
            }
        };
        let ((), received) = tokio::join!(
            connection.close_and_wait(Duration::from_secs(1)),
            tokio::time::timeout(Duration::from_secs(1), receive_close),
        );
        received.expect("peer must decode CONNECTION_CLOSE before the driver stops");
    }
}
