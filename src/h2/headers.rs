//! HTTP/2 edge header handling, mirroring cloudflared's
//! `connection/header.go` and `connection/http2.go`.
//!
//! User headers travel base64-serialized inside a single
//! `cf-cloudflared-response-headers` header so HTTP/2 header validation is
//! never applied to HTTP/1 values; `content-length` also passes through as a
//! real HTTP/2 header.

use base64::Engine as _;

use crate::origin::Response;

/// The header carrying base64-serialized origin user headers.
pub(crate) const RESPONSE_USER_HEADERS: &str = "cf-cloudflared-response-headers";
/// The response meta header (Go canonicalizes to `Cf-Cloudflared-Response-Meta`).
pub(crate) const RESPONSE_META_HEADER: &str = "cf-cloudflared-response-meta";
/// The meta value cloudflared sets when the response comes from the origin.
const RESPONSE_META_ORIGIN: &str = r#"{"src":"origin"}"#;
/// The internal upgrade header used to classify edge-initiated streams.
pub(crate) const INTERNAL_UPGRADE_HEADER: &str = "cf-cloudflared-proxy-connection-upgrade";
pub(crate) const INTERNAL_TCP_SRC_HEADER: &str = "cf-cloudflared-proxy-src";
pub(crate) const WEBSOCKET_UPGRADE: &str = "websocket";
pub(crate) const CONTROL_STREAM_UPGRADE: &str = "control-stream";
pub(crate) const CONFIGURATION_UPDATE: &str = "update-configuration";

const TRACING_INTERNAL_HEADER: &str = "cf-int-cloudflared-tracing";

/// Serializes HTTP/1 headers as `[base64(name):base64(value);]`, exactly like
/// cloudflared's `SerializeHeaders`.
pub(crate) fn serialize_headers(headers: &[(String, String)]) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(&encode_b64(name));
        out.push(':');
        out.push_str(&encode_b64(value));
    }
    out
}

/// Deserializes headers serialized by [`serialize_headers`].
#[cfg(test)]
pub(crate) fn deserialize_headers(serialized: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pair in serialized.split(';') {
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once(':') else {
            continue;
        };
        let Ok(name) = decode_b64(name) else {
            continue;
        };
        let Ok(value) = decode_b64(value) else {
            continue;
        };
        out.push((name, value));
    }
    out
}

fn encode_b64(input: &str) -> String {
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(input.as_bytes())
}

#[cfg(test)]
fn decode_b64(input: &str) -> Result<String, base64::DecodeError> {
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(input)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Builds the HTTP/2 response headers for an origin [`Response`], applying
/// cloudflared's header rules (content-length passthrough, user-header
/// serialization, response meta, 101 -> 200 remap).
pub(crate) fn encode_response_headers(response: &Response) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    let mut user_headers: Vec<(String, String)> = Vec::new();
    for (name, value) in response.headers.iter() {
        let value = value.to_str().unwrap_or("").to_string();
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "content-length" {
            headers.append(name.clone(), http::HeaderValue::from_str(&value).unwrap());
        }
        if lower == TRACING_INTERNAL_HEADER {
            headers.insert(
                "Cf-Int-Cloudflared-Tracing",
                http::HeaderValue::from_str(&value).unwrap(),
            );
            continue;
        }
        if !is_control_response_header(&lower) || is_websocket_client_header(&lower) {
            user_headers.push((name.as_str().to_string(), value));
        }
    }
    if !user_headers.is_empty() {
        headers.insert(
            RESPONSE_USER_HEADERS,
            http::HeaderValue::from_str(&serialize_headers(&user_headers)).unwrap(),
        );
    }
    headers.insert(
        RESPONSE_META_HEADER,
        http::HeaderValue::from_static(RESPONSE_META_ORIGIN),
    );
    headers
}

fn is_control_response_header(name: &str) -> bool {
    name.starts_with(':')
        || name.starts_with("cf-int-")
        || name.starts_with("cf-cloudflared-")
        || name.starts_with("cf-proxy-")
}

fn is_websocket_client_header(name: &str) -> bool {
    matches!(name, "sec-websocket-accept" | "connection" | "upgrade")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_and_deserializes_headers() {
        let headers = vec![
            ("Content-Type".to_string(), "text/plain".to_string()),
            ("X-Custom".to_string(), "a:b;c".to_string()),
        ];
        let serialized = serialize_headers(&headers);
        let back = deserialize_headers(&serialized);
        assert_eq!(back, headers);
    }

    #[test]
    fn empty_serialization_is_empty() {
        assert_eq!(serialize_headers(&[]), "");
        assert!(deserialize_headers("").is_empty());
    }

    #[test]
    fn response_headers_apply_cloudflared_rules() {
        let mut origin_headers = http::HeaderMap::new();
        origin_headers.insert("content-length", "5".parse().unwrap());
        origin_headers.insert("content-type", "text/plain".parse().unwrap());
        origin_headers.insert("upgrade", "websocket".parse().unwrap());
        let response = Response::new(
            http::StatusCode::SWITCHING_PROTOCOLS,
            origin_headers,
            crate::origin::Body::empty(),
        );
        let headers = encode_response_headers(&response);
        assert_eq!(
            headers.get("content-length").unwrap(),
            "5",
            "content-length passes through as a real header"
        );
        assert_eq!(
            headers.get(RESPONSE_META_HEADER).unwrap(),
            RESPONSE_META_ORIGIN
        );
        let serialized = headers
            .get(RESPONSE_USER_HEADERS)
            .unwrap()
            .to_str()
            .unwrap();
        let user = deserialize_headers(serialized);
        assert!(user.contains(&("content-length".to_string(), "5".to_string())));
        assert!(user.contains(&("content-type".to_string(), "text/plain".to_string())));
        assert!(user.contains(&("upgrade".to_string(), "websocket".to_string())));
    }
}
