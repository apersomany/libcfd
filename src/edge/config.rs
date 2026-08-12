//! Remotely-managed tunnel configuration pushed by the edge.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use serde::Deserialize;

use libcfd_rpc::{CloudflaredHandler, UpdateConfigurationResponse};

/// The tunnel configuration the edge pushes for remotely-managed tunnels.
///
/// Only the fields libcfd understands are parsed: the ingress rules'
/// hostnames, i.e. the public hostnames routed to this tunnel (the
/// catch-all rule has no hostname). The rest of the payload is opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    /// The configuration version from the push.
    pub version: i32,
    /// Public hostnames routed to this tunnel.
    pub hostnames: Vec<String>,
}

/// The `CloudflaredHandler` used by both edge transports: applies config
/// pushes and forwards them to the consumer callback.
pub(crate) struct EdgeConfigHandler {
    on_config: Option<Arc<dyn Fn(RemoteConfig) + Send + Sync>>,
    applied: AtomicI32,
}

impl EdgeConfigHandler {
    pub(crate) fn new(on_config: Option<Arc<dyn Fn(RemoteConfig) + Send + Sync>>) -> Self {
        Self {
            on_config,
            applied: AtomicI32::new(-1),
        }
    }
}

impl CloudflaredHandler for EdgeConfigHandler {
    fn update_configuration(&self, version: i32, config: &[u8]) -> UpdateConfigurationResponse {
        match parse_remote_config(version, config) {
            Ok(remote) => {
                self.applied.store(version, Ordering::SeqCst);
                tracing::info!(version, hostnames = ?remote.hostnames, "edge pushed remote configuration");
                if let Some(on_config) = &self.on_config {
                    on_config(remote);
                }
                UpdateConfigurationResponse {
                    latest_applied_version: version,
                    error: String::new(),
                }
            }
            Err(e) => {
                tracing::warn!(version, "ignoring unparseable remote configuration: {e}");
                UpdateConfigurationResponse {
                    latest_applied_version: self.applied.load(Ordering::SeqCst),
                    error: e,
                }
            }
        }
    }
}

/// Parses the edge-pushed config JSON (cloudflared's config format with an
/// `ingress` list) into the hostnames libcfd understands.
pub(crate) fn parse_remote_config(version: i32, config: &[u8]) -> Result<RemoteConfig, String> {
    #[derive(Deserialize)]
    struct IngressRule {
        #[serde(default)]
        hostname: String,
    }
    #[derive(Deserialize)]
    struct IngressConfig {
        #[serde(default)]
        ingress: Vec<IngressRule>,
    }
    let parsed: IngressConfig =
        serde_json::from_slice(config).map_err(|e| format!("config is not valid JSON: {e}"))?;
    let hostnames = parsed
        .ingress
        .into_iter()
        .map(|rule| rule.hostname)
        .filter(|hostname| !hostname.is_empty())
        .collect();
    Ok(RemoteConfig { version, hostnames })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ingress_hostnames() {
        let config = br#"{"ingress":[{"hostname":"web.example.com","service":"http://localhost:80"},{"service":"http_status:404"}],"warp-routing":{}}"#;
        let remote = parse_remote_config(7, config).unwrap();
        assert_eq!(remote.version, 7);
        assert_eq!(remote.hostnames, vec!["web.example.com"]);
    }

    #[test]
    fn empty_ingress_yields_no_hostnames() {
        let remote = parse_remote_config(1, br#"{"ingress":[]}"#).unwrap();
        assert!(remote.hostnames.is_empty());
    }

    #[test]
    fn rejects_non_json_config() {
        assert!(parse_remote_config(1, b"not json").is_err());
    }
}
