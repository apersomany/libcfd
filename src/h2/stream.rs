//! Async adapters over HTTP/2 request and response streams.
//!
//! The edge opens HTTP/2 streams toward libcfd (libcfd is the HTTP/2
//! server). For websocket, TCP and control-stream traffic the stream body is
//! a raw bidirectional channel, which these adapters expose through the
//! `futures_io` traits so the RPC client and pumping code can treat it like
//! any byte stream.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_io::{AsyncRead, AsyncWrite};
use h2::RecvStream;

/// Reads the request body of an HTTP/2 stream.
///
/// Releases flow-control capacity as chunks are consumed so the edge can
/// keep sending.
pub(crate) struct RecvStreamReader {
    recv: RecvStream,
    chunk: Option<Bytes>,
    pos: usize,
    eof: bool,
}

impl RecvStreamReader {
    pub(crate) fn new(recv: RecvStream) -> Self {
        Self {
            recv,
            chunk: None,
            pos: 0,
            eof: false,
        }
    }
}

impl AsyncRead for RecvStreamReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            if self.eof {
                return Poll::Ready(Ok(0));
            }
            if let Some(chunk) = self.chunk.take() {
                if self.pos < chunk.len() {
                    let n = std::cmp::min(chunk.len() - self.pos, buf.len());
                    buf[..n].copy_from_slice(&chunk[self.pos..self.pos + n]);
                    self.pos += n;
                    if self.pos < chunk.len() {
                        self.chunk = Some(chunk);
                    } else {
                        let used = chunk.len();
                        if let Err(e) = self.recv.flow_control().release_capacity(used) {
                            return Poll::Ready(Err(io::Error::other(e)));
                        }
                        self.chunk = None;
                        self.pos = 0;
                    }
                    return Poll::Ready(Ok(n));
                }
                self.pos = 0;
            }
            match Pin::new(&mut self.recv).poll_data(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.chunk = Some(chunk);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(io::Error::other(e)));
                }
                Poll::Ready(None) => {
                    self.eof = true;
                    return Poll::Ready(Ok(0));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Writes to the response body of an HTTP/2 stream with backpressure.
///
/// `send_data` buffers without bound when capacity is not reserved, so
/// capacity is reserved first and `poll_capacity` gates each write.
pub(crate) struct SendStreamWriter {
    send: h2::SendStream<Bytes>,
    closed: bool,
}

impl SendStreamWriter {
    pub(crate) fn new(send: h2::SendStream<Bytes>) -> Self {
        Self {
            send,
            closed: false,
        }
    }
}

impl AsyncWrite for SendStreamWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stream already closed",
            )));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if self.send.capacity() == 0 {
            self.send.reserve_capacity(buf.len());
            match Pin::new(&mut self.send).poll_capacity(cx) {
                Poll::Ready(Some(Ok(_))) => {}
                Poll::Ready(None) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "stream reset by peer",
                    )));
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(io::Error::other(e)));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        let n = std::cmp::min(buf.len(), self.send.capacity());
        match self
            .send
            .send_data(Bytes::copy_from_slice(&buf[..n]), false)
        {
            Ok(()) => Poll::Ready(Ok(n)),
            Err(e) => Poll::Ready(Err(io::Error::other(e))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.closed {
            self.closed = true;
            let _ = self.send.send_data(Bytes::new(), true);
        }
        Poll::Ready(Ok(()))
    }
}

/// A bidirectional stream over an HTTP/2 request/response pair, used to
/// carry the registration RPC on the control-stream request.
pub(crate) struct H2Bidi {
    read: RecvStreamReader,
    write: SendStreamWriter,
}

impl H2Bidi {
    pub(crate) fn new(recv: RecvStream, send: h2::SendStream<Bytes>) -> Self {
        Self {
            read: RecvStreamReader::new(recv),
            write: SendStreamWriter::new(send),
        }
    }
}

impl AsyncRead for H2Bidi {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().read).poll_read(cx, buf)
    }
}

impl AsyncWrite for H2Bidi {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().write).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().write).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().write).poll_close(cx)
    }
}
