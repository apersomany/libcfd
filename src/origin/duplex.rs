//! Raw bidirectional byte streams between the edge and origin handlers.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::io::{AsyncRead, AsyncWrite};

use crate::origin::http::body::Response;

/// A read-only half of a [`Duplex`].
pub type ReadHalf = Pin<Box<dyn AsyncRead + Send>>;
/// A write-only half of a [`Duplex`].
pub type WriteHalf = Pin<Box<dyn AsyncWrite + Send>>;

/// A raw bidirectional byte stream between the edge and an origin handler.
///
/// Used for websocket and TCP connections once the transport switches to
/// raw streaming. The halves are runtime-agnostic (`futures_io` traits);
/// consumers typically split their own socket and pass the halves here.
pub struct Duplex {
    read: ReadHalf,
    write: WriteHalf,
}

impl Duplex {
    /// Builds a duplex from separate read and write halves.
    pub fn new<R, W>(read: R, write: W) -> Self
    where
        R: AsyncRead + Send + 'static,
        W: AsyncWrite + Send + 'static,
    {
        Self {
            read: Box::pin(read),
            write: Box::pin(write),
        }
    }

    /// Splits the duplex back into its read and write halves.
    pub fn into_parts(self) -> (ReadHalf, WriteHalf) {
        (self.read, self.write)
    }
}

impl AsyncRead for Duplex {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        self.read.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for Duplex {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.write.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.write.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.write.as_mut().poll_close(cx)
    }
}

/// An upgrade accepted by a [`WebSocketOrigin`](crate::WebSocketOrigin): the
/// response headers to send to the edge and the origin-side byte stream to
/// pump.
pub struct WebSocketConnection {
    /// The response headers to send to the edge (e.g. `101` with
    /// `Sec-WebSocket-Accept`).
    pub response: Response,
    /// The origin-side byte stream to pump with the edge.
    pub origin: Duplex,
}
