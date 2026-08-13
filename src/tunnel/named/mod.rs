//! Named tunnel identity loaded from a cloudflared credentials file.

use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::parse_tunnel_identifier;

/// A named tunnel loaded from a credentials file.
///
/// The Serde layout matches cloudflared's credentials file exactly:
/// `AccountTag`, `TunnelSecret` (standard base64) and `TunnelID` (a UUID
/// string), with an optional `Endpoint` region override.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedTunnel {
    /// The account tag that owns the tunnel.
    #[serde(rename = "AccountTag")]
    pub account_tag: String,
    /// The registration secret (opaque bytes; never logged).
    #[serde(rename = "TunnelSecret", with = "crate::tunnel::secret")]
    pub tunnel_secret: Vec<u8>,
    /// The tunnel id as a UUID string.
    #[serde(rename = "TunnelID")]
    pub tunnel_identifier: String,
    /// An optional edge region override stored in the credentials.
    #[serde(rename = "Endpoint", default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl NamedTunnel {
    /// Loads tunnel credentials from a cloudflared credentials file.
    pub fn from_credentials_file(path: impl AsRef<Path>) -> Result<NamedTunnel> {
        let bytes = std::fs::read(path.as_ref()).map_err(|e| {
            Error::named_tunnel_credentials(format!("failed to read credentials file: {e}"))
        })?;
        let tunnel: NamedTunnel = serde_json::from_slice(&bytes)
            .map_err(|e| Error::named_tunnel_credentials(e.to_string()))?;
        if tunnel.tunnel_identifier.is_empty() {
            return Err(Error::named_tunnel_credentials(
                "credentials file has no TunnelID",
            ));
        }
        Ok(tunnel)
    }

    /// Parses a Cloudflare dashboard connector token into tunnel
    /// credentials.
    ///
    /// The token is what the Zero Trust dashboard shows for
    /// `cloudflared tunnel run --token`: a standard-base64 JSON payload with
    /// compact keys `a` (account tag), `s` (secret, base64) and `t` (tunnel
    /// id), plus an optional `e` endpoint, matching cloudflared's
    /// `connection.TunnelToken` layout. The secret is never logged.
    pub fn from_token(token: &str) -> Result<NamedTunnel> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(token)
            .map_err(|e| {
                Error::named_tunnel_credentials(format!("token is not valid base64: {e}"))
            })?;
        let payload: TokenPayload = serde_json::from_slice(&raw).map_err(|e| {
            Error::named_tunnel_credentials(format!("token payload is not valid JSON: {e}"))
        })?;
        let secret = base64::engine::general_purpose::STANDARD
            .decode(&payload.secret)
            .map_err(|e| {
                Error::named_tunnel_credentials(format!("token secret is not valid base64: {e}"))
            })?;
        let tunnel = NamedTunnel {
            account_tag: payload.account_tag,
            tunnel_secret: secret,
            tunnel_identifier: payload.tunnel_identifier,
            endpoint: payload.endpoint,
        };
        if tunnel.account_tag.is_empty() || tunnel.tunnel_secret.is_empty() {
            return Err(Error::named_tunnel_credentials(
                "token payload is missing the account tag or secret",
            ));
        }
        parse_tunnel_identifier(&tunnel.tunnel_identifier)?;
        Ok(tunnel)
    }

    /// The 16-byte tunnel id.
    pub fn tunnel_identifier_bytes(&self) -> Result<[u8; 16]> {
        parse_tunnel_identifier(&self.tunnel_identifier)
    }
}

/// The compact JSON shape of a dashboard connector token.
#[derive(Deserialize)]
struct TokenPayload {
    #[serde(rename = "a")]
    account_tag: String,
    #[serde(rename = "s")]
    secret: String,
    #[serde(rename = "t")]
    tunnel_identifier: String,
    #[serde(rename = "e", default)]
    endpoint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_tunnel_credentials_round_trip() {
        let tunnel = NamedTunnel {
            account_tag: "abc123".into(),
            tunnel_secret: b"top-secret".to_vec(),
            tunnel_identifier: "550e8400-e29b-41d4-a716-446655440000".into(),
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
        assert_eq!(
            back.tunnel_identifier,
            "550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn named_tunnel_token_parses_compact_payload() {
        let payload = br#"{"a":"abc123","s":"dG9wLXNlY3JldA==","t":"550e8400-e29b-41d4-a716-446655440000","e":"us-east-1"}"#;
        let token = base64::engine::general_purpose::STANDARD.encode(payload);
        let tunnel = NamedTunnel::from_token(&token).unwrap();
        assert_eq!(tunnel.account_tag, "abc123");
        assert_eq!(tunnel.tunnel_secret, b"top-secret");
        assert_eq!(
            tunnel.tunnel_identifier,
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(tunnel.endpoint.as_deref(), Some("us-east-1"));
        assert_eq!(tunnel.tunnel_identifier_bytes().unwrap().len(), 16);
    }

    #[test]
    fn named_tunnel_token_endpoint_is_optional() {
        let payload =
            br#"{"a":"abc123","s":"dG9wLXNlY3JldA==","t":"550e8400-e29b-41d4-a716-446655440000"}"#;
        let token = base64::engine::general_purpose::STANDARD.encode(payload);
        let tunnel = NamedTunnel::from_token(&token).unwrap();
        assert_eq!(tunnel.account_tag, "abc123");
        assert_eq!(tunnel.tunnel_secret, b"top-secret");
        assert_eq!(tunnel.endpoint, None);
    }

    #[test]
    fn named_tunnel_token_rejects_malformed_input() {
        assert!(NamedTunnel::from_token("not base64!").is_err());
        let payload = br#"{"a":"abc123","s":"dG9wLXNlY3JldA==","t":"not-a-uuid"}"#;
        let token = base64::engine::general_purpose::STANDARD.encode(payload);
        assert!(NamedTunnel::from_token(&token).is_err());
        let payload = br#"{"a":"","s":"","t":"550e8400-e29b-41d4-a716-446655440000"}"#;
        let token = base64::engine::general_purpose::STANDARD.encode(payload);
        assert!(NamedTunnel::from_token(&token).is_err());
    }
}
