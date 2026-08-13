use crate::error::{Result, RpcError};
use crate::io::{AsyncStream, read_message};
use crate::rpc_capnp;

/// A minimal Cap'n Proto RPC client for tunnel registration.
///
/// Implements the exact wire sequence cloudflared uses on its control
/// stream: bootstrap to obtain the peer's main interface, then
/// `call`/`return`/`finish` for each registration method. Question ids are
/// allocated monotonically from 0 and the bootstrapped capability is
/// tracked as import id 0.
///
/// All capnp message construction and decoding happens in synchronous
/// helpers so no non-`Send` capnp state is held across an await, keeping
/// every future `Send`.
pub struct RpcClient<S> {
    stream: S,
    next_question: u32,
    has_bootstrap: bool,
}

enum BootstrapOutcome {
    Capability,
    Exception(String),
    NoCapability,
    UnexpectedKind,
}

enum CallOutcome<T> {
    Value(T),
    Exception(String),
    UnexpectedKind,
}

impl<S: AsyncStream + Unpin> RpcClient<S> {
    /// Wraps a bidirectional stream as an RPC client.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            next_question: 0,
            has_bootstrap: false,
        }
    }

    /// Returns the underlying stream (the connection stays open).
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

        tracing::trace!(question, "sending bootstrap");
        let bytes = build_bootstrap(question)?;
        crate::io::write_raw(&mut self.stream, &bytes).await?;
        let reader = read_message(&mut self.stream).await?;
        let outcome = decode_bootstrap(&reader, question)?;
        drop(reader);
        match outcome {
            BootstrapOutcome::Capability => {
                self.has_bootstrap = true;
                self.send_finish(question, false).await?;
                Ok(0)
            }
            BootstrapOutcome::Exception(reason) => {
                self.send_finish(question, true).await?;
                Err(RpcError::RemoteCall(reason))
            }
            BootstrapOutcome::NoCapability => {
                self.send_finish(question, true).await?;
                Err(RpcError::Protocol(
                    "bootstrap return carried no capability".into(),
                ))
            }
            BootstrapOutcome::UnexpectedKind => {
                self.send_finish(question, true).await?;
                Err(RpcError::Protocol(
                    "bootstrap got unexpected return kind".into(),
                ))
            }
        }
    }

    /// Performs a method call on an imported capability, decodes the results
    /// payload with `decode`, then sends `finish` for the question.
    pub async fn call<T>(
        &mut self,
        import_identifier: u32,
        interface_identifier: u64,
        method_identifier: u16,
        fill_parameters: impl FnOnce(&mut rpc_capnp::payload::Builder<'_>) -> Result<()>,
        decode: impl FnOnce(rpc_capnp::payload::Reader<'_>) -> Result<T>,
    ) -> Result<T> {
        if !self.has_bootstrap {
            return Err(RpcError::Protocol("call before bootstrap".into()));
        }
        let question = self.next_question;
        self.next_question += 1;

        tracing::trace!(
            question,
            interface_identifier,
            method_identifier,
            "sending call"
        );
        let bytes = build_call(
            question,
            import_identifier,
            interface_identifier,
            method_identifier,
            fill_parameters,
        )?;
        crate::io::write_raw(&mut self.stream, &bytes).await?;
        let reader = read_message(&mut self.stream).await?;
        let outcome = decode_call(&reader, question, decode)?;
        drop(reader);
        match outcome {
            CallOutcome::Value(value) => {
                self.send_finish(question, false).await?;
                Ok(value)
            }
            CallOutcome::Exception(reason) => {
                self.send_finish(question, true).await?;
                Err(RpcError::RemoteCall(reason))
            }
            CallOutcome::UnexpectedKind => {
                self.send_finish(question, true).await?;
                Err(RpcError::Protocol("call got unexpected return kind".into()))
            }
        }
    }

    /// Sends `Message::finish` for a resolved or failed question. Capnp-go
    /// sends `releaseResultCaps: true` when the return was an exception.
    async fn send_finish(&mut self, question: u32, release_result_caps: bool) -> Result<()> {
        let bytes = build_finish(question, release_result_caps)?;
        crate::io::write_raw(&mut self.stream, &bytes).await
    }

    /// Releases the bootstrapped capability and returns the underlying
    /// stream. Sends `Message::release` for import id 0, mirroring what
    /// capnp-go sends when a registration client is closed.
    pub async fn close(mut self) -> Result<S> {
        if self.has_bootstrap {
            let bytes = build_release(0, 1)?;
            crate::io::write_raw(&mut self.stream, &bytes).await?;
            self.has_bootstrap = false;
        }
        Ok(self.stream)
    }
}

fn decode_bootstrap(
    reader: &capnp::message::Reader<capnp::serialize::OwnedSegments>,
    question: u32,
) -> Result<BootstrapOutcome> {
    let root = reader.get_root::<rpc_capnp::message::Reader>()?;
    let answer = expect_return(&root, question)?;
    let payload = match answer.reborrow().which()? {
        rpc_capnp::return_::Results(r) => r?,
        rpc_capnp::return_::Exception(e) => {
            return Ok(BootstrapOutcome::Exception(
                e?.get_reason()?.to_str()?.to_string(),
            ));
        }
        _ => return Ok(BootstrapOutcome::UnexpectedKind),
    };
    let ctab = payload.get_cap_table()?;
    if ctab.is_empty() {
        return Ok(BootstrapOutcome::NoCapability);
    }
    let desc = ctab.get(0);
    match desc.reborrow().which()? {
        rpc_capnp::cap_descriptor::SenderHosted(_)
        | rpc_capnp::cap_descriptor::SenderPromise(_) => Ok(BootstrapOutcome::Capability),
        _ => Ok(BootstrapOutcome::UnexpectedKind),
    }
}

fn decode_call<T>(
    reader: &capnp::message::Reader<capnp::serialize::OwnedSegments>,
    question: u32,
    decode: impl FnOnce(rpc_capnp::payload::Reader<'_>) -> Result<T>,
) -> Result<CallOutcome<T>> {
    let root = reader.get_root::<rpc_capnp::message::Reader>()?;
    let answer = expect_return(&root, question)?;
    match answer.reborrow().which()? {
        rpc_capnp::return_::Results(r) => Ok(CallOutcome::Value(decode(r?)?)),
        rpc_capnp::return_::Exception(e) => Ok(CallOutcome::Exception(
            e?.get_reason()?.to_str()?.to_string(),
        )),
        _ => Ok(CallOutcome::UnexpectedKind),
    }
}

fn expect_return<'a>(
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

fn build_bootstrap(question: u32) -> Result<Vec<u8>> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut bs = root.init_bootstrap();
    bs.set_question_id(question);
    Ok(crate::io::serialize_message(&message))
}

fn build_call<F>(
    question: u32,
    import_identifier: u32,
    interface_identifier: u64,
    method_identifier: u16,
    fill_parameters: F,
) -> Result<Vec<u8>>
where
    F: FnOnce(&mut rpc_capnp::payload::Builder<'_>) -> Result<()>,
{
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut call = root.init_call();
    call.set_question_id(question);
    let mut target = call.reborrow().init_target();
    target.set_imported_cap(import_identifier);
    call.reborrow().set_interface_id(interface_identifier);
    call.reborrow().set_method_id(method_identifier);
    call.reborrow().init_send_results_to().set_caller(());
    let mut payload = call.reborrow().init_params();
    fill_parameters(&mut payload)?;
    Ok(crate::io::serialize_message(&message))
}

fn build_finish(question: u32, release_result_caps: bool) -> Result<Vec<u8>> {
    let mut finish = capnp::message::Builder::new_default();
    let froot = finish.init_root::<rpc_capnp::message::Builder>();
    let mut f = froot.init_finish();
    f.set_question_id(question);
    f.set_release_result_caps(release_result_caps);
    Ok(crate::io::serialize_message(&finish))
}

fn build_release(identifier: u32, reference_count: u32) -> Result<Vec<u8>> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut rel = root.init_release();
    rel.set_id(identifier);
    rel.set_reference_count(reference_count);
    Ok(crate::io::serialize_message(&message))
}

/// Replies to a call with an `unimplemented` exception, mirroring what
/// capnp-go sends when a server does not implement a method.
pub async fn send_exception<S: AsyncStream + Unpin>(
    stream: &mut S,
    question_identifier: u32,
    reason: &str,
) -> Result<()> {
    let bytes = build_exception(question_identifier, reason)?;
    crate::io::write_raw(stream, &bytes).await
}

/// Builds the framed bytes for an `unimplemented` exception return,
/// mirroring what capnp-go sends when a server does not implement a method.
pub(crate) fn build_exception(question_identifier: u32, reason: &str) -> Result<Vec<u8>> {
    let mut message = capnp::message::Builder::new_default();
    let root = message.init_root::<rpc_capnp::message::Builder>();
    let mut ret = root.init_return();
    ret.set_answer_id(question_identifier);
    let mut exc = ret.init_exception();
    exc.set_reason(reason);
    exc.set_type(rpc_capnp::exception::Type::Unimplemented);
    Ok(crate::io::serialize_message(&message))
}

/// A summary of an incoming RPC message, sufficient for a server to dispatch.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A bootstrap request for the peer's main interface.
    Bootstrap {
        /// The question id to answer.
        question_identifier: u32,
    },
    /// A method call.
    Call {
        /// The question id to answer.
        question_identifier: u32,
        /// The called interface.
        interface_identifier: u64,
        /// The called method.
        method_identifier: u16,
    },
    /// A finish for a resolved question.
    Finish {
        /// The question id being released.
        question_identifier: u32,
    },
    /// A capability release.
    Release,
    /// Any other message kind (kept so the enum is exhaustive).
    Other,
}

/// Reads and classifies the next RPC message on a stream without exposing
/// Cap'n Proto types. Returns `None` when the stream ends.
pub async fn read_incoming<S: AsyncStream + Unpin>(stream: &mut S) -> Result<Option<Incoming>> {
    let reader = match read_message(stream).await {
        Ok(r) => r,
        Err(RpcError::Eof) => return Ok(None),
        Err(e) => return Err(e),
    };
    let root = reader.get_root::<rpc_capnp::message::Reader>()?;
    match root.reborrow().which()? {
        rpc_capnp::message::Bootstrap(b) => Ok(Some(Incoming::Bootstrap {
            question_identifier: b?.get_question_id(),
        })),
        rpc_capnp::message::Call(c) => {
            let c = c?;
            Ok(Some(Incoming::Call {
                question_identifier: c.reborrow().get_question_id(),
                interface_identifier: c.reborrow().get_interface_id(),
                method_identifier: c.reborrow().get_method_id(),
            }))
        }
        rpc_capnp::message::Finish(f) => Ok(Some(Incoming::Finish {
            question_identifier: f?.get_question_id(),
        })),
        rpc_capnp::message::Release(_) => Ok(Some(Incoming::Release)),
        _ => Ok(Some(Incoming::Other)),
    }
}
