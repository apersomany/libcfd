//! HTTP/2 loopback test: websocket and TCP raw-stream exchanges through the
//! origin handlers.

use std::sync::Arc;

use crate::h2::H2EdgeConnection;
use crate::h2::stream::H2Bidi;
use crate::origin::{Body, Origin, Request, Response};
use crate::tunnel::Tunnel;

use super::h2_common::{
    OneShotTcpOrigin, OneShotWsOrigin, run_control_rpc, start_edge, test_shared,
};
use super::make_tunnel;

#[tokio::test(flavor = "multi_thread")]
async fn h2_websocket_tcp_round_trip() {
    use bytes::Bytes;

    let (listener, ca_pem, acceptor) = start_edge().await;
    let edge_addr = listener.local_addr().unwrap();

    let edge_task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let tls = acceptor.accept(tcp).await.expect("tls accept");
        let (mut client, conn) = h2::client::handshake(tls)
            .await
            .expect("h2 client handshake");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        client = client.ready().await.expect("client ready");

        let control_request = http::Request::builder()
            .method("POST")
            .uri("https://example.com/")
            .header("Cf-Cloudflared-Proxy-Connection-Upgrade", "control-stream")
            .body(())
            .unwrap();
        let (response_future, send) = client
            .send_request(control_request, false)
            .expect("send control request");
        let response = response_future.await.expect("control response");
        assert_eq!(response.status(), http::StatusCode::OK);
        let (_, recv) = response.into_parts();
        run_control_rpc(H2Bidi::new(recv, send)).await;

        // Websocket upgrade stream: 101 is remapped to 200 and the origin's
        // Sec-WebSocket-Accept travels in the serialized user headers. The
        // request body is half-closed after the payload; the response side
        // stays open until the origin closes.
        let ws_request = http::Request::builder()
            .method("GET")
            .uri("http://example.com/ws")
            .header("host", "example.com")
            .header("Cf-Cloudflared-Proxy-Connection-Upgrade", "websocket")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap();
        let (response_future, mut send) = client
            .send_request(ws_request, false)
            .expect("send websocket request");
        send.send_data(Bytes::from_static(b"ping-ws"), false)
            .unwrap();
        send.send_data(Bytes::new(), true).unwrap();
        let response = response_future.await.expect("websocket response");
        let ws_status = response.status();
        let ws_headers = response.headers().clone();
        let (_, mut body) = response.into_parts();
        let mut ws_body = Vec::new();
        while let Some(chunk) = body.data().await {
            ws_body.extend_from_slice(&chunk.expect("body chunk"));
        }

        // Raw TCP proxy stream: the ack is a bare 101 (remapped to 200).
        let tcp_request = http::Request::builder()
            .method("GET")
            .uri("http://10.0.0.1:8080/")
            .header("host", "10.0.0.1:8080")
            .header("Cf-Cloudflared-Proxy-Src", "127.0.0.1")
            .body(())
            .unwrap();
        let (response_future, mut send) = client
            .send_request(tcp_request, false)
            .expect("send tcp request");
        send.send_data(Bytes::from_static(b"ping-tcp"), false)
            .unwrap();
        send.send_data(Bytes::new(), true).unwrap();
        let response = response_future.await.expect("tcp response");
        let tcp_status = response.status();
        let (_, mut body) = response.into_parts();
        let mut tcp_body = Vec::new();
        while let Some(chunk) = body.data().await {
            tcp_body.extend_from_slice(&chunk.expect("body chunk"));
        }

        (ws_status, ws_headers, ws_body, tcp_status, tcp_body)
    });

    let (conn, _local_ip) = H2EdgeConnection::connect(edge_addr, Some(&ca_pem))
        .await
        .expect("h2 edge connect");
    let tunnel = Arc::new(Tunnel::quick(make_tunnel()));
    let origin = Arc::new(
        Origin::http(|_request: Request| async move {
            Ok(Response::new(
                http::StatusCode::NOT_FOUND,
                http::HeaderMap::new(),
                Body::empty(),
            ))
        })
        .with_websocket(OneShotWsOrigin)
        .with_tcp(OneShotTcpOrigin),
    );
    let (shared, shutdown) = test_shared(tunnel, origin);
    let serve_task = tokio::spawn(conn.serve(shared));

    let (ws_status, ws_headers, ws_body, tcp_status, tcp_body) =
        edge_task.await.expect("edge task");
    assert_eq!(ws_status, http::StatusCode::OK);
    assert_eq!(ws_body, b"ping-ws");
    let serialized = ws_headers
        .get("cf-cloudflared-response-headers")
        .expect("websocket accept in serialized user headers")
        .to_str()
        .unwrap();
    let user = crate::h2::headers::deserialize_headers(serialized);
    assert!(
        user.iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("Sec-WebSocket-Accept")
                && v == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
    );
    assert_eq!(tcp_status, http::StatusCode::OK);
    assert_eq!(tcp_body, b"ping-tcp");

    shutdown.fire();
    serve_task.abort();
}
