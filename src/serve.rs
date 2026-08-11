//! Serving incoming HTTP requests from the edge to the origin handler.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use libcfd_rpc::quic::{
    ConnectResponse, ConnectionType, DATA_STREAM_PROTOCOL_SIGNATURE, HTTP_HOST_KEY,
    HTTP_METHOD_KEY, HTTP_STATUS_KEY, PROTOCOL_V1, read_connect_request, write_connect_response,
};

use crate::error::{Error, Result};
use crate::origin::{Body, HttpOriginDyn, Request, Response};
use crate::quic::QuicConnection;

const HEADER_KEY_PREFIX: &str = "HttpHeader:";
/// Max bytes drained from an unread request body after the handler returns.
const DRAIN_LIMIT: u64 = 256 * 1024;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Accepts incoming streams and dispatches each new data stream to a
/// per-request task. The control stream (id 0) is skipped.
pub(crate) async fn serve_requests(
    conn: Arc<QuicConnection>,
    origin: Arc<dyn HttpOriginDyn>,
) -> Result<()> {
    let mut active = HashSet::new();
    active.insert(0);
    let mut rx = conn.subscribe();
    loop {
        let new_ids = {
            let g = conn.inner.lock().unwrap();
            if g.closed {
                return Err(Error::Quic(
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
            tokio::spawn(async move {
                if let Err(e) = serve_stream(c, o.as_ref(), id).await {
                    tracing::debug!(stream = id, "request stream failed: {e}");
                }
            });
        }
        // Wait for the next connection event before rescanning, so idle
        // connections do not spin.
        let _ = rx.changed().await;
    }
}

async fn serve_stream(
    conn: Arc<QuicConnection>,
    origin: &dyn HttpOriginDyn,
    stream_id: u64,
) -> Result<()> {
    let mut stream = conn.stream(stream_id);

    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await?;
    if header[..6] != DATA_STREAM_PROTOCOL_SIGNATURE {
        return Err(Error::Quic(format!(
            "stream {stream_id} has no data-stream signature"
        )));
    }
    if &header[6..] != PROTOCOL_V1 {
        return Err(Error::Quic(format!(
            "stream {stream_id} has unsupported protocol version"
        )));
    }

    let connect = read_connect_request(&mut stream).await?;
    if connect.conn_type != ConnectionType::Http {
        return Err(Error::Quic(format!(
            "stream {stream_id} is not an http request (type {:?})",
            connect.conn_type
        )));
    }

    let request = build_request(connect)?;
    let request = Request::new(
        request.method,
        request.uri,
        request.headers,
        Body::from_reader(stream),
    );

    let response = match origin.handle_boxed(request).await {
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
    resp_stream
        .write_all(&DATA_STREAM_PROTOCOL_SIGNATURE)
        .await?;
    resp_stream.write_all(PROTOCOL_V1).await?;
    write_connect_response(&mut resp_stream, &connect_response).await?;

    let mut body = response.body;
    let copied = futures_util::io::copy(&mut body, &mut resp_stream).await?;
    tracing::trace!(stream = stream_id, copied, "response sent");

    // Drain any request body bytes the origin did not consume so the edge's
    // flow-control credit is not held forever.
    drain_unread(conn.clone(), stream_id);

    resp_stream.finish();
    Ok(())
}

fn build_request(connect: libcfd_rpc::quic::ConnectRequest) -> Result<Request> {
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
        .map_err(|e| Error::Quic(format!("invalid request dest {:?}: {e}", connect.dest)))?;
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
        loop {
            let res = tokio::time::timeout(DRAIN_TIMEOUT, drain.read(&mut buf)).await;
            match res {
                Ok(Ok(n)) if n > 0 => {
                    total = total.saturating_add(n as u64);
                    if total >= DRAIN_LIMIT {
                        break;
                    }
                }
                Ok(Ok(_)) => break, // EOF
                Ok(Err(_)) => break,
                Err(_) => break, // drain timed out; give up
            }
            read += 1;
            if read > 4096 {
                break;
            }
        }
    });
}
