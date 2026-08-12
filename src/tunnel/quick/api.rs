//! Minimal HTTPS client used for the quick tunnel HTTP API.
//!
//! Hand-rolled HTTP/1.1 over tokio-rustls so the crate does not need a full
//! HTTP client stack. Only used for `POST https://<api>/tunnel`.

use std::io;
use std::sync::Arc;

use http::header::HeaderName;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{Error, Result};

/// Sends a POST request with an empty body and returns the response body.
pub(crate) async fn post_empty(
    url: &str,
    headers: &[(HeaderName, String)],
    timeout: std::time::Duration,
) -> Result<(u16, Vec<u8>)> {
    let uri = http::Uri::try_from(url)
        .map_err(|e| Error::QuickTunnelResponse(format!("invalid api url: {e}")))?;
    let scheme = uri.scheme_str().unwrap_or("https");
    if scheme != "https" {
        return Err(Error::QuickTunnelResponse(format!(
            "unsupported scheme {scheme}"
        )));
    }
    let host = uri
        .host()
        .ok_or_else(|| Error::QuickTunnelResponse("missing host".into()))?;
    let port = uri.port_u16().unwrap_or(443);
    let path = if uri.path().is_empty() {
        "/"
    } else {
        uri.path()
    };
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| Error::QuickTunnelResponse(format!("invalid host: {e}")))?;

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store())
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let addr = tokio::net::lookup_host((host, port))
        .await
        .map_err(Error::QuickTunnelRequest)?
        .next()
        .ok_or_else(|| Error::QuickTunnelResponse(format!("could not resolve {host}:{port}")))?;

    let tcp = tokio::time::timeout(timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| Error::QuickTunnelApi("connection timed out".into()))?
        .map_err(Error::QuickTunnelRequest)?;

    let mut stream = tokio::time::timeout(timeout, connector.connect(server_name, tcp))
        .await
        .map_err(|_| Error::QuickTunnelApi("tls handshake timed out".into()))?
        .map_err(|e| Error::QuickTunnelRequest(io::Error::other(e)))?;

    let mut request = format!(
        "POST {path}{query} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n"
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(Error::QuickTunnelRequest)?;
    stream.flush().await.map_err(Error::QuickTunnelRequest)?;

    let mut buf = Vec::new();
    tokio::time::timeout(timeout, stream.read_to_end(&mut buf))
        .await
        .map_err(|_| Error::QuickTunnelApi("response timed out".into()))?
        .map_err(Error::QuickTunnelRequest)?;

    let (status, _headers, body) = parse_http_response(&buf)?;
    Ok((status, body))
}

fn root_store() -> rustls::RootCertStore {
    let mut store = rustls::RootCertStore::empty();
    store
        .roots
        .extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    store
}

/// Parses a minimal HTTP/1.1 response (status line, headers, body).
type HttpResponse = (u16, Vec<(String, String)>, Vec<u8>);

fn parse_http_response(bytes: &[u8]) -> Result<HttpResponse> {
    let header_end = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| Error::QuickTunnelResponse("missing response headers".into()))?;
    let head = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| Error::QuickTunnelResponse("response headers not utf-8".into()))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| Error::QuickTunnelResponse("empty status line".into()))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::QuickTunnelResponse("malformed status line".into()))?
        .parse()
        .map_err(|_| Error::QuickTunnelResponse("malformed status code".into()))?;

    let mut headers = Vec::new();
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.push((name.trim().to_string(), value.trim().to_string()));
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }

    let body = &bytes[header_end + 4..];
    let body = match content_length {
        Some(len) if len <= body.len() => body[..len].to_vec(),
        _ => body.to_vec(),
    };
    Ok((status, headers, body))
}

#[cfg(test)]
mod tests {
    use super::parse_http_response;

    #[test]
    fn parses_http_response_with_content_length() {
        let bytes =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 5\r\n\r\nhello";
        let (status, headers, body) = parse_http_response(bytes).unwrap();
        assert_eq!(status, 200);
        assert_eq!(
            headers[0],
            ("Content-Type".to_string(), "application/json".to_string())
        );
        assert_eq!(body, b"hello");
    }

    #[test]
    fn parses_http_response_without_content_length() {
        let bytes = b"HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\n\r\nboom";
        let (status, _headers, body) = parse_http_response(bytes).unwrap();
        assert_eq!(status, 500);
        assert_eq!(body, b"boom");
    }
}
