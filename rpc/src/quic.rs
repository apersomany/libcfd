//! Plain-Rust types for the QUIC per-stream metadata protocol.
//!
//! The wire format is Cap'n Proto (see `quic_metadata_protocol.capnp`);
//! encoding and decoding happen here so the `libcfd` crate never touches
//! `capnp` directly.

use crate::error::Result;
use crate::io::{AsyncStream, read_message, write_message};
use crate::quic_metadata_protocol_capnp as mpc;

/// Magic bytes that identify a data (request) stream, from cloudflared's
/// `tunnelrpc/quic/protocol.go`.
pub const DATA_STREAM_PROTOCOL_SIGNATURE: [u8; 6] = [0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E];

/// Magic bytes that identify an edge-initiated RPC stream.
pub const RPC_STREAM_PROTOCOL_SIGNATURE: [u8; 6] = [0x52, 0xBB, 0x82, 0x5C, 0xDB, 0x65];

/// The per-stream protocol version ("01").
pub const PROTOCOL_V1: &[u8] = b"01";

/// Metadata keys used on data streams.
pub const HTTP_METHOD_KEY: &str = "HttpMethod";
pub const HTTP_HOST_KEY: &str = "HttpHost";
pub const HTTP_HEADER_KEY: &str = "HttpHeader";
pub const HTTP_STATUS_KEY: &str = "HttpStatus";
pub const FLOW_ID_KEY: &str = "FlowID";
pub const TRACE_ID_KEY: &str = "cf-trace-id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    Http,
    Websocket,
    Tcp,
}

impl ConnectionType {
    fn from_capnp(t: mpc::ConnectionType) -> Result<Self> {
        Ok(match t {
            mpc::ConnectionType::Http => Self::Http,
            mpc::ConnectionType::Websocket => Self::Websocket,
            mpc::ConnectionType::Tcp => Self::Tcp,
        })
    }

    fn to_capnp(self) -> mpc::ConnectionType {
        match self {
            Self::Http => mpc::ConnectionType::Http,
            Self::Websocket => mpc::ConnectionType::Websocket,
            Self::Tcp => mpc::ConnectionType::Tcp,
        }
    }
}

/// The connection request sent by the edge on a data stream.
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    pub dest: String,
    pub conn_type: ConnectionType,
    pub metadata: Vec<(String, String)>,
}

/// The connection response sent back to the edge.
#[derive(Debug, Clone, Default)]
pub struct ConnectResponse {
    pub error: String,
    pub metadata: Vec<(String, String)>,
}

/// Reads a `ConnectRequest` message from a stream.
pub async fn read_connect_request<S: AsyncStream + Unpin>(
    stream: &mut S,
) -> Result<ConnectRequest> {
    let reader = read_message(stream).await?;
    decode_connect_request_message(&reader)
}

/// Writes a `ConnectResponse` message to a stream.
pub async fn write_connect_response<S: AsyncStream + Unpin>(
    stream: &mut S,
    response: &ConnectResponse,
) -> Result<()> {
    let message = encode_connect_response(response)?;
    write_message(stream, &message).await
}

/// Encodes a `ConnectResponse` into a Cap'n Proto message.
pub fn encode_connect_response(
    response: &ConnectResponse,
) -> Result<capnp::message::Builder<capnp::message::HeapAllocator>> {
    let mut message = capnp::message::Builder::new_default();
    let mut root = message.init_root::<mpc::connect_response::Builder>();
    root.set_error(&response.error);
    let mut md = root
        .reborrow()
        .init_metadata(response.metadata.len() as u32);
    for (i, (key, val)) in response.metadata.iter().enumerate() {
        let mut entry = md.reborrow().get(i as u32);
        entry.set_key(key);
        entry.set_val(val);
    }
    Ok(message)
}

/// Encodes a `ConnectRequest` into a Cap'n Proto message (used by tests and
/// the mock edge).
pub fn encode_connect_request(
    request: &ConnectRequest,
) -> Result<capnp::message::Builder<capnp::message::HeapAllocator>> {
    let mut message = capnp::message::Builder::new_default();
    let mut root = message.init_root::<mpc::connect_request::Builder>();
    root.set_dest(&request.dest);
    root.set_type(request.conn_type.to_capnp());
    let mut md = root.reborrow().init_metadata(request.metadata.len() as u32);
    for (i, (key, val)) in request.metadata.iter().enumerate() {
        let mut entry = md.reborrow().get(i as u32);
        entry.set_key(key);
        entry.set_val(val);
    }
    Ok(message)
}

/// Encodes a `ConnectRequest` as framed message bytes.
pub fn encode_connect_request_bytes(request: &ConnectRequest) -> Result<Vec<u8>> {
    let message = encode_connect_request(request)?;
    Ok(capnp::serialize::write_message_to_words(&message))
}

/// Writes a `ConnectRequest` message to a stream (used by the mock edge).
pub async fn write_connect_request<S: AsyncStream + Unpin>(
    stream: &mut S,
    request: &ConnectRequest,
) -> Result<()> {
    let message = encode_connect_request(request)?;
    write_message(stream, &message).await
}

fn decode_connect_request_message<R: capnp::message::ReaderSegments>(
    reader: &capnp::message::Reader<R>,
) -> Result<ConnectRequest> {
    let root = reader.get_root::<mpc::connect_request::Reader>()?;
    let dest = root.get_dest()?.to_str()?.to_string();
    let conn_type = ConnectionType::from_capnp(root.get_type()?)?;
    let mut metadata = Vec::new();
    for entry in root.get_metadata()? {
        metadata.push((
            entry.get_key()?.to_str()?.to_string(),
            entry.get_val()?.to_str()?.to_string(),
        ));
    }
    Ok(ConnectRequest {
        dest,
        conn_type,
        metadata,
    })
}

/// Decodes a `ConnectRequest` from serialized message bytes.
pub fn decode_connect_request_bytes(bytes: &[u8]) -> Result<ConnectRequest> {
    let reader = capnp::serialize::read_message_from_flat_slice(
        &mut &bytes[..],
        capnp::message::ReaderOptions::new(),
    )?;
    decode_connect_request_message(&reader)
}

/// Decodes a `ConnectResponse` from serialized message bytes.
pub fn decode_connect_response_bytes(bytes: &[u8]) -> Result<ConnectResponse> {
    let reader = capnp::serialize::read_message_from_flat_slice(
        &mut &bytes[..],
        capnp::message::ReaderOptions::new(),
    )?;
    let root = reader.get_root::<mpc::connect_response::Reader>()?;
    let error = root.get_error()?.to_str()?.to_string();
    let mut metadata = Vec::new();
    for entry in root.get_metadata()? {
        metadata.push((
            entry.get_key()?.to_str()?.to_string(),
            entry.get_val()?.to_str()?.to_string(),
        ));
    }
    Ok(ConnectResponse { error, metadata })
}

/// Reads a `ConnectResponse` message from a stream (used by the mock edge).
pub async fn read_connect_response<S: AsyncStream + Unpin>(
    stream: &mut S,
) -> Result<ConnectResponse> {
    let reader = read_message(stream).await?;
    let root = reader.get_root::<mpc::connect_response::Reader>()?;
    let error = root.get_error()?.to_str()?.to_string();
    let mut metadata = Vec::new();
    for entry in root.get_metadata()? {
        metadata.push((
            entry.get_key()?.to_str()?.to_string(),
            entry.get_val()?.to_str()?.to_string(),
        ));
    }
    Ok(ConnectResponse { error, metadata })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_request_round_trip() {
        let request = ConnectRequest {
            dest: "http://example.com/path".into(),
            conn_type: ConnectionType::Http,
            metadata: vec![
                ("HttpMethod".into(), "GET".into()),
                ("HttpHost".into(), "example.com".into()),
            ],
        };
        let message = encode_connect_request(&request).unwrap();
        let bytes = capnp::serialize::write_message_to_words(&message);
        let decoded = decode_connect_request_bytes(&bytes).unwrap();
        assert_eq!(decoded.dest, request.dest);
        assert_eq!(decoded.conn_type, request.conn_type);
        assert_eq!(decoded.metadata, request.metadata);
    }

    #[test]
    fn connect_response_round_trip() {
        let response = ConnectResponse {
            error: String::new(),
            metadata: vec![
                ("HttpStatus".into(), "200".into()),
                ("HttpHeader:content-type".into(), "text/plain".into()),
            ],
        };
        let message = encode_connect_response(&response).unwrap();
        let bytes = capnp::serialize::write_message_to_words(&message);
        let decoded = decode_connect_response_bytes(&bytes).unwrap();
        assert_eq!(decoded.error, "");
        assert_eq!(decoded.metadata, response.metadata);
    }
}
