//! Serving incoming edge streams to the origin handlers.

use std::sync::Arc;
use std::time::Duration;

use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use libcfd_rpc::quic::{
    ConnectRequest, ConnectResponse, ConnectionType, DATA_STREAM_PROTOCOL_SIGNATURE, HTTP_HOST_KEY,
    HTTP_METHOD_KEY, HTTP_STATUS_KEY, PROTOCOL_V1, RPC_STREAM_PROTOCOL_SIGNATURE,
    read_connect_request, write_connect_response,
};

use crate::edge::configuration::EdgeConfigurationHandler;
use crate::edge::quic::{QuicConnection, QuicStream};
use crate::error::{Error, Result};
use crate::origin::{Body, Origin, OriginEvent, Request, Responder, Response, pump, wait_event};

const HEADER_KEY_PREFIX: &str = "HttpHeader:";
/// Max bytes drained from an unread request body after the handler returns.
const DRAIN_LIMIT: u64 = 256 * 1024;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Accepts incoming streams and dispatches each new data stream to a
/// per-request task. The control stream (id 0) is never yielded by
/// `accept_stream`. Completed streams are removed from the active set so it
/// stays bounded.
pub(crate) async fn serve_requests(
    connection: Arc<QuicConnection>,
    origin: Arc<Origin>,
    configuration_handler: Arc<EdgeConfigurationHandler>,
) -> Result<()> {
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok(Err(e))) => {
                        tracing::debug!("request stream failed: {e}");
                    }
                    Some(Ok(Ok(()))) => {}
                    Some(Err(e)) => {
                        tracing::debug!("request stream task failed: {e}");
                    }
                    None => {}
                }
            }
            accepted = connection.accept_stream() => {
                match accepted {
                    Ok(Some(stream)) => {
                        let stream_id = stream.id();
                        let o = origin.clone();
                        let ch = configuration_handler.clone();
                        let c = connection.clone();
                        tasks.spawn(async move {
                            let result = serve_stream(o.as_ref(), ch.as_ref(), stream).await;
                            c.release(stream_id);
                            result
                        });
                    }
                    Ok(None) => {
                        return Err(Error::quic(
                            connection
                                .close_reason()
                                .unwrap_or_else(|| "connection closed".into()),
                        ));
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

async fn serve_stream(
    origin: &Origin,
    configuration_handler: &EdgeConfigurationHandler,
    mut stream: QuicStream,
) -> Result<()> {
    let mut signature = [0u8; 6];
    stream.read_exact(&mut signature).await?;
    if signature == RPC_STREAM_PROTOCOL_SIGNATURE {
        return handle_rpc_stream(stream, configuration_handler).await;
    }
    if signature != DATA_STREAM_PROTOCOL_SIGNATURE {
        return Err(Error::quic(format!(
            "stream {} has no data-stream signature",
            stream.id()
        )));
    }
    let mut version = [0u8; 2];
    stream.read_exact(&mut version).await?;
    if version != PROTOCOL_V1 {
        return Err(Error::quic(format!(
            "stream {} has unsupported protocol version",
            stream.id()
        )));
    }

    let connect = read_connect_request(&mut stream).await?;
    match connect.connection_type {
        ConnectionType::Http => handle_quic_http(origin, connect, stream).await,
        ConnectionType::Websocket => handle_quic_websocket(origin, connect, stream).await,
        ConnectionType::Tcp => handle_quic_tcp(origin, connect, stream).await,
    }
}

async fn handle_quic_http(
    origin: &Origin,
    connect: ConnectRequest,
    stream: QuicStream,
) -> Result<()> {
    let request = build_request(&connect)?;
    let request = Request::new(
        request.method,
        request.uri,
        request.headers,
        Body::from_reader(stream.clone()),
    );

    let (responder, mut events) = Responder::channel();
    origin.http.handle(request, responder);
    let response = match wait_event(&mut events).await {
        Ok(OriginEvent::Response(response)) => response,
        Ok(_) => {
            return write_stream_error(&stream, "origin produced an unexpected response").await;
        }
        Err(message) => return write_stream_error(&stream, &message).await,
    };

    let mut response_stream = stream.clone();
    let metadata = encode_response_metadata(&response);
    let connect_response = ConnectResponse {
        error: String::new(),
        metadata,
    };
    if let Err(e) = write_response_preamble(&mut response_stream, &connect_response).await {
        stream.cancel_write();
        return Err(e);
    }

    let mut body = response.body;
    let copied = futures_util::io::copy(&mut body, &mut response_stream).await;
    match copied {
        Ok(_) => {}
        Err(e) => {
            stream.cancel_write();
            return Err(Error::edge_io(e));
        }
    }
    tracing::trace!(stream = stream.id(), "response sent");

    // Drain unconsumed request body bytes so the edge's flow-control credit is not held forever.
    drain_unread(stream);

    response_stream.finish();
    Ok(())
}

async fn handle_quic_websocket(
    origin: &Origin,
    connect: ConnectRequest,
    stream: QuicStream,
) -> Result<()> {
    let Some(websocket) = &origin.websocket else {
        return write_stream_error(&stream, "no websocket origin handler").await;
    };
    let request = build_request(&connect)?;
    let (responder, mut events) = Responder::channel();
    websocket.connect(request, responder);
    let connection = match wait_event(&mut events).await {
        Ok(OriginEvent::WebSocket(connection)) => connection,
        Ok(_) => {
            return write_stream_error(&stream, "origin produced an unexpected response").await;
        }
        Err(message) => return write_stream_error(&stream, &message).await,
    };

    let mut response_stream = stream.clone();
    let metadata = encode_response_metadata(&connection.response);
    let connect_response = ConnectResponse {
        error: String::new(),
        metadata,
    };
    write_response_preamble(&mut response_stream, &connect_response).await?;

    pump(connection.origin, stream.clone(), stream.clone()).await
}

async fn handle_quic_tcp(
    origin: &Origin,
    connect: ConnectRequest,
    stream: QuicStream,
) -> Result<()> {
    let Some(tcp) = &origin.tcp else {
        return write_stream_error(&stream, "no tcp origin handler").await;
    };
    let request = Request::tcp(&connect.destination);
    let (responder, mut events) = Responder::channel();
    tcp.connect(request, responder);
    let duplex = match wait_event(&mut events).await {
        Ok(OriginEvent::Stream(duplex)) => duplex,
        Ok(_) => {
            return write_stream_error(&stream, "origin produced an unexpected response").await;
        }
        Err(message) => return write_stream_error(&stream, &message).await,
    };

    let mut response_stream = stream.clone();
    write_response_preamble(&mut response_stream, &ConnectResponse::default()).await?;

    pump(duplex, stream.clone(), stream.clone()).await
}

/// Handles edge-initiated RPC streams: the edge bootstraps the connector's
/// `CloudflaredServer` interface and calls `updateConfiguration` to push the
/// remotely-managed tunnel configuration (and UDP session methods, which
/// libcfd answers with an error).
async fn handle_rpc_stream(
    mut stream: QuicStream,
    configuration_handler: &EdgeConfigurationHandler,
) -> Result<()> {
    libcfd_rpc::serve_cloudflared(&mut stream, configuration_handler)
        .await
        .map_err(Error::from)
}

async fn write_stream_error(stream: &QuicStream, message: &str) -> Result<()> {
    let mut response_stream = stream.clone();
    let connect_response = ConnectResponse {
        error: message.to_string(),
        metadata: vec![(HTTP_STATUS_KEY.to_string(), "502".to_string())],
    };
    write_response_preamble(&mut response_stream, &connect_response).await?;
    stream.cancel_write();
    Ok(())
}

async fn write_response_preamble(
    stream: &mut QuicStream,
    response: &ConnectResponse,
) -> Result<()> {
    stream.write_all(&DATA_STREAM_PROTOCOL_SIGNATURE).await?;
    stream.write_all(PROTOCOL_V1).await?;
    write_connect_response(stream, response).await?;
    Ok(())
}

fn build_request(connect: &ConnectRequest) -> Result<Request> {
    let mut method = http::Method::GET;
    let mut headers = http::HeaderMap::new();
    for (key, val) in &connect.metadata {
        if key == HTTP_METHOD_KEY {
            method = http::Method::from_bytes(val.as_bytes()).unwrap_or(http::Method::GET);
        } else if key == HTTP_HOST_KEY
            && let Ok(hv) = http::HeaderValue::from_str(val)
        {
            headers.insert(http::header::HOST, hv);
        } else if let Some(name) = key.strip_prefix(HEADER_KEY_PREFIX)
            && let (Ok(n), Ok(v)) = (
                http::HeaderName::from_bytes(name.as_bytes()),
                http::HeaderValue::from_str(val),
            )
        {
            headers.append(n, v);
        }
    }
    let uri = http::Uri::try_from(connect.destination.as_str()).map_err(|e| {
        Error::quic(format!(
            "invalid request destination {:?}: {e}",
            connect.destination
        ))
    })?;
    Ok(Request::new(method, uri, headers, Body::empty()))
}

fn encode_response_metadata(response: &Response) -> Vec<(String, String)> {
    let mut metadata = vec![(
        HTTP_STATUS_KEY.to_string(),
        response.status.as_u16().to_string(),
    )];
    for (name, value) in response.headers.iter() {
        metadata.push((
            format!("{HEADER_KEY_PREFIX}{name}"),
            value.to_str().unwrap_or("").to_string(),
        ));
    }
    metadata
}

fn drain_unread(stream: QuicStream) {
    tokio::task::spawn(async move {
        let mut drain = stream;
        let mut buffer = [0u8; 8192];
        let mut total: u64 = 0;
        let mut read = 0;
        let mut gave_up = false;
        loop {
            let result = tokio::time::timeout(DRAIN_TIMEOUT, drain.read(&mut buffer)).await;
            match result {
                Ok(Ok(n)) if n > 0 => {
                    total = total.saturating_add(n as u64);
                    if total >= DRAIN_LIMIT {
                        gave_up = true;
                        break;
                    }
                }
                Ok(Ok(_)) => break, // EOF
                Ok(Err(_)) => {
                    gave_up = true;
                    break;
                }
                Err(_) => {
                    gave_up = true;
                    break; // drain timed out; give up
                }
            }
            read += 1;
            if read > 4096 {
                gave_up = true;
                break;
            }
        }
        if gave_up {
            // Stop the read side so abandoned uploads beyond the drain limit do not hold the flow-control window open (cloudflared cancels the stream too).
            drain.stop_read();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_request() -> ConnectRequest {
        ConnectRequest {
            destination: "http://example.com/path".into(),
            connection_type: ConnectionType::Http,
            metadata: vec![
                ("HttpMethod".into(), "POST".into()),
                ("HttpHost".into(), "example.com".into()),
                ("HttpHeader:content-type".into(), "text/plain".into()),
            ],
        }
    }

    #[test]
    fn classifies_http_request() {
        let request = build_request(&http_request()).unwrap();
        assert_eq!(request.method, http::Method::POST);
        assert_eq!(request.uri, "http://example.com/path");
        assert_eq!(request.headers[http::header::HOST], "example.com");
        assert_eq!(request.headers["content-type"], "text/plain");
    }

    #[test]
    fn classifies_tcp_request() {
        let connect = ConnectRequest {
            destination: "10.0.0.1:8080".into(),
            connection_type: ConnectionType::Tcp,
            metadata: vec![],
        };
        let request = Request::tcp(&connect.destination);
        assert_eq!(request.uri.to_string(), "http://10.0.0.1:8080/");
    }

    #[test]
    fn encodes_response_metadata() {
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, "text/plain".parse().unwrap());
        let response = Response::new(http::StatusCode::OK, headers, Body::empty());
        let metadata = encode_response_metadata(&response);
        assert!(metadata.contains(&("HttpStatus".into(), "200".into())));
        assert!(metadata.contains(&("HttpHeader:content-type".into(), "text/plain".into())));
    }
}
