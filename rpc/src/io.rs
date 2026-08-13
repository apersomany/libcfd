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
///
/// Mirrors capnp-go's stream decoder including its `defaultDecodeLimit`
/// of 64 MiB of total segment data.
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
    let mut total_words = 0usize;
    let first = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes")) as usize;
    total_words = total_words.saturating_add(first);
    if total_words > MAXIMUM_TOTAL_WORDS {
        return Err(RpcError::Protocol(format!(
            "message too large: {total_words} words exceeds limit of {MAXIMUM_TOTAL_WORDS}"
        )));
    }
    builder.try_push_segment(first)?;
    if segment_count > 1 {
        // Go: streamHeaderSize(maxSeg) = (4 + 4*numSegments + 7) & !7
        let header_size = (4 + 4 * segment_count + 7) & !7;
        let mut sizes = vec![0u8; header_size - 8];
        read_exact(stream, &mut sizes).await?;
        for i in 1..segment_count {
            let offset = (i - 1) * 4;
            let length =
                u32::from_le_bytes(sizes[offset..offset + 4].try_into().expect("4 bytes")) as usize;
            total_words = total_words.saturating_add(length);
            if total_words > MAXIMUM_TOTAL_WORDS {
                return Err(RpcError::Protocol(format!(
                    "message too large: {total_words} words exceeds limit of {MAXIMUM_TOTAL_WORDS}"
                )));
            }
            builder.try_push_segment(length)?;
        }
    }

    let mut segments = builder.into_owned_segments();
    read_exact(stream, &mut segments[..]).await?;
    Ok(capnp::message::Reader::new(
        segments,
        capnp::message::ReaderOptions::new(),
    ))
}

/// capnp-go's `defaultDecodeLimit` is 64 MiB; as words (8 bytes each) with
/// one header word per segment this bounds a single message.
const MAXIMUM_TOTAL_WORDS: usize = 8 * 1024 * 1024;

/// Serializes a Cap'n Proto message (including its segment table) to framed
/// bytes. Synchronous so no non-`Send` capnp builder state is held across an
/// await; write the result with [`write_raw`].
pub fn serialize_message(
    message: &capnp::message::Builder<capnp::message::HeapAllocator>,
) -> Vec<u8> {
    capnp::serialize::write_message_to_words(message)
}

/// Writes already-framed bytes (as produced by `serialize::write_message_to_words`)
/// to the stream.
pub async fn write_raw<S: AsyncStream + Unpin>(stream: &mut S, bytes: &[u8]) -> Result<()> {
    write_all(stream, bytes).await?;
    Ok(())
}

async fn read_exact<S: AsyncRead + Unpin>(stream: &mut S, mut buffer: &mut [u8]) -> Result<()> {
    while !buffer.is_empty() {
        let n = poll_fn(|cx| Pin::new(&mut *stream).poll_read(cx, buffer)).await?;
        if n == 0 {
            return Err(RpcError::Eof);
        }
        let tmp = buffer;
        buffer = &mut tmp[n..];
    }
    Ok(())
}

async fn write_all<S: AsyncWrite + Unpin>(stream: &mut S, mut buffer: &[u8]) -> Result<()> {
    while !buffer.is_empty() {
        let n = poll_fn(|cx| Pin::new(&mut *stream).poll_write(cx, buffer)).await?;
        if n == 0 {
            return Err(RpcError::Protocol("write returned zero bytes".into()));
        }
        buffer = &buffer[n..];
    }
    Ok(())
}
