//! Errors from origin handlers and their byte streams.

use thiserror::Error;

/// Errors from origin handlers and their byte streams.
#[derive(Debug, Error)]
pub enum Error {
    /// The origin handler returned an error.
    #[error("origin handler error: {0}")]
    Handler(String),
    /// An underlying I/O operation failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
