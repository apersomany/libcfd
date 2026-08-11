use capnp::traits::ImbueMut;
use core::marker::Unpin;
use core::pin::Pin;
use core::task::{Context, Poll};
use libcfd_rpc::tunnel::{ClientInfo, ConnectionOptions, TunnelAuth, TunnelClient};
use libcfd_rpc::{RpcClient, rpc_capnp, tunnelrpc_capnp};
use tokio::io::{AsyncRead as _, AsyncWrite as _};

#[derive(Debug, PartialEq)]
enum Received {
    Bootstrap,
    Call { interface_id: u64, method_id: u16 },
    Finish { question_id: u32 },
}

/// Bridges a tokio duplex stream to futures-io traits.
struct TokioBridge(tokio::io::DuplexStream);
impl Unpin for TokioBridge {}

impl futures::io::AsyncRead for TokioBridge {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut rb = tokio::io::ReadBuf::new(buf);
        match Pin::new(&mut self.0).poll_read(cx, &mut rb) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(rb.filled().len())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl futures::io::AsyncWrite for TokioBridge {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// A mock edge registration server running as an async task.
///
/// Reads framed RPC messages from its stream, answers `bootstrap` with a
/// capability, answers method calls with results, and records what it saw.
async fn serve_mock<S: libcfd_rpc::AsyncStream + Unpin>(mut stream: S) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match libcfd_rpc::io::read_message(&mut stream).await {
            Ok(r) => r,
            Err(libcfd_rpc::RpcError::Eof) => break,
            Err(e) => panic!("mock server read error: {e}"),
        };
        let root = reader.get_root::<rpc_capnp::message::Reader>().unwrap();
        match root.reborrow().which().unwrap() {
            rpc_capnp::message::Bootstrap(bs) => {
                let q = bs.unwrap().get_question_id();
                received.push(Received::Bootstrap);
                let reply = build_bootstrap_return(q);
                libcfd_rpc::io::write_raw(&mut stream, &reply)
                    .await
                    .unwrap();
            }
            rpc_capnp::message::Call(c) => {
                let c = c.unwrap();
                let q = c.get_question_id();
                let iface = c.get_interface_id();
                let method = c.get_method_id();
                received.push(Received::Call {
                    interface_id: iface,
                    method_id: method,
                });
                let reply = match method {
                    0 => build_register_return(q),
                    1 | 2 => build_empty_return(q),
                    m => panic!("mock server: unexpected method {m}"),
                };
                libcfd_rpc::io::write_raw(&mut stream, &reply)
                    .await
                    .unwrap();
            }
            rpc_capnp::message::Finish(f) => {
                let f = f.unwrap();
                received.push(Received::Finish {
                    question_id: f.get_question_id(),
                });
            }
            _ => panic!("mock server: unexpected message"),
        }
    }
    received
}

fn build_bootstrap_return(answer_id: u32) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    let mut cap_table: capnp::private::layout::CapTable = Vec::new();
    let mut root = message.init_root::<rpc_capnp::message::Builder>();
    root.imbue_mut(&mut cap_table);
    let mut ret = root.init_return();
    ret.set_answer_id(answer_id);
    ret.set_release_param_caps(false);
    let mut results = ret.init_results();
    let mut content = results.reborrow().init_content();
    content.set_as_capability(Box::new(StubHook));
    let mut ctab = results.init_cap_table(1);
    ctab.reborrow().get(0).set_sender_hosted(0);
    capnp::serialize::write_message_to_words(&message)
}

fn build_register_return(answer_id: u32) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(answer_id);
    ret.set_release_param_caps(false);
    let mut res = ret.init_results();
    let mut rres = res
        .reborrow()
        .init_content()
        .init_as::<tunnelrpc_capnp::registration_server::register_connection_results::Builder>(
    );
    let mut conn_resp = rres.reborrow().init_result();
    let mut cd = conn_resp.reborrow().init_result().init_connection_details();
    cd.set_uuid(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    cd.set_location_name("lhr");
    cd.set_tunnel_is_remotely_managed(false);
    res.reborrow().init_cap_table(0);
    capnp::serialize::write_message_to_words(&message)
}

fn build_register_error_return(
    answer_id: u32,
    cause: &str,
    retry_after: i64,
    should_retry: bool,
) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(answer_id);
    ret.set_release_param_caps(false);
    let mut res = ret.init_results();
    let mut rres = res
        .reborrow()
        .init_content()
        .init_as::<tunnelrpc_capnp::registration_server::register_connection_results::Builder>(
    );
    let mut conn_resp = rres.reborrow().init_result();
    let mut err = conn_resp.reborrow().init_result().init_error();
    err.set_cause(cause);
    err.set_retry_after(retry_after);
    err.set_should_retry(should_retry);
    res.reborrow().init_cap_table(0);
    capnp::serialize::write_message_to_words(&message)
}

fn build_empty_return(answer_id: u32) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(answer_id);
    ret.set_release_param_caps(false);
    let mut res = ret.init_results();
    res.reborrow().init_cap_table(0);
    capnp::serialize::write_message_to_words(&message)
}

#[derive(Clone)]
pub struct StubHook;
impl capnp::private::capability::ClientHook for StubHook {
    fn add_ref(&self) -> Box<dyn capnp::private::capability::ClientHook> {
        Box::new(StubHook)
    }
    fn new_call(
        &self,
        _i: u64,
        _m: u16,
        _s: Option<capnp::MessageSize>,
    ) -> capnp::capability::Request<capnp::any_pointer::Owned, capnp::any_pointer::Owned> {
        unimplemented!()
    }
    fn call(
        &self,
        _i: u64,
        _m: u16,
        _p: Box<dyn capnp::private::capability::ParamsHook>,
        _r: Box<dyn capnp::private::capability::ResultsHook>,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        unimplemented!()
    }
    fn get_brand(&self) -> usize {
        0
    }
    fn get_ptr(&self) -> usize {
        0
    }
    fn get_resolved(&self) -> Option<Box<dyn capnp::private::capability::ClientHook>> {
        None
    }
    fn when_more_resolved(
        &self,
    ) -> Option<
        capnp::capability::Promise<Box<dyn capnp::private::capability::ClientHook>, capnp::Error>,
    > {
        None
    }
    fn when_resolved(&self) -> capnp::capability::Promise<(), capnp::Error> {
        capnp::capability::Promise::ok(())
    }
}

#[test]
fn tunnel_client_registers_with_mock_edge() {
    futures::executor::block_on(async {
        let (client_half, server_half) = tokio::io::duplex(65536);
        let client_stream = TokioBridge(client_half);
        let server_stream = TokioBridge(server_half);

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            let auth = TunnelAuth {
                account_tag: "account-tag-123".into(),
                tunnel_secret: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            };
            let tunnel_id: Vec<u8> = (0..16).map(|i| i as u8).collect();
            let options = ConnectionOptions {
                client: ClientInfo {
                    client_id: b"0123456789abcdef".to_vec(),
                    features: vec!["allow_remote_config".into(), "support_datagram_v2".into()],
                    version: "2026.7.3".into(),
                    arch: "linux/amd64".into(),
                },
                origin_local_ip: vec![10, 0, 0, 1],
                replace_existing: false,
                compression_quality: 0,
                num_previous_attempts: 1,
            };
            client
                .register_connection(auth, &tunnel_id, 0, &options)
                .await
                .unwrap()
        };
        let server_fut = async move { serve_mock(server_stream).await };

        let (_client_res, received) = futures::future::join(client_fut, server_fut).await;

        // The client should have done bootstrap(0) -> call(0, register) -> finish.
        assert_eq!(
            received,
            vec![
                Received::Bootstrap,
                Received::Call {
                    interface_id: 0xf71695ec7fe85497,
                    method_id: 0
                },
                Received::Finish { question_id: 1 },
            ]
        );
    });
}

async fn serve_mock_err<S: libcfd_rpc::AsyncStream + Unpin>(mut stream: S) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match libcfd_rpc::io::read_message(&mut stream).await {
            Ok(r) => r,
            Err(libcfd_rpc::RpcError::Eof) => break,
            Err(e) => panic!("mock server read error: {e}"),
        };
        let root = reader.get_root::<rpc_capnp::message::Reader>().unwrap();
        match root.reborrow().which().unwrap() {
            rpc_capnp::message::Bootstrap(bs) => {
                let q = bs.unwrap().get_question_id();
                received.push(Received::Bootstrap);
                let reply = build_bootstrap_return(q);
                libcfd_rpc::io::write_raw(&mut stream, &reply)
                    .await
                    .unwrap();
            }
            rpc_capnp::message::Call(c) => {
                let c = c.unwrap();
                let q = c.get_question_id();
                received.push(Received::Call {
                    interface_id: c.get_interface_id(),
                    method_id: c.get_method_id(),
                });
                let reply = build_register_error_return(q, "EDUPCONN", 0, false);
                libcfd_rpc::io::write_raw(&mut stream, &reply)
                    .await
                    .unwrap();
            }
            rpc_capnp::message::Finish(_) => {}
            _ => panic!("mock server: unexpected message"),
        }
    }
    received
}

#[test]
fn tunnel_client_register_error_returns_connection_error() {
    futures::executor::block_on(async {
        let (client_half, server_half) = tokio::io::duplex(65536);
        let client_stream = TokioBridge(client_half);
        let server_stream = TokioBridge(server_half);

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            let auth = TunnelAuth {
                account_tag: "tag".into(),
                tunnel_secret: vec![1; 16],
            };
            client
                .register_connection(auth, &[0u8; 16], 0, &ConnectionOptions::default())
                .await
        };
        let server_fut = async move { serve_mock_err(server_stream).await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        let resp = client_res.unwrap();
        let failure = resp.into_result().unwrap_err();
        match failure {
            libcfd_rpc::tunnel::RegistrationFailure::Permanent(cause) => {
                assert_eq!(cause, "EDUPCONN");
            }
            other => panic!("expected permanent EDUPCONN, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_unregisters_with_mock_edge() {
    futures::executor::block_on(async {
        let (client_half, server_half) = tokio::io::duplex(65536);
        let client_stream = TokioBridge(client_half);
        let server_stream = TokioBridge(server_half);

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client.unregister_connection().await.unwrap();
        };
        let server_fut = async move { serve_mock(server_stream).await };

        let (_client_res, received) = futures::future::join(client_fut, server_fut).await;

        assert_eq!(
            received,
            vec![
                Received::Bootstrap,
                Received::Call {
                    interface_id: 0xf71695ec7fe85497,
                    method_id: 1
                },
                Received::Finish { question_id: 1 },
            ]
        );
    });
}
