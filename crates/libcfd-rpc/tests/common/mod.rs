//! Shared test support for the rpc integration tests: a tokio-to-futures
//! stream bridge, a stub capability hook, and a scripted mock edge that
//! answers framed RPC messages and records what it saw.

#![allow(dead_code)]

use core::marker::Unpin;
use core::pin::Pin;
use core::task::{Context, Poll};

use capnp::traits::ImbueMut;
use libcfd_rpc::{
    rpc_capnp,
    tunnel::{ClientInformation, ConnectionOptions},
    tunnelrpc_capnp,
};
use tokio::io::{AsyncRead as _, AsyncWrite as _};

/// Bridges a tokio duplex stream to the futures-io traits the RPC crate
/// uses.
pub struct TokioBridge(pub tokio::io::DuplexStream);
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

/// A minimal capability hook used to place a capability in a return message.
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

/// What the mock edge server observed on its stream.
#[derive(Debug, PartialEq)]
pub enum Received {
    Bootstrap,
    Call { interface_id: u64, method_id: u16 },
    Finish { question_id: u32 },
    Release,
}

/// The decoded `registerConnection` parameters, so tests can verify the
/// client encoded every credential and option.
#[derive(Debug)]
pub struct DecodedRegister {
    pub account_tag: String,
    pub tunnel_secret: Vec<u8>,
    pub tunnel_identifier: Vec<u8>,
    pub connection_index: u8,
    pub options: ConnectionOptions,
}

pub fn build_bootstrap_return(answer_id: u32) -> Vec<u8> {
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

pub fn build_register_return(answer_id: u32) -> Vec<u8> {
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

pub fn build_register_error_return(
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

pub fn build_empty_return(answer_id: u32) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(answer_id);
    ret.set_release_param_caps(false);
    let mut res = ret.init_results();
    res.reborrow().init_cap_table(0);
    capnp::serialize::write_message_to_words(&message)
}

pub fn build_exception_return(answer_id: u32, reason: &str) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(answer_id);
    let mut exc = ret.init_exception();
    exc.set_reason(reason);
    exc.set_type(rpc_capnp::exception::Type::Unimplemented);
    capnp::serialize::write_message_to_words(&message)
}

/// A `finish` message where a return is expected: the wrong message kind.
pub fn build_finish_message(question_id: u32) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut f = root.init_finish();
    f.set_question_id(question_id);
    f.set_release_result_caps(false);
    capnp::serialize::write_message_to_words(&message)
}

async fn read_message_or_eof<S: libcfd_rpc::AsyncStream + Unpin>(
    stream: &mut S,
) -> Option<capnp::message::Reader<capnp::serialize::OwnedSegments>> {
    match libcfd_rpc::io::read_message(stream).await {
        Ok(r) => Some(r),
        Err(libcfd_rpc::RpcError::Eof) => None,
        Err(e) => panic!("mock server read error: {e}"),
    }
}

fn decode_register_params(payload: rpc_capnp::payload::Reader<'_>) -> Option<DecodedRegister> {
    let params = payload
        .get_content()
        .get_as::<tunnelrpc_capnp::registration_server::register_connection_params::Reader<'_>>()
        .ok()?;
    let auth = params.reborrow().get_auth().ok()?;
    let options = params.reborrow().get_options().ok()?;
    let client = options.reborrow().get_client().ok()?;
    let mut features = Vec::new();
    if let Ok(list) = client.reborrow().get_features() {
        for feature in list.iter().flatten() {
            features.push(feature.to_str().unwrap().to_string());
        }
    }
    Some(DecodedRegister {
        account_tag: auth.get_account_tag().ok()?.to_str().ok()?.to_string(),
        tunnel_secret: auth.get_tunnel_secret().ok()?.to_vec(),
        tunnel_identifier: params.get_tunnel_id().ok()?.to_vec(),
        connection_index: params.get_conn_index(),
        options: ConnectionOptions {
            client: ClientInformation {
                client_identifier: client.get_client_id().ok()?.to_vec(),
                features,
                version: client.get_version().ok()?.to_str().ok()?.to_string(),
                arch: client.get_arch().ok()?.to_str().ok()?.to_string(),
            },
            origin_local_ip: options.get_origin_local_ip().ok()?.to_vec(),
            replace_existing: options.get_replace_existing(),
            compression_quality: options.get_compression_quality(),
            number_previous_attempts: options.get_num_previous_attempts(),
        },
    })
}

/// Answers `bootstrap` with a capability, methods 0/1/2 with results, and
/// records everything it saw. Decodes method 0 parameters for later
/// assertions.
pub async fn serve_mock<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
) -> (Vec<Received>, Option<DecodedRegister>) {
    let mut received = Vec::new();
    let mut register = None;
    loop {
        let reader = match read_message_or_eof(&mut stream).await {
            Some(r) => r,
            None => break,
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
                    0 => {
                        let payload = c.reborrow().get_params().unwrap();
                        register = decode_register_params(payload);
                        build_register_return(q)
                    }
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
            rpc_capnp::message::Release(_) => {
                received.push(Received::Release);
            }
            _ => panic!("mock server: unexpected message"),
        }
    }
    (received, register)
}

/// Like [`serve_mock`] but stops after the release message so tests of the
/// close path can observe it.
pub async fn serve_mock_with_release<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match read_message_or_eof(&mut stream).await {
            Some(r) => r,
            None => break,
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
                received.push(Received::Call {
                    interface_id: c.get_interface_id(),
                    method_id: c.get_method_id(),
                });
                let reply = build_empty_return(c.get_question_id());
                libcfd_rpc::io::write_raw(&mut stream, &reply)
                    .await
                    .unwrap();
            }
            rpc_capnp::message::Finish(_) => {}
            rpc_capnp::message::Release(_) => {
                received.push(Received::Release);
                break;
            }
            _ => panic!("mock server: unexpected message"),
        }
    }
    received
}

/// Answers bootstrap and then every call with the given registration error.
pub async fn serve_mock_register_error<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
    cause: &str,
    retry_after: i64,
    should_retry: bool,
) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match read_message_or_eof(&mut stream).await {
            Some(r) => r,
            None => break,
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
                let reply = build_register_error_return(q, cause, retry_after, should_retry);
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

/// Answers bootstrap with a capability and every call with an RPC-level
/// exception return.
pub async fn serve_mock_exception<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
    reason: &str,
) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match read_message_or_eof(&mut stream).await {
            Some(r) => r,
            None => break,
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
                received.push(Received::Call {
                    interface_id: c.get_interface_id(),
                    method_id: c.get_method_id(),
                });
                let reply = build_exception_return(c.get_question_id(), reason);
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

/// Answers bootstrap with an RPC-level exception return instead of a
/// capability.
pub async fn serve_mock_bootstrap_exception<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
    reason: &str,
) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match read_message_or_eof(&mut stream).await {
            Some(r) => r,
            None => break,
        };
        let root = reader.get_root::<rpc_capnp::message::Reader>().unwrap();
        match root.reborrow().which().unwrap() {
            rpc_capnp::message::Bootstrap(bs) => {
                let q = bs.unwrap().get_question_id();
                received.push(Received::Bootstrap);
                let reply = build_exception_return(q, reason);
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

/// Answers bootstrap with a capability but replies to calls with the wrong
/// answer id, which the client must reject.
pub async fn serve_mock_wrong_answer<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match read_message_or_eof(&mut stream).await {
            Some(r) => r,
            None => break,
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
                let reply = build_register_return(q.wrapping_add(1));
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

/// Answers bootstrap with a capability but replies to calls with a `finish`
/// message instead of a return: the wrong message kind.
pub async fn serve_mock_unexpected_kind<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match read_message_or_eof(&mut stream).await {
            Some(r) => r,
            None => break,
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
                received.push(Received::Call {
                    interface_id: c.get_interface_id(),
                    method_id: c.get_method_id(),
                });
                let reply = build_finish_message(c.get_question_id());
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

/// Answers bootstrap with a capability, then feeds the client garbage
/// bytes where a return is expected.
pub async fn serve_mock_garbage<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
    garbage: &[u8],
) -> Vec<Received> {
    let mut received = Vec::new();
    loop {
        let reader = match read_message_or_eof(&mut stream).await {
            Some(r) => r,
            None => break,
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
                received.push(Received::Call {
                    interface_id: c.get_interface_id(),
                    method_id: c.get_method_id(),
                });
                libcfd_rpc::io::write_raw(&mut stream, garbage)
                    .await
                    .unwrap();
                break;
            }
            rpc_capnp::message::Finish(_) => {}
            _ => panic!("mock server: unexpected message"),
        }
    }
    received
}

/// Answers bootstrap with a capability, reads the following call (so the
/// client's write completes), then drops the stream: the client must
/// observe EOF instead of a return.
pub async fn serve_mock_close_after_bootstrap<S: libcfd_rpc::AsyncStream + Unpin>(
    mut stream: S,
) -> Vec<Received> {
    let mut received = Vec::new();
    let reader = read_message_or_eof(&mut stream).await.expect("bootstrap");
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
        _ => panic!("mock server: expected bootstrap"),
    }
    // Consume the client's finish and call so its writes complete before
    // the stream is dropped.
    loop {
        let reader = read_message_or_eof(&mut stream).await.expect("call");
        let root = reader.get_root::<rpc_capnp::message::Reader>().unwrap();
        match root.reborrow().which().unwrap() {
            rpc_capnp::message::Finish(_) => {}
            rpc_capnp::message::Call(c) => {
                let c = c.unwrap();
                received.push(Received::Call {
                    interface_id: c.get_interface_id(),
                    method_id: c.get_method_id(),
                });
                break;
            }
            _ => panic!("mock server: expected call"),
        }
    }
    // Dropping the stream closes this half of the duplex.
    received
}

/// Convenience for tests that drive `RpcClient`/`TunnelClient` against the
/// mock server: builds both halves of a duplex.
pub fn client_stream() -> (TokioBridge, TokioBridge) {
    let (client_half, server_half) = tokio::io::duplex(65536);
    (TokioBridge(client_half), TokioBridge(server_half))
}
