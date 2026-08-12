//! Edge connectivity: discovery, connections, registration, and serving.
//!
//! [`EdgeConnector`] orchestrates edge discovery, connection establishment,
//! retries, and transport selection. The `quic` and `h2` transports are
//! gated behind the `quic-edge` and `h2-edge` features, respectively.

pub(crate) mod config;
mod connector;
pub(crate) mod control;
mod discovery;
mod error;
pub use error::Error;
pub(crate) mod event;
#[cfg(h2_any)]
pub(crate) mod h2;
#[cfg(quic_any)]
pub(crate) mod quic;
mod roots;
#[cfg(quic_any)]
pub(crate) mod serve;

pub(crate) use discovery::discover_edges;

pub use config::RemoteConfig;
pub use connector::{EdgeConnector, EdgeOptions, Transport, default_config_json};
