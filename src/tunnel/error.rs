//! Errors from tunnel identities and the quick tunnel HTTP API.

use thiserror::Error;

/// Errors from tunnel identities and the quick tunnel HTTP API.
#[derive(Debug, Error)]
pub enum Error {
    /// The quick tunnel HTTP API rejected the request or returned an error.
    #[cfg(feature = "quick-tunnel")]
    #[error("quick tunnel API error: {0}")]
    QuickTunnelApi(String),
    /// The quick tunnel HTTP API response could not be parsed.
    #[cfg(feature = "quick-tunnel")]
    #[error("quick tunnel API response was malformed: {0}")]
    QuickTunnelResponse(String),
    /// The HTTP request to the quick tunnel API failed.
    #[cfg(feature = "quick-tunnel")]
    #[error("quick tunnel API request failed: {0}")]
    QuickTunnelRequest(#[source] std::io::Error),
    /// A named tunnel credentials file could not be loaded or parsed.
    #[cfg(feature = "named-tunnel")]
    #[error("named tunnel credentials error: {0}")]
    NamedTunnelCredentials(String),
    /// A tunnel id could not be parsed as a UUID.
    #[error("invalid tunnel id: {0}")]
    InvalidTunnelIdentifier(String),
}
