//! Serving incoming edge streams to the origin handlers.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use libcfd_rpc::Incoming;
use libcfd_rpc::quic::{
    ConnectRequest, ConnectResponse, ConnectionType, DATA_STREAM_PROTOCOL_SIGNATURE, HTTP_HOST_KEY,
    HTTP_METHOD_KEY, HTTP_STATUS_KEY, PROTOCOL_V1, RPC_STREAM_PROTOCOL_SIGNATURE,
    read_connect_request, write_connect_response,
};

use crate::edge::quic::{QuicConnection, QuicStream};
use crate::error::{Error, Result};
use crate::origin::{Body, Origin, Request, Response, pump};

const HEADER_KEY_PREFIX: &str = "HttpHeader:";
/// Max bytes drained from an unread request body after the handler returns.
const DRAIN_LIMIT: u64 = 256 * 1024;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Accepts incoming streams and dispatches each new data stream to a
/// per-request task. The control stream (id 0) is skipped. Completed
/// streams are removed from the active set so it stays bounded.
pub(crate) async fn serve_requests(conn: Arc<QuicConnection>, origin: Arc<Origin>) -> Result<()> {
    let mut active = HashSet::new();
    active.insert(0);
    let mut tasks = tokio::task::JoinSet::new();
    let mut rx = conn.subscribe();
    loop {
        let new_ids = {
            let g = conn.inner.lock().unwrap();
            if g.closed {
                return Err(Error::quic(
                    g.close_reason
                        .clone()
                        .unwrap_or_else(|| "connection closed".into()),
                ));
            }
            g.conn
                .readable()
                .filter(|id| active.insert(*id))
                .collect::<Vec<_>>()
        };
        for id in new_ids {
            let c = conn.clone();
            let o = origin.clone();
            tasks.spawn(async move {
                let result = serve_stream(c, o.as_ref(), id).await;
                (id, result)
            });
        }
        tokio::select! {
            _ = rx.changed() => {}
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok((id, result))) => {
                        active.remove(&id);
                        if let Err(e) = result {
                            tracing::debug!(stream = id, "request stream failed: {e}");
                        }
                    }
                    Some(Err(e)) => {
                        tracing::debug!("request stream task failed: {e}");
                    }
                    None => {}
                }
            }
        }
    }
}

async fn serve_stream(conn: Arc<QuicConnection>, origin: &Origin, stream_id: u64) -> Result<()> {
    let mut stream = conn.stream(stream_id);

    let mut signature = [0u8; 6];
    stream.read_exact(&mut signature).await?;
    if signature == RPC_STREAM_PROTOCOL_SIGNATURE {
        return handle_rpc_stream(stream).await;
    }
    if signature != DATA_STREAM_PROTOCOL_SIGNATURE {
        return Err(Error::quic(format!(
            "stream {stream_id} has no data-stream signature"
        )));
    }
    let mut version = [0u8; 2];
    stream.read_exact(&mut version).await?;
    if version != PROTOCOL_V1 {
        return Err(Error::quic(format!(
            "stream {stream_id} has unsupported protocol version"
        )));
    }

    let connect = read_connect_request(&mut stream).await?;
    match connect.conn_type {
        ConnectionType::Http => handle_quic_http(conn, origin, connect, stream_id).await,
        ConnectionType::Websocket => handle_quic_websocket(conn, origin, connect, stream_id).await,
        ConnectionType::Tcp => handle_quic_tcp(conn, origin, connect, stream_id).await,
    }
}

async fn handle_quic_http(
    conn: Arc<QuicConnection>,
    origin: &Origin,
    connect: ConnectRequest,
    stream_id: u64,
) -> Result<()> {
    let request = build_request(&connect)?;
    let request = Request::new(
        request.method,
        request.uri,
        request.headers,
        Body::from_reader(conn.stream(stream_id)),
    );

    let response = match origin.http.handle_boxed(request).await {
        Ok(response) => response,
        Err(e) => {
            tracing::warn!("origin handler failed: {e}");
            Response::bad_gateway()
        }
    };

    let mut resp_stream = conn.stream(stream_id);
    let metadata = encode_response_metadata(&response);
    let connect_response = ConnectResponse {
        error: String::new(),
        metadata,
    };
    if let Err(e) = write_response_preamble(&mut resp_stream, &connect_response).await {
        cancel_write(&conn, stream_id);
        return Err(e);
    }

    let mut body = response.body;
    let copied = futures_util::io::copy(&mut body, &mut resp_stream).await;
    match copied {
        Ok(_) => {}
        Err(e) => {
            cancel_write(&conn, stream_id);
            return Err(Error::edge_io(e));
        }
    }
    tracing::trace!(stream = stream_id, "response sent");

    // Drain any request body bytes the origin did not consume so the edge's
    // flow-control credit is not held forever.
    drain_unread(conn.clone(), stream_id);

    resp_stream.finish();
    Ok(())
}

async fn handle_quic_websocket(
    conn: Arc<QuicConnection>,
    origin: &Origin,
    connect: ConnectRequest,
    stream_id: u64,
) -> Result<()> {
    let Some(websocket) = &origin.websocket else {
        return write_stream_error(&conn, stream_id, "no websocket origin handler").await;
    };
    let request = build_request(&connect)?;
    let connection = match websocket.connect_boxed(request).await {
        Ok(connection) => connection,
        Err(e) => return write_stream_error(&conn, stream_id, &format!("{e}")).await,
    };

    let mut resp_stream = conn.stream(stream_id);
    let metadata = encode_response_metadata(&connection.response);
    let connect_response = ConnectResponse {
        error: String::new(),
        metadata,
    };
    write_response_preamble(&mut resp_stream, &connect_response).await?;

    pump(
        connection.origin,
        conn.stream(stream_id),
        conn.stream(stream_id),
    )
    .await
}

async fn handle_quic_tcp(
    conn: Arc<QuicConnection>,
    origin: &Origin,
    connect: ConnectRequest,
    stream_id: u64,
) -> Result<()> {
    let Some(tcp) = &origin.tcp else {
        return write_stream_error(&conn, stream_id, "no tcp origin handler").await;
    };
    let request = Request::tcp(&connect.dest);
    let duplex = match tcp.connect_boxed(request).await {
        Ok(duplex) => duplex,
        Err(e) => return write_stream_error(&conn, stream_id, &format!("{e}")).await,
    };

    let mut resp_stream = conn.stream(stream_id);
    write_response_preamble(&mut resp_stream, &ConnectResponse::default()).await?;

    pump(duplex, conn.stream(stream_id), conn.stream(stream_id)).await
}

/// Handles edge-initiated RPC streams (session and configuration managers).
/// libcfd does not implement those interfaces, so every call is answered
/// with an `unimplemented` exception so the edge's RPC client fails cleanly.
async fn handle_rpc_stream(mut stream: QuicStream) -> Result<()> {
    loop {
        match libcfd_rpc::read_incoming(&mut stream).await? {
            Some(Incoming::Call { question_id, .. })
            | Some(Incoming::Bootstrap { question_id }) => {
                libcfd_rpc::send_exception(&mut stream, question_id, "unimplemented").await?;
            }
            Some(_) => {}
            None => return Ok(()),
        }
    }
}

async fn write_stream_error(conn: &QuicConnection, stream_id: u64, message: &str) -> Result<()> {
    let mut resp_stream = conn.stream(stream_id);
    let connect_response = ConnectResponse {
        error: message.to_string(),
        metadata: vec![(HTTP_STATUS_KEY.to_string(), "502".to_string())],
    };
    write_response_preamble(&mut resp_stream, &connect_response).await?;
    cancel_write(conn, stream_id);
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

fn cancel_write(conn: &QuicConnection, stream_id: u64) {
    let mut g = conn.inner.lock().unwrap();
    if !g.closed {
        let _ = g
            .conn
            .stream_shutdown(stream_id, quiche::Shutdown::Write, 0);
        if let Some(w) = g.write_wakers.remove(&stream_id) {
            w.wake();
        }
    }
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
    let uri = http::Uri::try_from(connect.dest.as_str())
        .map_err(|e| Error::quic(format!("invalid request dest {:?}: {e}", connect.dest)))?;
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

fn drain_unread(conn: Arc<QuicConnection>, stream_id: u64) {
    tokio::task::spawn(async move {
        let mut drain = conn.stream(stream_id);
        let mut buf = [0u8; 8192];
        let mut total: u64 = 0;
        let mut read = 0;
        let mut gave_up = false;
        loop {
            let res = tokio::time::timeout(DRAIN_TIMEOUT, drain.read(&mut buf)).await;
            match res {
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
            // Abandoned uploads beyond the drain limit would hold the
            // connection flow-control window open forever; reset the read
            // side so the edge stops sending (cloudflared cancels the
            // stream in the same situation).
            let mut g = conn.inner.lock().unwrap();
            if !g.closed {
                let _ = g.conn.stream_shutdown(stream_id, quiche::Shutdown::Read, 0);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_request() -> ConnectRequest {
        ConnectRequest {
            dest: "http://example.com/path".into(),
            conn_type: ConnectionType::Http,
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
            dest: "10.0.0.1:8080".into(),
            conn_type: ConnectionType::Tcp,
            metadata: vec![],
        };
        let request = Request::tcp(&connect.dest);
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
