//! Origin-side byte streams and the [`StreamOrigin`] trait.
//!
//! Origin outcomes form a hierarchy that mirrors the edge's connection
//! types. At the base, an [`HttpOrigin`](crate::HttpOrigin) responds with a
//! [`Response`]: status and headers followed by a one-way
//! body. A websocket upgrade is that response (a `101` handshake) followed
//! by a bidirectional [`Stream`] — [`WebSocketConnection`]. A raw TCP stream
//! drops the response entirely and hands back the [`Stream`] alone; the
//! transport owns the acknowledgement (a bare ack over QUIC, a synthesized
//! `101` over HTTP/2). The responder type a [`StreamOrigin`] is instantiated
//! with fixes which of these contracts it satisfies.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use crate::origin::http::body::{Request, Response};
use crate::origin::responder::StreamResponder;

/// A read-only half of a [`Stream`].
pub type ReadHalf = Pin<Box<dyn AsyncRead + Send>>;
/// A write-only half of a [`Stream`].
pub type WriteHalf = Pin<Box<dyn AsyncWrite + Send>>;

/// A raw bidirectional byte stream between the edge and an origin handler.
///
/// Used for websocket and TCP connections once the transport switches to
/// raw streaming. The halves are runtime-agnostic (`futures_io` traits);
/// consumers typically split their own socket and pass the halves here.
pub struct Stream {
    read: ReadHalf,
    write: WriteHalf,
}

impl Stream {
    /// Builds a stream from separate read and write halves.
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

    /// Builds a stream from a single bidirectional I/O object, splitting it
    /// into read and write halves internally.
    pub fn from_io<S>(io: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (read, write) = io.split();
        Self {
            read: Box::pin(read),
            write: Box::pin(write),
        }
    }

    /// Splits the stream back into its read and write halves.
    pub fn into_parts(self) -> (ReadHalf, WriteHalf) {
        (self.read, self.write)
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        self.read.as_mut().poll_read(cx, buffer)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.write.as_mut().poll_write(cx, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.write.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.write.as_mut().poll_close(cx)
    }
}

/// An upgrade accepted by a [`StreamOrigin<WebSocketResponder>`](crate::StreamOrigin): the
/// response headers to send to the edge and the origin-side byte stream to
/// pump.
pub struct WebSocketConnection {
    /// The response headers to send to the edge (e.g. `101` with
    /// `Sec-WebSocket-Accept`).
    pub response: Response,
    /// The origin-side byte stream to pump with the edge.
    pub origin: Stream,
}

/// Handles websocket upgrades and raw TCP streams from the edge.
///
/// The responder type fixes the contract. A
/// [`StreamOrigin<WebSocketResponder>`](crate::StreamOrigin) answers the
/// origin-side handshake with a
/// [`WebSocketConnection`]: the 101 response
/// headers the edge should see plus the origin byte stream. A
/// [`StreamOrigin<TcpResponder>`](crate::StreamOrigin) hands back only the
/// byte stream ([`TcpResponder::stream`](crate::TcpResponder::stream)); the
/// transport owns the proxy acknowledgement.
///
/// `connect` is synchronous; consumers that need to await origin I/O spawn a
/// task that calls the responder when the work completes.
pub trait StreamOrigin<R: StreamResponder>: Send + Sync {
    /// Runs the origin-side handshake or connection setup and writes the
    /// outcome into `respond`.
    fn connect(&self, request: Request, respond: R);
}
