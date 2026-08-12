//! Shared HTTP/2 loopback harness: the in-process mock-edge TLS listener,
//! the control-stream RPC replies, and the origin stubs.

use std::sync::Arc;
use std::time::Duration;

use libcfd_rpc::Incoming;
use tokio::sync::Notify;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::control::RegistrationOptions;
use crate::error::Result;
use crate::event::Event;
use crate::h2::H2Shared;
use crate::h2::stream::H2Bidi;
use crate::origin::{Body, Duplex, Origin, Request, Response, WebSocketConnection};
use crate::tunnel::Tunnel;

use super::{BOOTSTRAP_RETURN, EMPTY_RETURN, REGISTER_RETURN, hex};

/// Generates a self-signed cert and binds the loopback listener the mock
/// edge accepts on.
pub(crate) async fn start_edge() -> (tokio::net::TcpListener, Vec<u8>, tokio_rustls::TlsAcceptor) {
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
    (listener, ca_pem, acceptor)
}

/// Answers the registration RPC calls on the mock-edge control stream and
/// closes it once unregistration/config-push completes.
pub(crate) async fn run_control_rpc(mut bidi: H2Bidi) {
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
}

/// Builds the shared state for a loopback h2 serve, returning it with the
/// shutdown signal the test fires at the end.
pub(crate) fn test_shared(tunnel: Arc<Tunnel>, origin: Arc<Origin>) -> (Arc<H2Shared>, Arc<Event>) {
    let shutdown = Arc::new(Event::new());
    let shared = Arc::new(H2Shared {
        tunnel,
        origin,
        reg_opts: Arc::new(RegistrationOptions::default()),
        config_json: Arc::new(br#"{"ingress":[]}"#.to_vec()),
        shutdown: shutdown.clone(),
        control_shutdown: Arc::new(Notify::new()),
        registered: Event::new(),
        grace_period: Duration::from_secs(30),
    });
    (shared, shutdown)
}

/// A duplex that echoes everything written to it back to its reader, then
/// closes once the write half is closed. Self-terminating so the response
/// side of a websocket/TCP exchange can end.
pub(crate) fn duplex_echo() -> Duplex {
    let (lib_read, mut echo_write) = tokio::io::duplex(16384);
    let (mut echo_read, lib_write) = tokio::io::duplex(16384);
    tokio::spawn(async move {
        let _ = tokio::io::copy(&mut echo_read, &mut echo_write).await;
    });
    Duplex::new(lib_read.compat(), lib_write.compat_write())
}

pub(crate) struct OneShotWsOrigin;

impl crate::origin::WebSocketOrigin for OneShotWsOrigin {
    async fn connect(&self, request: Request) -> Result<WebSocketConnection> {
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
        Ok(WebSocketConnection {
            response: Response::new(
                http::StatusCode::SWITCHING_PROTOCOLS,
                headers,
                Body::empty(),
            ),
            origin: duplex_echo(),
        })
    }
}

pub(crate) struct OneShotTcpOrigin;

impl crate::origin::TcpOrigin for OneShotTcpOrigin {
    async fn connect(&self, _request: Request) -> Result<Duplex> {
        Ok(duplex_echo())
    }
}
