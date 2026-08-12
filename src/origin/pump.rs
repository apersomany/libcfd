//! Bidirectional byte pumping and websocket handshake helpers.

use futures_util::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::Result;
use crate::origin::duplex::Duplex;

/// Pumps bytes in both directions between an origin duplex and the edge
/// stream until both directions reach the end.
///
/// Mirrors cloudflared's `PipeBidirectional`: each direction closes only
/// its own destination write side when the source ends, and the other
/// direction keeps pumping until it ends as well.
#[cfg_attr(not(edge_conn), allow(dead_code))]
pub(crate) async fn pump<R, W>(origin: Duplex, edge_read: R, edge_write: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (mut origin_read, mut origin_write) = origin.into_parts();
    let mut edge_read = edge_read;
    let mut edge_write = edge_write;
    let mut edge_done = false;
    let mut origin_done = false;
    let mut e_buf = [0u8; 8192];
    let mut o_buf = [0u8; 8192];
    loop {
        if edge_done && origin_done {
            break;
        }
        tokio::select! {
            read = edge_read.read(&mut e_buf), if !edge_done => {
                match read {
                    Ok(0) => {
                        edge_done = true;
                        let _ = origin_write.close().await;
                    }
                    Ok(n) => {
                        if let Err(e) = origin_write.write_all(&e_buf[..n]).await {
                            tracing::debug!("origin write failed: {e}");
                            edge_done = true;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("edge read failed: {e}");
                        edge_done = true;
                    }
                }
            }
            read = origin_read.read(&mut o_buf), if !origin_done => {
                match read {
                    Ok(0) => {
                        origin_done = true;
                        let _ = edge_write.close().await;
                    }
                    Ok(n) => {
                        if let Err(e) = edge_write.write_all(&o_buf[..n]).await {
                            tracing::debug!("edge write failed: {e}");
                            origin_done = true;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("origin read failed: {e}");
                        origin_done = true;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Computes the RFC 6455 `Sec-WebSocket-Accept` value for a challenge key.
///
/// Consumers implementing [`WebSocketOrigin`](crate::WebSocketOrigin) use
/// this to answer the handshake in their `connect` method.
#[cfg_attr(not(h2_any), allow(dead_code))]
pub fn websocket_accept(challenge_key: &str) -> String {
    use base64::Engine as _;
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(challenge_key.as_bytes());
    hasher.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}
