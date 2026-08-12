//! Async stream adapter over a quiche stream.
//!
//! Provides `futures_io::AsyncRead`/`AsyncWrite` over a single QUIC stream
//! so the RPC client and request serving code can treat it like any byte
//! stream. All access to the quiche connection is serialized through the
//! shared `Inner` mutex; blocked readers/writers register their wakers there
//! and the driver task wakes them as data or flow-control credit arrives.
//!
//! quiche 0.29 semantics used here: `stream_recv` returns `Err(Done)` when no
//! data is currently available, `Ok((0, true))` when the peer's FIN has been
//! consumed, and `Err(InvalidStreamState)` when the stream no longer exists.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;

use super::Inner;

pub(crate) struct QuicStream {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    stream_id: u64,
    read_eof: bool,
}

impl QuicStream {
    pub(crate) fn new(inner: Arc<Mutex<Inner>>, notify: Arc<Notify>, stream_id: u64) -> Self {
        Self {
            inner,
            notify,
            stream_id,
            read_eof: false,
        }
    }

    /// Sends a FIN for the write side.
    pub(crate) fn finish(&self) {
        let mut g = self.inner.lock().unwrap();
        if !g.closed {
            let _ = g.conn.stream_send(self.stream_id, &[], true);
        }
        drop(g);
        self.notify.notify_waiters();
    }
}

fn closed_error() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionReset, "quic connection closed")
}

fn stream_error(e: quiche::Error) -> io::Error {
    match e {
        quiche::Error::InvalidStreamState(_) => {
            io::Error::new(io::ErrorKind::ConnectionAborted, "stream state error")
        }
        other => io::Error::other(other.to_string()),
    }
}

impl AsyncRead for QuicStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        {
            let mut g = this.inner.lock().unwrap();
            if g.closed {
                return Poll::Ready(Err(closed_error()));
            }
            if this.read_eof {
                return Poll::Ready(Ok(0));
            }
            match g.conn.stream_recv(this.stream_id, buf) {
                Ok((0, true)) => {
                    this.read_eof = true;
                    drop(g);
                    this.notify.notify_waiters();
                    return Poll::Ready(Ok(0));
                }
                Ok((n, fin)) => {
                    if fin {
                        this.read_eof = true;
                    }
                    drop(g);
                    this.notify.notify_waiters();
                    return Poll::Ready(Ok(n));
                }
                Err(quiche::Error::Done) => {}
                Err(quiche::Error::InvalidStreamState(_)) => {}
                Err(e) => return Poll::Ready(Err(stream_error(e))),
            }
            g.read_wakers.insert(this.stream_id, cx.waker().clone());
            // Re-check after registration to avoid a lost wakeup.
            match g.conn.stream_recv(this.stream_id, buf) {
                Ok((0, true)) => {
                    g.read_wakers.remove(&this.stream_id);
                    this.read_eof = true;
                    drop(g);
                    this.notify.notify_waiters();
                    return Poll::Ready(Ok(0));
                }
                Ok((n, fin)) => {
                    g.read_wakers.remove(&this.stream_id);
                    if fin {
                        this.read_eof = true;
                    }
                    drop(g);
                    this.notify.notify_waiters();
                    return Poll::Ready(Ok(n));
                }
                Err(quiche::Error::Done) => {}
                Err(quiche::Error::InvalidStreamState(_)) => {}
                Err(e) => {
                    g.read_wakers.remove(&this.stream_id);
                    return Poll::Ready(Err(stream_error(e)));
                }
            }
            drop(g);
            Poll::Pending
        }
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        {
            let mut g = this.inner.lock().unwrap();
            if g.closed {
                return Poll::Ready(Err(closed_error()));
            }
            match g.conn.stream_send(this.stream_id, buf, false) {
                Ok(n) => {
                    drop(g);
                    this.notify.notify_waiters();
                    return Poll::Ready(Ok(n));
                }
                Err(quiche::Error::Done) => {}
                Err(quiche::Error::InvalidStreamState(_)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "stream is not open",
                    )));
                }
                Err(e) => return Poll::Ready(Err(stream_error(e))),
            }
            // Register interest in flow-control capacity for this stream.
            let _ = g.conn.stream_writable(this.stream_id, buf.len());
            g.write_wakers.insert(this.stream_id, cx.waker().clone());
            drop(g);
            Poll::Pending
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.finish();
        Poll::Ready(Ok(()))
    }
}
