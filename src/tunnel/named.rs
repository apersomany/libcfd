//! Named tunnel identity loaded from a cloudflared credentials file.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::parse_tunnel_id;

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
    pub tunnel_id: String,
    /// An optional edge region override stored in the credentials.
    #[serde(rename = "Endpoint", default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl NamedTunnel {
    /// Loads tunnel credentials from a cloudflared credentials file.
    pub fn from_credentials_file(path: impl AsRef<Path>) -> Result<NamedTunnel> {
        let bytes = std::fs::read(path.as_ref()).map_err(|e| {
            Error::NamedTunnelCredentials(format!("failed to read credentials file: {e}"))
        })?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
