//! HTTP/2 loopback test: end-to-end registration, HTTP serving, and
//! configuration acknowledgement.

use std::sync::Arc;

use crate::edge::h2::H2EdgeConnection;
use crate::edge::h2::stream::H2Bidi;
use crate::origin::{Body, Origin, Request, Response};
use crate::tunnel::Tunnel;

use super::h2_common::{start_edge, test_shared};
use super::{make_tunnel, serve_control};

#[tokio::test(flavor = "multi_thread")]
async fn h2_tunnel_end_to_end() {
    use bytes::Bytes;

    let (listener, ca_pem, acceptor) = start_edge().await;
    let edge_addr = listener.local_addr().unwrap();

    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();

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
        serve_control(H2Bidi::new(recv, send))
            .await
            .expect("control rpc");

        let request = http::Request::builder()
            .method("GET")
            .uri("http://example.com/hello")
            .header("host", "example.com")
            .body(())
            .unwrap();
        let (response_future, mut send) = client
            .send_request(request, false)
            .expect("send data request");
        send.send_data(Bytes::from_static(b"ping"), true).unwrap();
        let response = response_future.await.expect("data response");
        let status = response.status();
        let headers = response.headers().clone();
        let (_, mut body) = response.into_parts();
        let mut resp_body = Vec::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk.expect("body chunk");
            resp_body.extend_from_slice(&chunk);
        }

        // Configuration update stream: acknowledge the version without
        // applying the config (locally managed).
        let config_request = http::Request::builder()
            .method("POST")
            .uri("https://example.com/")
            .header(
                "Cf-Cloudflared-Proxy-Connection-Upgrade",
                "update-configuration",
            )
            .header("content-type", "application/json")
            .body(())
            .unwrap();
        let (response_future, mut send) = client
            .send_request(config_request, false)
            .expect("send config request");
        send.send_data(Bytes::from_static(br#"{"version":7,"config":{}}"#), true)
            .unwrap();
        let response = response_future.await.expect("config response");
        let config_status = response.status();
        let (_, mut body) = response.into_parts();
        let mut config_body = Vec::new();
        while let Some(chunk) = body.data().await {
            config_body.extend_from_slice(&chunk.expect("body chunk"));
        }
        (status, headers, resp_body, config_status, config_body)
    });

    let (conn, _local_ip) = H2EdgeConnection::connect(edge_addr, Some(&ca_pem))
        .await
        .expect("h2 edge connect");
    let tunnel = Arc::new(Tunnel::quick(make_tunnel()));
    let origin = Arc::new(Origin::http(move |mut request: Request| {
        let seen_tx = seen_tx.clone();
        async move {
            let body = request.body.collect().await.expect("body read");
            let _ = seen_tx.send((
                request.method.as_str().to_string(),
                request.uri.to_string(),
                body,
            ));
            Ok(Response::new(
                http::StatusCode::OK,
                http::HeaderMap::new(),
                Body::from_bytes(b"pong".to_vec()),
            ))
        }
    }));
    let (shared, shutdown) = test_shared(tunnel, origin);
    let serve_task = tokio::spawn(conn.serve(shared));

    let (status, headers, resp_body, config_status, config_body) =
        edge_task.await.expect("edge task");
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(resp_body, b"pong");
    assert!(headers.contains_key("cf-cloudflared-response-meta"));
    assert_eq!(config_status, http::StatusCode::OK);
    assert_eq!(
        String::from_utf8(config_body).unwrap(),
        r#"{"lastAppliedVersion":7,"err":null}"#
    );

    let (method, uri, request_body) = seen_rx
        .recv()
        .await
        .expect("origin handler should observe the request");
    assert_eq!(method, "GET");
    assert_eq!(uri, "http://example.com/hello");
    assert_eq!(request_body, b"ping");

    shutdown.fire();
    serve_task.abort();
}
