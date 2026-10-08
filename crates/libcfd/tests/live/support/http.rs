//! A minimal HTTPS client for the live tests.
//!
//! TLS is verified against the public web PKI since tunnel hostnames serve
//! public certificates. Handles status lines, content-length bodies, and
//! chunked transfer encoding.

use std::sync::Arc;

use rustls_pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A parsed HTTPS response.
#[derive(Debug)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// Headers in order of appearance.
    pub headers: Vec<(String, String)>,
    /// The decoded body.
    pub body: Vec<u8>,
}

/// Establishes a TLS connection to `hostname:port` verified against the
/// public web PKI. The hostname is resolved with
/// [`super::state::resolve_host`] so a broken local resolver falls back to
/// Cloudflare DNS.
pub async fn tls_connect(
    hostname: &str,
    port: u16,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let server_name = ServerName::try_from(hostname.to_string())
        .map_err(|e| format!("invalid host {hostname:?}: {e}"))?;

    let mut store = rustls::RootCertStore::empty();
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let configuration = rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(configuration));

    let address = super::state::resolve_host(hostname)
        .await?
        .first()
        .copied()
        .ok_or_else(|| format!("no address for {hostname}"))?;
    let tcp = TcpStream::connect((address, port))
        .await
        .map_err(|e| format!("tcp connect {address}:{port}: {e}"))?;
    connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls handshake with {hostname}: {e}"))
}

/// Performs an HTTPS GET and returns the parsed response.
pub async fn https_get(url: &str) -> Result<HttpResponse, String> {
    let uri: http::Uri = url
        .parse()
        .map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let host = uri
        .host()
        .ok_or_else(|| format!("url {url:?} has no host"))?
        .to_string();
    let port = uri.port_u16().unwrap_or(443);
    let path = if uri.path().is_empty() {
        "/"
    } else {
        uri.path()
    };
    let mut stream = tls_connect(&host, port).await?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: libcfd-live-test\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write request: {e}"))?;
    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .await
        .map_err(|e| format!("read response: {e}"))?;
    parse_http_response(&buffer).map_err(|e| format!("{e}: {:?}", String::from_utf8_lossy(&buffer)))
}

/// Parses an HTTP/1.1 response into status, headers, and body, decoding
/// chunked transfer encoding.
fn parse_http_response(bytes: &[u8]) -> Result<HttpResponse, String> {
    let header_end = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "response has no header terminator".to_string())?;
    let head = std::str::from_utf8(&bytes[..header_end])
        .map_err(|e| format!("response head is not utf-8: {e}"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "empty status line".to_string())?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed status line {status_line:?}"))?
        .parse()
        .map_err(|e| format!("malformed status code: {e}"))?;

    let mut headers = Vec::new();
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            headers.push((name.to_string(), value.to_string()));
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse::<usize>().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
    }
    let raw = &bytes[header_end + 4..];
    let body = if chunked {
        decode_chunked(raw)?
    } else {
        match content_length {
            Some(len) if len <= raw.len() => raw[..len].to_vec(),
            _ => raw.to_vec(),
        }
    };
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// Decodes an HTTP/1.1 chunked body. The terminating `0\r\n\r\n` may or may
/// not still be in `data` when the size line is parsed.
fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "chunk size line unterminated".to_string())?;
        let size_str = std::str::from_utf8(&data[..line_end])
            .map_err(|e| format!("chunk size not utf-8: {e}"))?;
        let size_str = size_str.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|e| format!("bad chunk size {size_str:?}: {e}"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            break;
        }
        if data.len() < size + 2 {
            return Err("chunk body truncated".to_string());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_response() {
        let response = parse_http_response(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello",
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers[0],
            ("Content-Type".to_string(), "text/plain".to_string())
        );
        assert_eq!(response.body, b"hello");
    }

    #[test]
    fn parses_chunked_response() {
        let response = parse_http_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(response.body, b"hello world");
    }

    #[test]
    fn rejects_truncated_response() {
        assert!(parse_http_response(b"HTTP/1.1 200 OK\r\nX: y").is_err());
        assert!(parse_http_response(b"").is_err());
    }

    #[test]
    fn rejects_truncated_chunk() {
        assert!(decode_chunked(b"5\r\nhel").is_err());
        assert!(decode_chunked(b"zz\r\n").is_err());
    }
}
