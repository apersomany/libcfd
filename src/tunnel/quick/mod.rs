//! Quick tunnel identity and the trycloudflare.com HTTP API client.

mod api;

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::parse_tunnel_id;

/// Default quick tunnel service (trycloudflare.com).
pub const DEFAULT_QUICK_SERVICE_URL: &str = "https://api.trycloudflare.com";

/// Options for [`create_quick_tunnel`].
#[derive(Debug, Clone)]
pub struct QuickTunnelOptions {
    /// Base URL of the quick tunnel service. Defaults to
    /// `DEFAULT_QUICK_SERVICE_URL`.
    pub service_url: String,
    /// HTTP timeout for the creation request.
    pub http_timeout: Duration,
}

impl Default for QuickTunnelOptions {
    fn default() -> Self {
        Self {
            service_url: DEFAULT_QUICK_SERVICE_URL.to_string(),
            http_timeout: Duration::from_secs(15),
        }
    }
}

/// A quick tunnel created through the HTTP API.
///
/// The hostname is the public URL; the account tag, tunnel id and secret are
/// the credentials used to register with the edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickTunnel {
    /// The tunnel id as a UUID string.
    pub tunnel_id: String,
    /// The tunnel name assigned by the service.
    pub name: String,
    /// The public hostname.
    pub hostname: String,
    /// The account tag that owns the tunnel.
    pub account_tag: String,
    /// The registration secret (opaque bytes; never logged).
    #[serde(with = "crate::tunnel::secret")]
    pub secret: Vec<u8>,
}

impl QuickTunnel {
    /// The public URL of the quick tunnel.
    pub fn url(&self) -> String {
        if self.hostname.starts_with("https://") {
            self.hostname.clone()
        } else {
            format!("https://{}", self.hostname)
        }
    }

    /// The 16-byte tunnel id, as parsed from the API response.
    pub fn tunnel_id_bytes(&self) -> Result<[u8; 16]> {
        parse_tunnel_id(&self.tunnel_id)
    }
}

#[derive(Debug, Deserialize)]
struct QuickTunnelResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    result: Option<QuickTunnelResult>,
    #[serde(default)]
    errors: Vec<QuickTunnelError>,
}

#[derive(Debug, Deserialize)]
struct QuickTunnelResult {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    account_tag: String,
    #[serde(default)]
    #[serde(with = "crate::tunnel::secret")]
    secret: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct QuickTunnelError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

/// Requests a new quick tunnel from the service.
///
/// This mirrors `cloudflared tunnel --url` with no account: the service
/// assigns the tunnel and returns its credentials.
pub async fn create_quick_tunnel(options: &QuickTunnelOptions) -> Result<QuickTunnel> {
    let url = format!("{}/tunnel", options.service_url.trim_end_matches('/'));
    let headers = vec![(
        http::header::USER_AGENT,
        format!("cloudflared/{}", env!("CARGO_PKG_VERSION")),
    )];
    let (status, body) = api::post_empty(&url, &headers, options.http_timeout).await?;
    if status >= 300 {
        let message = String::from_utf8_lossy(&body);
        return Err(Error::quick_tunnel_api(format!(
            "service returned status {status}: {message}"
        )));
    }
    let data: QuickTunnelResponse =
        serde_json::from_slice(&body).map_err(|e| Error::quick_tunnel_response(e.to_string()))?;
    if !data.success {
        let message = data
            .errors
            .first()
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown error".into());
        return Err(Error::quick_tunnel_api(message));
    }
    let result = data
        .result
        .ok_or_else(|| Error::quick_tunnel_response("response has no result"))?;
    if result.id.is_empty() || result.hostname.is_empty() {
        return Err(Error::quick_tunnel_response(
            "response result is missing id or hostname",
        ));
    }
    Ok(QuickTunnel {
        tunnel_id: result.id,
        name: result.name,
        hostname: result.hostname,
        account_tag: result.account_tag,
        secret: result.secret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick() -> QuickTunnel {
        QuickTunnel {
            tunnel_id: "6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f".into(),
            name: String::new(),
            hostname: "abc.trycloudflare.com".into(),
            account_tag: "tag".into(),
            secret: b"secret".to_vec(),
        }
    }

    #[test]
    fn quick_tunnel_url_prepends_scheme() {
        let t = quick();
        assert_eq!(t.url(), "https://abc.trycloudflare.com");
    }

    #[test]
    fn quick_tunnel_url_keeps_scheme() {
        let mut t = quick();
        t.hostname = "https://abc.trycloudflare.com".into();
        assert_eq!(t.url(), "https://abc.trycloudflare.com");
    }

    #[test]
    fn parses_api_response() {
        let body = br#"{"success":true,"result":{"id":"6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f","name":"","hostname":"random.trycloudflare.com","account_tag":"abc123","secret":"c2VjcmV0"}}"#;
        let data: QuickTunnelResponse = serde_json::from_slice(body).unwrap();
        assert!(data.success);
        let r = data.result.unwrap();
        assert_eq!(r.hostname, "random.trycloudflare.com");
        assert_eq!(r.account_tag, "abc123");
        assert_eq!(r.secret, b"secret");
    }

    #[test]
    fn parses_api_error() {
        let body = br#"{"success":false,"errors":[{"code":10000,"message":"nope"}]}"#;
        let data: QuickTunnelResponse = serde_json::from_slice(body).unwrap();
        assert!(!data.success);
        assert_eq!(data.errors[0].message, "nope");
    }
}
