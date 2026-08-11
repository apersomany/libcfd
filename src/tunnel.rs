//! Tunnel identities: quick tunnels and named tunnels.

#[cfg(feature = "named-tunnel")]
use std::path::Path;
#[cfg(feature = "quick-tunnel")]
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[cfg(feature = "quick-tunnel")]
use crate::api;
use crate::error::{Error, Result};

/// Default quick tunnel service (trycloudflare.com).
#[cfg(feature = "quick-tunnel")]
pub const DEFAULT_QUICK_SERVICE_URL: &str = "https://api.trycloudflare.com";

/// A tunnel identity, containing everything an edge connection needs to
/// register: the account tag, the tunnel id, and the tunnel secret.
///
/// Quick tunnels are created through the HTTP API and carry a public
/// hostname; named tunnels are provisioned by an administrator and loaded
/// from a credentials file or token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tunnel {
    /// A tunnel created through the quick tunnel HTTP API.
    #[cfg(feature = "quick-tunnel")]
    Quick(QuickTunnel),
    /// A tunnel loaded from a cloudflared credentials file.
    #[cfg(feature = "named-tunnel")]
    Named(NamedTunnel),
}

impl Tunnel {
    /// Wraps a quick tunnel.
    #[cfg(feature = "quick-tunnel")]
    pub fn quick(tunnel: QuickTunnel) -> Self {
        Self::Quick(tunnel)
    }

    /// Wraps a named tunnel.
    #[cfg(feature = "named-tunnel")]
    pub fn named(tunnel: NamedTunnel) -> Self {
        Self::Named(tunnel)
    }

    /// The account tag that owns the tunnel.
    pub fn account_tag(&self) -> &str {
        match self {
            #[cfg(feature = "quick-tunnel")]
            Self::Quick(t) => &t.account_tag,
            #[cfg(feature = "named-tunnel")]
            Self::Named(t) => &t.account_tag,
        }
    }

    /// The tunnel secret used to prove ownership at registration.
    pub fn tunnel_secret(&self) -> &[u8] {
        match self {
            #[cfg(feature = "quick-tunnel")]
            Self::Quick(t) => &t.secret,
            #[cfg(feature = "named-tunnel")]
            Self::Named(t) => &t.tunnel_secret,
        }
    }

    /// The tunnel id as a UUID string.
    pub fn tunnel_id(&self) -> &str {
        match self {
            #[cfg(feature = "quick-tunnel")]
            Self::Quick(t) => &t.tunnel_id,
            #[cfg(feature = "named-tunnel")]
            Self::Named(t) => &t.tunnel_id,
        }
    }

    /// The 16-byte tunnel id.
    pub fn tunnel_id_bytes(&self) -> Result<[u8; 16]> {
        parse_tunnel_id(self.tunnel_id())
    }

    /// The public hostname of a quick tunnel, when this is one.
    #[cfg(feature = "quick-tunnel")]
    pub fn hostname(&self) -> Option<&str> {
        match self {
            Self::Quick(t) => Some(&t.hostname),
            #[cfg(feature = "named-tunnel")]
            Self::Named(_) => None,
        }
    }

    /// The region override carried by the tunnel credentials, if any
    /// (named tunnels can pin an `Endpoint` that acts as the region).
    #[cfg_attr(not(any(feature = "quic-edge", feature = "h2-edge")), allow(dead_code))]
    pub(crate) fn region_override(&self) -> Option<String> {
        match self {
            #[cfg(feature = "named-tunnel")]
            Self::Named(t) => t.endpoint.clone(),
            #[cfg(feature = "quick-tunnel")]
            Self::Quick(_) => None,
        }
    }
}

/// Options for [`create_quick_tunnel`].
#[cfg(feature = "quick-tunnel")]
#[derive(Debug, Clone)]
pub struct QuickTunnelOptions {
    /// Base URL of the quick tunnel service. Defaults to
    /// `DEFAULT_QUICK_SERVICE_URL`.
    pub service_url: String,
    /// HTTP timeout for the creation request.
    pub http_timeout: Duration,
}

#[cfg(feature = "quick-tunnel")]
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
#[cfg(feature = "quick-tunnel")]
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
    #[serde(with = "secret_codec")]
    pub secret: Vec<u8>,
}

#[cfg(feature = "quick-tunnel")]
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

/// A named tunnel loaded from a credentials file.
///
/// The Serde layout matches cloudflared's credentials file exactly:
/// `AccountTag`, `TunnelSecret` (standard base64) and `TunnelID` (a UUID
/// string), with an optional `Endpoint` region override.
#[cfg(feature = "named-tunnel")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedTunnel {
    /// The account tag that owns the tunnel.
    #[serde(rename = "AccountTag")]
    pub account_tag: String,
    /// The registration secret (opaque bytes; never logged).
    #[serde(rename = "TunnelSecret", with = "secret_codec")]
    pub tunnel_secret: Vec<u8>,
    /// The tunnel id as a UUID string.
    #[serde(rename = "TunnelID")]
    pub tunnel_id: String,
    /// An optional edge region override stored in the credentials.
    #[serde(rename = "Endpoint", default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

#[cfg(feature = "named-tunnel")]
impl NamedTunnel {
    /// Loads tunnel credentials from a cloudflared credentials file.
    pub fn from_credentials_file(path: impl AsRef<Path>) -> Result<NamedTunnel> {
        let bytes = std::fs::read(path.as_ref())?;
        let tunnel: NamedTunnel = serde_json::from_slice(&bytes)
            .map_err(|e| Error::NamedTunnelCredentials(e.to_string()))?;
        if tunnel.tunnel_id.is_empty() {
            return Err(Error::NamedTunnelCredentials(
                "credentials file has no TunnelID".into(),
            ));
        }
        Ok(tunnel)
    }

    /// The 16-byte tunnel id.
    pub fn tunnel_id_bytes(&self) -> Result<[u8; 16]> {
        parse_tunnel_id(&self.tunnel_id)
    }
}

fn parse_tunnel_id(id: &str) -> Result<[u8; 16]> {
    let uuid = uuid::Uuid::parse_str(id)
        .map_err(|e| Error::NamedTunnelCredentials(format!("bad tunnel id: {e}")))?;
    Ok(*uuid.as_bytes())
}

/// Serializes the tunnel secret as base64, matching Go's `[]byte` JSON
/// encoding that the quick tunnel service and credentials files use.
mod secret_codec {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(secret: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(secret))
    }
}

#[cfg(feature = "quick-tunnel")]
#[derive(Debug, Deserialize)]
struct QuickTunnelResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    result: Option<QuickTunnelResult>,
    #[serde(default)]
    errors: Vec<QuickTunnelError>,
}

#[cfg(feature = "quick-tunnel")]
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
    #[serde(with = "secret_codec")]
    secret: Vec<u8>,
}

#[cfg(feature = "quick-tunnel")]
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
#[cfg(feature = "quick-tunnel")]
pub async fn create_quick_tunnel(options: &QuickTunnelOptions) -> Result<QuickTunnel> {
    let url = format!("{}/tunnel", options.service_url.trim_end_matches('/'));
    let headers = vec![(
        http::header::USER_AGENT,
        format!("cloudflared/{}", env!("CARGO_PKG_VERSION")),
    )];
    let (status, body) = api::post_empty(&url, &headers, options.http_timeout).await?;
    if status >= 300 {
        let message = String::from_utf8_lossy(&body);
        return Err(Error::QuickTunnelApi(format!(
            "service returned status {status}: {message}"
        )));
    }
    let data: QuickTunnelResponse =
        serde_json::from_slice(&body).map_err(|e| Error::QuickTunnelResponse(e.to_string()))?;
    if !data.success {
        let message = data
            .errors
            .first()
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown error".into());
        return Err(Error::QuickTunnelApi(message));
    }
    let result = data
        .result
        .ok_or_else(|| Error::QuickTunnelResponse("response has no result".into()))?;
    if result.id.is_empty() || result.hostname.is_empty() {
        return Err(Error::QuickTunnelResponse(
            "response result is missing id or hostname".into(),
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

    #[cfg(feature = "quick-tunnel")]
    fn quick() -> QuickTunnel {
        QuickTunnel {
            tunnel_id: "6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f".into(),
            name: String::new(),
            hostname: "abc.trycloudflare.com".into(),
            account_tag: "tag".into(),
            secret: b"secret".to_vec(),
        }
    }

    #[cfg(feature = "quick-tunnel")]
    #[test]
    fn quick_tunnel_url_prepends_scheme() {
        let t = quick();
        assert_eq!(t.url(), "https://abc.trycloudflare.com");
    }

    #[cfg(feature = "quick-tunnel")]
    #[test]
    fn quick_tunnel_url_keeps_scheme() {
        let mut t = quick();
        t.hostname = "https://abc.trycloudflare.com".into();
        assert_eq!(t.url(), "https://abc.trycloudflare.com");
    }

    #[cfg(feature = "quick-tunnel")]
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

    #[cfg(feature = "quick-tunnel")]
    #[test]
    fn parses_api_error() {
        let body = br#"{"success":false,"errors":[{"code":10000,"message":"nope"}]}"#;
        let data: QuickTunnelResponse = serde_json::from_slice(body).unwrap();
        assert!(!data.success);
        assert_eq!(data.errors[0].message, "nope");
    }

    #[cfg(feature = "named-tunnel")]
    #[test]
    fn named_tunnel_credentials_round_trip() {
        let tunnel = NamedTunnel {
            account_tag: "abc123".into(),
            tunnel_secret: b"top-secret".to_vec(),
            tunnel_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            endpoint: None,
        };
        let json = serde_json::to_value(&tunnel).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "AccountTag": "abc123",
                "TunnelSecret": "dG9wLXNlY3JldA==",
                "TunnelID": "550e8400-e29b-41d4-a716-446655440000",
            })
        );
        let back: NamedTunnel = serde_json::from_value(json).unwrap();
        assert_eq!(back.account_tag, "abc123");
        assert_eq!(back.tunnel_secret, b"top-secret");
        assert_eq!(back.tunnel_id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[cfg(feature = "quick-tunnel")]
    #[test]
    fn tunnel_enum_round_trip() {
        let tunnel = Tunnel::quick(quick());
        let json = serde_json::to_value(&tunnel).unwrap();
        let back: Tunnel = serde_json::from_value(json).unwrap();
        assert_eq!(back.account_tag(), "tag");
        assert_eq!(back.tunnel_secret(), b"secret");
        assert_eq!(back.tunnel_id(), "6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f");
    }

    #[cfg(feature = "quick-tunnel")]
    #[test]
    fn tunnel_id_bytes_parse() {
        let tunnel = Tunnel::quick(quick());
        assert_eq!(tunnel.tunnel_id_bytes().unwrap().len(), 16);
    }
}
