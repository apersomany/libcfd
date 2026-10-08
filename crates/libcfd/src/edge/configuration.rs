//! Remotely-managed tunnel configuration pushed by the edge.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use serde::Deserialize;

use libcfd_rpc::{CloudflaredHandler, UpdateConfigurationResponse};

/// The tunnel configuration the edge pushes for remotely-managed tunnels.
///
/// The ingress rules' hostnames are the public hostnames routed to this
/// tunnel (the catch-all rule has no hostname); `services` carries each
/// rule's service in the same order (empty for the catch-all rule), so
/// consumers can tell an HTTP route from a websocket or `tcp://` route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConfiguration {
    /// The configuration version from the push.
    pub version: i32,
    /// Public hostnames routed to this tunnel.
    pub hostnames: Vec<String>,
    /// The ingress service for each hostname (e.g. `http://127.0.0.1:8080`
    /// or `tcp://127.0.0.1:5432`); empty for the catch-all rule.
    pub services: Vec<String>,
}

/// The `CloudflaredHandler` used by both edge transports: applies config
/// pushes and forwards them to the consumer callback.
pub(crate) struct EdgeConfigurationHandler {
    on_configuration: Option<Arc<dyn Fn(RemoteConfiguration) + Send + Sync>>,
    applied: AtomicI32,
}

impl EdgeConfigurationHandler {
    pub(crate) fn new(
        on_configuration: Option<Arc<dyn Fn(RemoteConfiguration) + Send + Sync>>,
    ) -> Self {
        Self {
            on_configuration,
            applied: AtomicI32::new(-1),
        }
    }
}

impl CloudflaredHandler for EdgeConfigurationHandler {
    fn update_configuration(
        &self,
        version: i32,
        configuration: &[u8],
    ) -> UpdateConfigurationResponse {
        match parse_remote_configuration(version, configuration) {
            Ok(remote) => {
                self.applied.store(version, Ordering::SeqCst);
                tracing::info!(version, hostnames = ?remote.hostnames, "edge pushed remote configuration");
                if let Some(on_configuration) = &self.on_configuration {
                    on_configuration(remote);
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
/// `ingress` list) into the hostnames and services libcfd understands.
pub(crate) fn parse_remote_configuration(
    version: i32,
    configuration: &[u8],
) -> Result<RemoteConfiguration, String> {
    #[derive(Deserialize)]
    struct IngressRule {
        #[serde(default)]
        hostname: String,
        #[serde(default)]
        service: String,
    }
    #[derive(Deserialize)]
    struct IngressConfiguration {
        #[serde(default)]
        ingress: Vec<IngressRule>,
    }
    let parsed: IngressConfiguration = serde_json::from_slice(configuration)
        .map_err(|e| format!("config is not valid JSON: {e}"))?;
    let mut hostnames = Vec::new();
    let mut services = Vec::new();
    for rule in parsed.ingress {
        if !rule.hostname.is_empty() {
            hostnames.push(rule.hostname);
            services.push(rule.service);
        }
    }
    Ok(RemoteConfiguration {
        version,
        hostnames,
        services,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ingress_hostnames() {
        let configuration = br#"{"ingress":[{"hostname":"web.example.com","service":"http://localhost:80"},{"service":"http_status:404"}],"warp-routing":{}}"#;
        let remote = parse_remote_configuration(7, configuration).unwrap();
        assert_eq!(remote.version, 7);
        assert_eq!(remote.hostnames, vec!["web.example.com"]);
        assert_eq!(remote.services, vec!["http://localhost:80"]);
    }

    #[test]
    fn parses_tcp_service() {
        let configuration =
            br#"{"ingress":[{"hostname":"db.example.com","service":"tcp://127.0.0.1:5432"}]}"#;
        let remote = parse_remote_configuration(2, configuration).unwrap();
        assert_eq!(remote.hostnames, vec!["db.example.com"]);
        assert_eq!(remote.services, vec!["tcp://127.0.0.1:5432"]);
    }

    #[test]
    fn empty_ingress_yields_no_hostnames() {
        let remote = parse_remote_configuration(1, br#"{"ingress":[]}"#).unwrap();
        assert!(remote.hostnames.is_empty());
    }

    #[test]
    fn rejects_non_json_configuration() {
        assert!(parse_remote_configuration(1, b"not json").is_err());
    }
}
