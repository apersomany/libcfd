//! Per-stream request handling for the HTTP/2 edge connection.

use std::sync::Arc;

use bytes::Bytes;
use futures_util::io::{AsyncReadExt, AsyncWriteExt};
use h2::RecvStream;
use h2::server::SendResponse;

use crate::error::{Error, Result};
use crate::origin::{
    Body, HttpResponder, Request, Response, TcpResponder, WebSocketResponder, pump, wait_outcome,
};

use libcfd_rpc::CloudflaredHandler;

use super::H2Shared;
use super::StreamType;
use super::classify;
use super::headers::{INTERNAL_UPGRADE_HEADER, encode_response_headers};
use super::stream::{ReceiveStreamReader, SendStreamWriter};
use super::websocket_accept;

pub(crate) async fn handle_stream(
    request: http::Request<RecvStream>,
    respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    match classify(request.headers()) {
        StreamType::Http => handle_h2_http(request, respond, shared).await,
        StreamType::Websocket => handle_h2_websocket(request, respond, shared).await,
        StreamType::Tcp => handle_h2_tcp(request, respond, shared).await,
        StreamType::Configuration => handle_h2_configuration(request, respond, shared).await,
        StreamType::Control => Ok(()),
    }
}

async fn handle_h2_http(
    request: http::Request<RecvStream>,
    respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers;
    headers.remove(INTERNAL_UPGRADE_HEADER);
    let request = Request::new(
        parts.method,
        parts.uri,
        headers,
        Body::from_reader(ReceiveStreamReader::new(body)),
    );
    let (responder, receiver) = HttpResponder::channel();
    shared.origin.http.handle(request, responder);
    let response = match wait_outcome(receiver).await {
        Ok(response) => response,
        Err(message) => return write_h2_error(respond, &message).await,
    };
    write_h2_response(respond, response).await
}

async fn handle_h2_websocket(
    request: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    let Some(websocket) = &shared.origin.websocket else {
        return write_h2_error(respond, "no websocket origin handler").await;
    };
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers;
    headers.remove(INTERNAL_UPGRADE_HEADER);
    let request = Request::new(parts.method, parts.uri, headers, Body::empty());
    let (responder, receiver) = WebSocketResponder::channel();
    websocket.connect(request, responder);
    let connection = match wait_outcome(receiver).await {
        Ok(connection) => connection,
        Err(message) => return write_h2_error(respond, &message).await,
    };
    let send = write_h2_headers(&mut respond, &connection.response)?;
    pump(
        connection.origin,
        ReceiveStreamReader::new(body),
        SendStreamWriter::new(send),
    )
    .await
}

async fn handle_h2_tcp(
    request: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    let Some(tcp) = &shared.origin.tcp else {
        return write_h2_error(respond, "no tcp origin handler").await;
    };
    let (parts, body) = request.into_parts();
    let host = request_host(&parts);
    let request = Request::tcp(&host);
    let (responder, receiver) = TcpResponder::channel();
    tcp.connect(request, responder);
    let origin_stream = match wait_outcome(receiver).await {
        Ok(origin_stream) => origin_stream,
        Err(message) => return write_h2_error(respond, &message).await,
    };
    let mut ack_headers = http::HeaderMap::new();
    if let Some(key) = parts.headers.get("sec-websocket-key")
        && let Ok(key) = key.to_str()
    {
        ack_headers.insert("connection", "Upgrade".parse().unwrap());
        ack_headers.insert("upgrade", "websocket".parse().unwrap());
        ack_headers.insert(
            "sec-websocket-accept",
            websocket_accept(key).parse().unwrap(),
        );
    }
    let ack = Response::new(
        http::StatusCode::SWITCHING_PROTOCOLS,
        ack_headers,
        Body::empty(),
    );
    let send = write_h2_headers(&mut respond, &ack)?;
    pump(
        origin_stream,
        ReceiveStreamReader::new(body),
        SendStreamWriter::new(send),
    )
    .await
}

async fn handle_h2_configuration(
    request: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct ConfigurationUpdateBody {
        version: i32,
        #[serde(rename = "config")]
        configuration: serde_json::Value,
    }
    let (_, body) = request.into_parts();
    let mut reader = ReceiveStreamReader::new(body);
    let mut data = Vec::new();
    reader.read_to_end(&mut data).await?;
    let parsed = serde_json::from_slice::<ConfigurationUpdateBody>(&data);
    let response = match parsed {
        Ok(body) => {
            let configuration =
                serde_json::to_vec(&body.configuration).map_err(|e| Error::h2(e.to_string()))?;
            shared
                .configuration_handler
                .update_configuration(body.version, &configuration)
        }
        Err(e) => libcfd_rpc::UpdateConfigurationResponse {
            latest_applied_version: -1,
            error: format!("config update body is not valid JSON: {e}"),
        },
    };
    let reply = serde_json::to_string(&serde_json::json!({
        "latestAppliedVersion": response.latest_applied_version,
        "err": response.error,
    }))
    .map_err(|e| Error::h2(e.to_string()))?;
    let send = respond.send_response(http::Response::new(()), false)?;
    let mut writer = SendStreamWriter::new(send);
    writer.write_all(reply.as_bytes()).await?;
    writer.close().await?;
    Ok(())
}

/// Writes an HTTP/2 response and streams the response body.
async fn write_h2_response(mut respond: SendResponse<Bytes>, response: Response) -> Result<()> {
    if response.body.size_hint() == Some(0) {
        let empty = response;
        let mut http_response = http::Response::builder()
            .status(remap_status(empty.status))
            .body(())
            .unwrap();
        *http_response.headers_mut() = encode_response_headers(&empty);
        respond.send_response(http_response, true)?;
        return Ok(());
    }
    let send = write_h2_headers(&mut respond, &response)?;
    let mut writer = SendStreamWriter::new(send);
    let mut body = response.body;
    futures_util::io::copy(&mut body, &mut writer).await?;
    writer.close().await?;
    Ok(())
}

/// Writes the response headers for a streaming exchange (websocket, TCP,
/// control) and returns the write side of the stream.
fn write_h2_headers(
    respond: &mut SendResponse<Bytes>,
    response: &Response,
) -> Result<h2::SendStream<Bytes>> {
    let mut http_response = http::Response::builder()
        .status(remap_status(response.status))
        .body(())
        .unwrap();
    *http_response.headers_mut() = encode_response_headers(response);
    respond
        .send_response(http_response, false)
        .map_err(|e| Error::h2(format!("failed to send response headers: {e}")))
}

async fn write_h2_error(respond: SendResponse<Bytes>, message: &str) -> Result<()> {
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "text/plain".parse().unwrap());
    headers.insert("content-length", message.len().to_string().parse().unwrap());
    let response = Response::new(
        http::StatusCode::BAD_GATEWAY,
        headers,
        Body::from_bytes(message.as_bytes().to_vec()),
    );
    write_h2_response(respond, response).await
}

/// HTTP/2 has no 101; cloudflared remaps it to 200.
fn remap_status(status: http::StatusCode) -> http::StatusCode {
    if status == http::StatusCode::SWITCHING_PROTOCOLS {
        http::StatusCode::OK
    } else {
        status
    }
}

fn request_host(parts: &http::request::Parts) -> String {
    if let Some(host) = parts.headers.get(http::header::HOST) {
        return host.to_str().unwrap_or("").to_string();
    }
    parts.uri.host().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_switching_protocols() {
        assert_eq!(
            remap_status(http::StatusCode::SWITCHING_PROTOCOLS),
            http::StatusCode::OK
        );
        assert_eq!(remap_status(http::StatusCode::OK), http::StatusCode::OK);
    }
}
