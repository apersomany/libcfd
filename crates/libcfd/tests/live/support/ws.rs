//! Minimal websocket and raw TCP clients for the live tests.
//!
//! The websocket client performs the RFC 6455 handshake, sends one masked
//! text frame, and expects the tunnel's echo origin to bounce the bytes
//! back. Frames are opaque to the tunnel, so the echo arrives masked exactly
//! as sent and is unmasked client-side. The TCP client sends a probe line
//! over a plain socket and expects the echo origin to bounce it.

use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// Sends a masked text frame with `payload` and returns the first text
/// frame's payload read back from the server.
pub async fn websocket_echo_round_trip(hostname: &str, path: &str) -> Result<(), String> {
    let mut stream = super::http::tls_connect(hostname, 443).await?;

    let mut key_bytes = [0u8; 16];
    getrandom::fill(&mut key_bytes).map_err(|e| e.to_string())?;
    let key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {hostname}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("ws handshake write: {e}"))?;

    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = tokio::time::timeout(IO_TIMEOUT, stream.read(&mut byte))
            .await
            .map_err(|_| "ws handshake response timed out".to_string())?
            .map_err(|e| format!("ws handshake read: {e}"))?;
        if n == 0 {
            return Err("ws handshake: connection closed before 101".to_string());
        }
        head.push(byte[0]);
        if head.len() >= 4 && head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head =
        std::str::from_utf8(&head).map_err(|e| format!("ws handshake response not utf-8: {e}"))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| format!("ws handshake: malformed status line {head:?}"))?;
    if status != "101" {
        return Err(format!(
            "ws handshake: expected 101, got {status} ({head:?})"
        ));
    }
    let accept = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("sec-websocket-accept")
                .then_some(value.trim())
        })
        .ok_or_else(|| "ws handshake: missing sec-websocket-accept".to_string())?;
    let expected_accept = libcfd::websocket_accept(&key);
    if accept != expected_accept {
        return Err(format!(
            "ws handshake: accept {accept:?} does not match the origin's computed {expected_accept:?}"
        ));
    }

    let payload = b"hello";
    let mut mask = [0u8; 4];
    mask.copy_from_slice(&key_bytes[..4]);
    let mut frame = vec![0x81, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    for (i, byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[i % 4]);
    }
    stream
        .write_all(&frame)
        .await
        .map_err(|e| format!("ws frame write: {e}"))?;

    // The echo origin bounces the exact bytes it received: a masked frame.
    // Collect the full frame (2-byte header + 4-byte mask + payload) and
    // unmask the payload.
    let mut response = Vec::new();
    let mut buffer = [0u8; 256];
    let full_len = 2 + 4 + payload.len();
    loop {
        let n = tokio::time::timeout(IO_TIMEOUT, stream.read(&mut buffer))
            .await
            .map_err(|_| "ws echo timed out".to_string())?
            .map_err(|e| format!("ws frame read: {e}"))?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..n]);
        if response.len() >= full_len {
            break;
        }
    }
    if response.len() < full_len || response[0] & 0x0f != 1 {
        return Err(format!("ws echo: no text frame received ({response:?})"));
    }
    if (response[1] & 0x7f) as usize != payload.len() {
        return Err(format!("ws echo: unexpected frame length in {response:?}"));
    }
    let mut echoed = Vec::with_capacity(payload.len());
    for (i, byte) in response[6..full_len].iter().enumerate() {
        echoed.push(byte ^ mask[i % 4]);
    }
    if echoed != payload {
        return Err(format!(
            "ws echo: payload {echoed:?} does not match sent {payload:?}"
        ));
    }
    Ok(())
}

/// Connects to `hostname:port` over a plain socket, sends a probe line, and
/// expects the origin's TCP echo to bounce it back.
pub async fn tcp_echo_round_trip(hostname: &str, port: u16) -> Result<(), String> {
    let address = super::state::resolve_host(hostname)
        .await?
        .first()
        .copied()
        .ok_or_else(|| format!("no address for {hostname}"))?;
    let mut stream = tokio::net::TcpStream::connect((address, port))
        .await
        .map_err(|e| format!("tcp connect {address}:{port}: {e}"))?;

    let probe = b"hello-tcp";
    stream
        .write_all(probe)
        .await
        .map_err(|e| format!("tcp probe write: {e}"))?;
    let mut echoed = [0u8; 32];
    let n = tokio::time::timeout(IO_TIMEOUT, stream.read(&mut echoed))
        .await
        .map_err(|_| "tcp echo timed out".to_string())?
        .map_err(|e| format!("tcp echo read: {e}"))?;
    if &echoed[..n] != probe {
        return Err(format!(
            "tcp echo: got {:?}, expected {probe:?}",
            &echoed[..n]
        ));
    }
    Ok(())
}
