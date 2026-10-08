//! Edge connectivity: discovery, connections, registration, and serving.
//!
//! [`EdgeConnector`] orchestrates edge discovery, connection establishment,
//! retries, and transport selection. The `quic` and `h2` transports are
//! gated behind the `quic-edge` and `h2-edge` features, respectively.

pub(crate) mod configuration;
mod connector;
pub(crate) mod control;
mod discovery;
mod error;
pub use error::Error;
pub(crate) mod event;
pub(crate) mod transport;

pub(crate) use discovery::discover_edges;

pub use configuration::RemoteConfiguration;
pub use connector::{EdgeConnector, EdgeOptions, Transport, default_configuration_json};
