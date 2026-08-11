use core::future::poll_fn;
use core::pin::Pin;

use futures_io::{AsyncRead, AsyncWrite};

use crate::error::{Result, RpcError};

/// A bidirectional byte stream capable of carrying RPC messages.
///
/// This mirrors the transport cloudflared uses for its registration stream:
/// a single full-duplex stream (QUIC stream or HTTP/2 request/response body).
pub trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T> AsyncStream for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

/// Reads one Cap'n Proto message (including its segment table) from the stream.
///
/// Framing: `[u32 LE (numSegments-1)] [per segment: u32 LE word count]
/// [zero u32 pad to 8-byte alignment] [segment data]`.
pub async fn read_message<S: AsyncStream + Unpin>(
    stream: &mut S,
) -> Result<capnp::message::Reader<capnp::serialize::OwnedSegments>> {
    let mut header = [0u8; 8];
    read_exact(stream, &mut header).await?;
    let segment_count =
        u32::from_le_bytes(header[0..4].try_into().expect("4 bytes")).wrapping_add(1) as usize;
    if segment_count == 0 || segment_count > 512 {
        return Err(RpcError::Protocol(format!(
            "invalid segment count {segment_count}"
        )));
    }

    let mut builder = capnp::serialize::SegmentLengthsBuilder::with_capacity(segment_count);
    builder
        .try_push_segment(u32::from_le_bytes(header[4..8].try_into().expect("4 bytes")) as usize)?;
    if segment_count > 1 {
        // Go: streamHeaderSize(maxSeg) = (4 + 4*numSegments + 7) & !7
        let header_size = (4 + 4 * segment_count + 7) & !7;
        let mut sizes = vec![0u8; header_size - 8];
        read_exact(stream, &mut sizes).await?;
        for i in 1..segment_count {
            let off = (i - 1) * 4;
            let len = u32::from_le_bytes(sizes[off..off + 4].try_into().expect("4 bytes")) as usize;
            builder.try_push_segment(len)?;
        }
    }

    let mut segments = builder.into_owned_segments();
    read_exact(stream, &mut segments[..]).await?;
    Ok(capnp::message::Reader::new(
        segments,
        capnp::message::ReaderOptions::new(),
    ))
}

/// Writes one Cap'n Proto message (including its segment table) to the stream.
pub async fn write_message<S: AsyncStream + Unpin>(
    stream: &mut S,
    message: &capnp::message::Builder<capnp::message::HeapAllocator>,
) -> Result<()> {
    let bytes = capnp::serialize::write_message_to_words(message);
    write_all(stream, &bytes).await?;
    Ok(())
}

/// Writes already-framed bytes (as produced by `serialize::write_message_to_words`)
/// to the stream.
pub async fn write_raw<S: AsyncStream + Unpin>(stream: &mut S, bytes: &[u8]) -> Result<()> {
    write_all(stream, bytes).await?;
    Ok(())
}

async fn read_exact<S: AsyncRead + Unpin>(stream: &mut S, mut buf: &mut [u8]) -> Result<()> {
    while !buf.is_empty() {
        let n = poll_fn(|cx| Pin::new(&mut *stream).poll_read(cx, buf)).await?;
        if n == 0 {
            return Err(RpcError::Eof);
        }
        let tmp = buf;
        buf = &mut tmp[n..];
    }
    Ok(())
}

async fn write_all<S: AsyncWrite + Unpin>(stream: &mut S, mut buf: &[u8]) -> Result<()> {
    while !buf.is_empty() {
        let n = poll_fn(|cx| Pin::new(&mut *stream).poll_write(cx, buf)).await?;
        if n == 0 {
            return Err(RpcError::Protocol("write returned zero bytes".into()));
        }
        buf = &buf[n..];
    }
    Ok(())
}
