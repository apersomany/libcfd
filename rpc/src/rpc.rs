use crate::error::{Result, RpcError};
use crate::io::{AsyncStream, read_message, write_message};
use crate::rpc_capnp;

/// A minimal Cap'n Proto RPC client for tunnel registration.
///
/// Implements the exact wire sequence cloudflared uses on its control
/// stream: bootstrap to obtain the peer's main interface, then
/// `call`/`return`/`finish` for each registration method. Question ids are
/// allocated monotonically from 0 and the bootstrapped capability is
/// tracked as import id 0.
pub struct RpcClient<S> {
    stream: S,
    next_question: u32,
    has_bootstrap: bool,
}

impl<S: AsyncStream + Unpin> RpcClient<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            next_question: 0,
            has_bootstrap: false,
        }
    }

    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Sends `Message::bootstrap` and waits for the return carrying the
    /// peer's main interface. Returns the import id to use for calls.
    pub async fn bootstrap(&mut self) -> Result<u32> {
        if self.has_bootstrap {
            return Err(RpcError::Protocol("bootstrap called twice".into()));
        }
        let question = self.next_question;
        self.next_question += 1;

        let mut message = capnp::message::Builder::new_default();
        let root = message.init_root::<rpc_capnp::message::Builder>();
        let mut bs = root.init_bootstrap();
        bs.set_question_id(question);

        tracing::trace!(question, "sending bootstrap");
        write_message(&mut self.stream, &message).await?;
        let reader = read_message(&mut self.stream).await?;
        let root = reader.get_root::<rpc_capnp::message::Reader>()?;
        let answer = self.expect_return(&root, question)?;
        let payload = match answer.reborrow().which()? {
            rpc_capnp::return_::Results(r) => r?,
            rpc_capnp::return_::Exception(e) => {
                return Err(RpcError::RemoteCall(e?.get_reason()?.to_str()?.to_string()));
            }
            _ => {
                return Err(RpcError::Protocol(
                    "bootstrap got unexpected return kind".into(),
                ));
            }
        };
        let ctab = payload.get_cap_table()?;
        if ctab.is_empty() {
            return Err(RpcError::Protocol(
                "bootstrap return carried no capability".into(),
            ));
        }
        let desc = ctab.get(0);
        match desc.reborrow().which()? {
            rpc_capnp::cap_descriptor::SenderHosted(_)
            | rpc_capnp::cap_descriptor::SenderPromise(_) => {
                self.has_bootstrap = true;
                Ok(0)
            }
            _ => Err(RpcError::Protocol(
                "unexpected capability descriptor kind in bootstrap return".into(),
            )),
        }
    }

    /// Performs a method call on an imported capability, decodes the results
    /// payload with `decode`, then sends `finish` for the question.
    pub async fn call<T>(
        &mut self,
        import_id: u32,
        interface_id: u64,
        method_id: u16,
        fill_params: impl FnOnce(&mut rpc_capnp::payload::Builder<'_>) -> Result<()>,
        decode: impl FnOnce(rpc_capnp::payload::Reader<'_>) -> Result<T>,
    ) -> Result<T> {
        if !self.has_bootstrap {
            return Err(RpcError::Protocol("call before bootstrap".into()));
        }
        let question = self.next_question;
        self.next_question += 1;

        let mut message = capnp::message::Builder::new_default();
        let root = message.init_root::<rpc_capnp::message::Builder>();
        let mut call = root.init_call();
        call.set_question_id(question);
        let mut target = call.reborrow().init_target();
        target.set_imported_cap(import_id);
        call.reborrow().set_interface_id(interface_id);
        call.reborrow().set_method_id(method_id);
        call.reborrow().init_send_results_to().set_caller(());
        let mut payload = call.reborrow().init_params();
        fill_params(&mut payload)?;

        tracing::trace!(question, interface_id, method_id, "sending call");
        write_message(&mut self.stream, &message).await?;
        let reader = read_message(&mut self.stream).await?;
        let root = reader.get_root::<rpc_capnp::message::Reader>()?;
        let answer = self.expect_return(&root, question)?;
        let payload = match answer.reborrow().which()? {
            rpc_capnp::return_::Results(r) => r?,
            rpc_capnp::return_::Exception(e) => {
                return Err(RpcError::RemoteCall(e?.get_reason()?.to_str()?.to_string()));
            }
            _ => return Err(RpcError::Protocol("call got unexpected return kind".into())),
        };
        let value = decode(payload)?;

        let mut finish = capnp::message::Builder::new_default();
        let froot = finish.init_root::<rpc_capnp::message::Builder>();
        let mut f = froot.init_finish();
        f.set_question_id(question);
        f.set_release_result_caps(false);
        write_message(&mut self.stream, &finish).await?;

        Ok(value)
    }

    fn expect_return<'a>(
        &self,
        root: &'a rpc_capnp::message::Reader<'a>,
        question: u32,
    ) -> Result<rpc_capnp::return_::Reader<'a>> {
        match root.reborrow().which()? {
            rpc_capnp::message::Return(ret) => {
                let ret = ret?;
                if ret.reborrow().get_answer_id() != question {
                    return Err(RpcError::Protocol(format!(
                        "answer id {} does not match question {}",
                        ret.reborrow().get_answer_id(),
                        question
                    )));
                }
                Ok(ret)
            }
            rpc_capnp::message::Abort(exc) => {
                let exc = exc?;
                let reason = exc.get_reason()?.to_str()?.to_string();
                let error_type = exc.get_type()? as u16;
                Err(RpcError::Abort { reason, error_type })
            }
            _ => Err(RpcError::Protocol("expected return message".into())),
        }
    }
}
