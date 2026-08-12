//! Transport selection and connection options.

use std::sync::Arc;
use std::time::Duration;

use crate::edge::RemoteConfig;

/// The transport used for a tunnel connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// QUIC only.
    #[cfg(feature = "quic-edge")]
    Quic,
    /// HTTP/2 only.
    #[cfg(feature = "h2-edge")]
    H2,
    /// Start with QUIC and fall back to HTTP/2 after repeated QUIC failures.
    #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
    Auto,
}

/// Options controlling how a tunnel connects to the edge.
#[derive(Clone)]
pub struct EdgeOptions {
    /// Transport selection policy.
    pub transport: Transport,
    /// Edge region override (`--region`); `None` uses the default SRV lookup.
    pub region: Option<String>,
    /// PEM-encoded CA certificates trusted in addition to the system store
    /// (mirrors cloudflared's `--ca-cert`).
    pub ca_cert_pem: Option<Vec<u8>>,
    /// JSON configuration pushed to the edge via `updateLocalConfiguration`
    /// for locally-managed tunnels.
    pub config_json: Vec<u8>,
    /// Per-connection establishment timeout.
    pub connect_timeout: Duration,
    /// Base reconnect delay between failed attempts (exponential backoff).
    /// Cloudflared's base is 1 second.
    pub backoff: Duration,
    /// Bounded time to wait for a graceful unregister and for in-flight
    /// requests to drain after shutdown.
    pub grace_period: Duration,
    /// QUIC failures before `Transport::Auto` falls back to HTTP/2.
    /// Cloudflared's default retry count is 5.
    pub max_quic_failures: u8,
    /// Called with each configuration the edge pushes for a
    /// remotely-managed tunnel (e.g. the hostnames routed to it).
    pub on_remote_config: Option<Arc<dyn Fn(RemoteConfig) + Send + Sync>>,
}

impl std::fmt::Debug for EdgeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeOptions")
            .field("transport", &self.transport)
            .field("region", &self.region)
            .field("ca_cert_pem", &self.ca_cert_pem)
            .field("config_json", &self.config_json)
            .field("connect_timeout", &self.connect_timeout)
            .field("backoff", &self.backoff)
            .field("grace_period", &self.grace_period)
            .field("max_quic_failures", &self.max_quic_failures)
            .field(
                "on_remote_config",
                &self.on_remote_config.as_ref().map(|_| "<callback>"),
            )
            .finish()
    }
}

impl Default for EdgeOptions {
    fn default() -> Self {
        Self {
            transport: default_transport(),
            region: None,
            ca_cert_pem: None,
            config_json: default_config_json().into(),
            connect_timeout: Duration::from_secs(15),
            backoff: Duration::from_secs(1),
            grace_period: Duration::from_secs(30),
            max_quic_failures: 5,
            on_remote_config: None,
        }
    }
}

/// The default transport depends on which edge transports are enabled: auto
/// when both are, otherwise the only enabled one.
#[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
fn default_transport() -> Transport {
    Transport::Auto
}

#[cfg(all(feature = "quic-edge", not(feature = "h2-edge")))]
fn default_transport() -> Transport {
    Transport::Quic
}

#[cfg(all(not(feature = "quic-edge"), feature = "h2-edge"))]
fn default_transport() -> Transport {
    Transport::H2
}

/// The default local configuration payload, matching the shape cloudflared
/// sends for a quick tunnel (a single catch-all ingress rule).
pub fn default_config_json() -> &'static str {
    r#"{"ingress":[{"hostname":"","service":"http://127.0.0.1:8080"}],"warp-routing":{}}"#
}

/// Applies the transport selection policy after `quic_failures` failures.
#[cfg(feature = "quic-edge")]
#[cfg_attr(not(feature = "h2-edge"), allow(unused_variables))]
pub(crate) fn select_transport(
    requested: Transport,
    quic_failures: u8,
    max_quic_failures: u8,
) -> Transport {
    match requested {
        #[cfg(feature = "quic-edge")]
        Transport::Quic => Transport::Quic,
        #[cfg(feature = "h2-edge")]
        Transport::H2 => Transport::H2,
        #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
        Transport::Auto => {
            if quic_failures >= max_quic_failures {
                Transport::H2
            } else {
                Transport::Quic
            }
        }
    }
}

/// Without QUIC there is only the HTTP/2 transport to select.
#[cfg(not(feature = "quic-edge"))]
pub(crate) fn select_transport(
    requested: Transport,
    _quic_failures: u8,
    _max_quic_failures: u8,
) -> Transport {
    match requested {
        Transport::H2 => Transport::H2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "quic-edge")]
    #[test]
    fn quic_only_transport_never_falls_back() {
        assert_eq!(select_transport(Transport::Quic, 100, 5), Transport::Quic);
    }

    #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
    #[test]
    fn auto_falls_back_after_max_failures() {
        assert_eq!(select_transport(Transport::Auto, 0, 5), Transport::Quic);
        assert_eq!(select_transport(Transport::Auto, 4, 5), Transport::Quic);
        assert_eq!(select_transport(Transport::Auto, 5, 5), Transport::H2);
        assert_eq!(select_transport(Transport::Auto, 9, 5), Transport::H2);
    }

    #[cfg(feature = "h2-edge")]
    #[test]
    fn h2_stays_h2() {
        assert_eq!(select_transport(Transport::H2, 0, 5), Transport::H2);
    }

    #[cfg(all(feature = "quic-edge", feature = "h2-edge"))]
    #[test]
    fn default_transport_is_auto_when_both_edges_enabled() {
        assert_eq!(EdgeOptions::default().transport, Transport::Auto);
    }

    #[cfg(all(feature = "quic-edge", not(feature = "h2-edge")))]
    #[test]
    fn default_transport_is_quic_without_h2() {
        assert_eq!(EdgeOptions::default().transport, Transport::Quic);
    }

    #[cfg(all(not(feature = "quic-edge"), feature = "h2-edge"))]
    #[test]
    fn default_transport_is_h2_without_quic() {
        assert_eq!(EdgeOptions::default().transport, Transport::H2);
    }

    #[test]
    fn default_backoff_matches_cloudflared() {
        assert_eq!(EdgeOptions::default().backoff, Duration::from_secs(1));
        assert_eq!(EdgeOptions::default().max_quic_failures, 5);
    }
}
