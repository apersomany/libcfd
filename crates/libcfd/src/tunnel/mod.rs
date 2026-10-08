//! Tunnel identities: quick tunnels and named tunnels.

mod error;
pub use error::Error;

pub(crate) mod secret;

#[cfg(feature = "named-tunnel")]
mod named;
#[cfg(feature = "quick-tunnel")]
mod quick;

use serde::{Deserialize, Serialize};

#[cfg(feature = "named-tunnel")]
pub use named::NamedTunnel;
#[cfg(feature = "quick-tunnel")]
pub use quick::{QuickTunnel, QuickTunnelOptions, create_quick_tunnel};

use crate::error::{Error as CrateError, Result};

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
    pub fn tunnel_identifier(&self) -> &str {
        match self {
            #[cfg(feature = "quick-tunnel")]
            Self::Quick(t) => &t.tunnel_identifier,
            #[cfg(feature = "named-tunnel")]
            Self::Named(t) => &t.tunnel_identifier,
        }
    }

    /// The 16-byte tunnel id.
    pub fn tunnel_identifier_bytes(&self) -> Result<[u8; 16]> {
        parse_tunnel_identifier(self.tunnel_identifier())
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
    #[cfg_attr(not(any_edge), allow(dead_code))]
    pub(crate) fn region_override(&self) -> Option<String> {
        match self {
            #[cfg(feature = "named-tunnel")]
            Self::Named(t) => t.endpoint.clone(),
            #[cfg(feature = "quick-tunnel")]
            Self::Quick(_) => None,
        }
    }
}

pub(crate) fn parse_tunnel_identifier(identifier: &str) -> Result<[u8; 16]> {
    let uuid = uuid::Uuid::parse_str(identifier)
        .map_err(|e| CrateError::invalid_tunnel_identifier(format!("{identifier:?}: {e}")))?;
    Ok(*uuid.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "quick-tunnel")]
    fn quick() -> QuickTunnel {
        QuickTunnel {
            tunnel_identifier: "6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f".into(),
            name: String::new(),
            hostname: "abc.trycloudflare.com".into(),
            account_tag: "tag".into(),
            secret: b"secret".to_vec(),
        }
    }

    #[cfg(feature = "quick-tunnel")]
    #[test]
    fn tunnel_quick_serializes_to_expected_shape() {
        let tunnel = Tunnel::quick(quick());
        let json = serde_json::to_value(&tunnel).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "quick",
                "tunnel_id": "6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f",
                "name": "",
                "hostname": "abc.trycloudflare.com",
                "account_tag": "tag",
                "secret": "c2VjcmV0",
            })
        );
    }

    #[cfg(feature = "quick-tunnel")]
    #[test]
    fn tunnel_identifier_bytes_parse() {
        let tunnel = Tunnel::quick(quick());
        assert_eq!(
            tunnel.tunnel_identifier_bytes().unwrap(),
            [
                0x6e, 0xa0, 0x5b, 0xa1, 0x9e, 0x0e, 0x4f, 0x0d, 0x9e, 0x9e, 0x3d, 0x0f, 0x0f, 0x0f,
                0x0f, 0x0f
            ]
        );
    }

    #[test]
    fn parse_tunnel_identifier_rejects_invalid_uuids() {
        assert!(parse_tunnel_identifier("").is_err());
        assert!(parse_tunnel_identifier("not-a-uuid").is_err());
        assert!(parse_tunnel_identifier("6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f").is_ok());
        assert!(parse_tunnel_identifier("6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f0").is_err());
    }
}
