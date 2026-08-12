//! Server side of the connector's `CloudflaredServer` interface.
//!
//! The edge opens a dedicated RPC stream (prefixed with the RPC protocol
//! signature) and calls the connector's main interface: `updateConfiguration`
//! pushes the remotely-managed tunnel configuration (its ingress rules and
//! therefore its public hostnames), while the UDP session methods manage
//! QUIC datagram sessions, which libcfd does not support.

use capnp::private::capability::{ClientHook, ParamsHook, ResultsHook};
use capnp::traits::ImbueMut;

use crate::error::{Result, RpcError};
use crate::io::{AsyncStream, read_message, write_raw};
use crate::rpc_capnp;
use crate::tunnelrpc_capnp;

/// The 64-bit interface id of the connector's `CloudflaredServer`.
pub const CLOUDFLARED_SERVER_INTERFACE_ID: u64 = 0xf548_cef9_dea2_a4a1;
/// The 64-bit interface id of `SessionManager` (extended by
/// `CloudflaredServer`); capnp-go addresses its methods by this id.
pub const SESSION_MANAGER_INTERFACE_ID: u64 = 0x8394_45a5_9fb0_1686;
/// The 64-bit interface id of `ConfigurationManager`; the edge addresses
/// `updateConfiguration` by this id.
pub const CONFIGURATION_MANAGER_INTERFACE_ID: u64 = 0xb48e_dfbd_aa25_db04;

/// The connector's reply to `updateConfiguration`.
#[derive(Debug, Clone, Default)]
pub struct UpdateConfigurationResponse {
    /// The configuration version the connector applied.
    pub latest_applied_version: i32,
    /// An empty string on success.
    pub error: String,
}

/// The connector's reply to `registerUdpSession`.
#[derive(Debug, Clone, Default)]
pub struct RegisterUdpSessionResponse {
    /// An empty string on success.
    pub error: String,
    /// Session spans (unused by libcfd).
    pub spans: Vec<u8>,
}

/// Handlers for the RPC calls the edge makes on the connector.
pub trait CloudflaredHandler: Send + Sync {
    /// Applies a remotely-managed configuration push from the edge.
    fn update_configuration(&self, version: i32, config: &[u8]) -> UpdateConfigurationResponse;
    /// Registers a UDP session for QUIC datagrams. libcfd does not support
    /// this; the default replies with an error.
    fn register_udp_session(
        &self,
        _session_id: &[u8; 16],
        _dst_ip: &[u8],
        _dst_port: u16,
    ) -> RegisterUdpSessionResponse {
        RegisterUdpSessionResponse {
            error: "UDP sessions are not supported".into(),
            spans: Vec::new(),
        }
    }
    /// Unregisters a UDP session. No-op by default.
    fn unregister_udp_session(&self, _session_id: &[u8; 16], _message: &str) {}
}

/// Serves the edge's calls on an RPC stream until the stream ends.
///
/// Answers the bootstrap with a `senderHosted` capability (mirroring
/// capnp-go's main-interface handshake), dispatches `updateConfiguration`
/// to [`CloudflaredHandler::update_configuration`], and answers the UDP
/// session methods from the handler. The edge sends `finish` and `release`
/// messages between calls; those need no reply.
pub async fn serve_cloudflared<S, H>(stream: &mut S, handler: &H) -> Result<()>
where
    S: AsyncStream + Unpin,
    H: CloudflaredHandler,
{
    loop {
        // All capnp reading and reply building happens in a scope that ends
        // before the next await, so no non-`Send` capnp state is held
        // across an await point (mirroring the RPC client's rule).
        let reply: Option<Vec<u8>> = {
            let reader = match read_message(stream).await {
                Ok(reader) => reader,
                Err(RpcError::Eof) => return Ok(()),
                Err(e) => return Err(e),
            };
            let root = reader.get_root::<rpc_capnp::message::Reader>()?;
            match root.reborrow().which()? {
                rpc_capnp::message::Bootstrap(b) => {
                    let question = b?.get_question_id();
                    tracing::debug!(question, "edge bootstrapped the RPC stream");
                    Some(build_bootstrap_return(question)?)
                }
                rpc_capnp::message::Call(c) => {
                    let call = c?;
                    let question = call.get_question_id();
                    tracing::debug!(
                        question,
                        interface_id = call.get_interface_id(),
                        method_id = call.get_method_id(),
                        "edge called an RPC method"
                    );
                    let reply = match classify(call.get_interface_id(), call.get_method_id()) {
                        Method::UpdateConfiguration => {
                            let params = call.reborrow().get_params()?.get_content().get_as::<
                                tunnelrpc_capnp::configuration_manager::update_configuration_params::Reader<'_>,
                            >()?;
                            let response = handler
                                .update_configuration(params.get_version(), params.get_config()?);
                            build_update_configuration_return(question, &response)?
                        }
                        Method::RegisterUdpSession => {
                            let params = call.reborrow().get_params()?.get_content().get_as::<
                                tunnelrpc_capnp::session_manager::register_udp_session_params::Reader<'_>,
                            >()?;
                            let response = handler.register_udp_session(
                                &session_id_bytes(params.get_session_id()?),
                                params.get_dst_ip()?,
                                params.get_dst_port(),
                            );
                            build_register_udp_session_return(question, &response)?
                        }
                        Method::UnregisterUdpSession => {
                            build_unregister_udp_session_return(question)?
                        }
                        Method::Unknown => {
                            tracing::debug!(
                                interface_id = call.get_interface_id(),
                                method_id = call.get_method_id(),
                                "edge called an unknown RPC method"
                            );
                            crate::rpc::build_exception(question, "unimplemented")?
                        }
                    };
                    Some(reply)
                }
                rpc_capnp::message::Finish(_) | rpc_capnp::message::Release(_) => None,
                _ => None,
            }
        };
        if let Some(reply) = reply {
            write_raw(stream, &reply).await?;
        }
    }
}

/// Which `CloudflaredServer` method a call addresses.
///
/// Method ordinals are per-interface: `updateConfiguration` is method 0 on
/// `ConfigurationManager` but method 2 on the combined `CloudflaredServer`
/// (after `SessionManager`'s two methods); the edge uses the sub-interface
/// ids and ordinals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    UpdateConfiguration,
    RegisterUdpSession,
    UnregisterUdpSession,
    Unknown,
}

fn classify(interface_id: u64, method_id: u16) -> Method {
    match (interface_id, method_id) {
        (CONFIGURATION_MANAGER_INTERFACE_ID, 0) | (CLOUDFLARED_SERVER_INTERFACE_ID, 2) => {
            Method::UpdateConfiguration
        }
        (SESSION_MANAGER_INTERFACE_ID, 0) | (CLOUDFLARED_SERVER_INTERFACE_ID, 0) => {
            Method::RegisterUdpSession
        }
        (SESSION_MANAGER_INTERFACE_ID, 1) | (CLOUDFLARED_SERVER_INTERFACE_ID, 1) => {
            Method::UnregisterUdpSession
        }
        _ => Method::Unknown,
    }
}

/// Copies a data blob into a 16-byte session id, zero-padding short inputs.
fn session_id_bytes(data: &[u8]) -> [u8; 16] {
    let mut id = [0u8; 16];
    let n = data.len().min(16);
    id[..n].copy_from_slice(&data[..n]);
    id
}

/// Builds the bootstrap answer: a `return` whose results carry the
/// connector's main interface as a `senderHosted` capability, exactly as
/// capnp-go answers a bootstrap question.
fn build_bootstrap_return(question: u32) -> Result<Vec<u8>> {
    // The capability table must outlive the message so the interface
    // pointer written below can reference it.
    let mut caps: capnp::private::layout::CapTable = Vec::new();
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(question);
    let mut results = ret.init_results();
    let mut payload = results.reborrow();
    let mut content = payload.reborrow().init_content();
    content.imbue_mut(&mut caps);
    content.set_as_capability(Box::new(DummyHook));
    let mut ctab = payload.reborrow().init_cap_table(1);
    ctab.reborrow().get(0).set_sender_hosted(0);
    Ok(crate::io::serialize_message(&message))
}

/// Builds the `return` for `updateConfiguration` with the typed results
/// payload.
fn build_update_configuration_return(
    question: u32,
    response: &UpdateConfigurationResponse,
) -> Result<Vec<u8>> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(question);
    let mut results = ret.init_results();
    let mut payload = results.reborrow();
    {
        let content = payload.reborrow().init_content();
        let mut rres =
            content.init_as::<tunnelrpc_capnp::configuration_manager::update_configuration_results::Builder>();
        let mut resp = rres.reborrow().init_result();
        resp.set_latest_applied_version(response.latest_applied_version);
        resp.set_err(&response.error);
    }
    payload.reborrow().init_cap_table(0);
    Ok(crate::io::serialize_message(&message))
}

/// Builds the `return` for `registerUdpSession`.
fn build_register_udp_session_return(
    question: u32,
    response: &RegisterUdpSessionResponse,
) -> Result<Vec<u8>> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(question);
    let mut results = ret.init_results();
    let mut payload = results.reborrow();
    {
        let content = payload.reborrow().init_content();
        let mut rres = content
            .init_as::<tunnelrpc_capnp::session_manager::register_udp_session_results::Builder>(
        );
        let mut resp = rres.reborrow().init_result();
        resp.set_err(&response.error);
        resp.set_spans(&response.spans);
    }
    payload.reborrow().init_cap_table(0);
    Ok(crate::io::serialize_message(&message))
}

/// Builds the `return` for `unregisterUdpSession` (an empty results struct).
fn build_unregister_udp_session_return(question: u32) -> Result<Vec<u8>> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(question);
    let mut results = ret.init_results();
    let mut payload = results.reborrow();
    payload
        .reborrow()
        .init_content()
        .init_as::<tunnelrpc_capnp::session_manager::unregister_udp_session_results::Builder>();
    payload.reborrow().init_cap_table(0);
    Ok(crate::io::serialize_message(&message))
}

/// A placeholder capability hook for the bootstrap answer's cap table.
///
/// libcfd dispatches incoming calls by interface and method id directly, so
/// this hook is never invoked; it only exists so the interface pointer can
/// be written with capnp-rust's pointer API.
struct DummyHook;

impl ClientHook for DummyHook {
    fn add_ref(&self) -> Box<dyn ClientHook> {
        Box::new(DummyHook)
    }
    fn new_call(
        &self,
        _interface_id: u64,
        _method_id: u16,
        _size_hint: Option<capnp::MessageSize>,
    ) -> capnp::capability::Request<capnp::any_pointer::Owned, capnp::any_pointer::Owned> {
        unreachable!("dummy hook is never called")
    }
    fn call(
        &self,
        _interface_id: u64,
        _method_id: u16,
        _params: Box<dyn ParamsHook>,
        _results: Box<dyn ResultsHook>,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        unreachable!("dummy hook is never called")
    }
    fn get_brand(&self) -> usize {
        0
    }
    fn get_ptr(&self) -> usize {
        0
    }
    fn get_resolved(&self) -> Option<Box<dyn ClientHook>> {
        None
    }
    fn when_more_resolved(
        &self,
    ) -> Option<capnp::capability::Promise<Box<dyn ClientHook>, capnp::Error>> {
        None
    }
    fn when_resolved(&self) -> capnp::capability::Promise<(), capnp::Error> {
        capnp::capability::Promise::ok(())
    }
}
