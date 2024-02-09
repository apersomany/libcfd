use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
};

use anyhow::Result;
use async_tungstenite::{tungstenite::protocol::Role, WebSocketStream};
use base64::{engine::general_purpose::STANDARD as STANDARD_BASE64, Engine};
use futures::{AsyncRead, AsyncWrite, SinkExt, StreamExt};
use libcfd::{
    connection::{
        ConnectRequest, ConnectResponse, Connection, DstAddr, QuinnRecvStream, QuinnSendStream,
        SrcAddr,
    },
    tunnel_config::TunnelConfig,
};
use sha1::{Digest, Sha1};
use tokio::{
    runtime::Builder as RuntimeBuilder,
    task::{spawn_local, LocalSet},
};

fn main() {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(LocalSet::new().run_until(async {
        if let Err(error) = async_main().await {
            println!("{error}");
        }
    }));
}

async fn async_main() -> Result<()> {
    let config = TunnelConfig::try_cloudflare().await?;
    println!("Quick tunnel created at wss://{}", config.hostname);
    let (connection, connection_details) =
        Connection::new(config, 0, SrcAddr::Default, DstAddr::Default).await?;
    println!("Registered tunnel connection: {:#?}", connection_details);
    loop {
        let connect_request = connection.accept().await?;
        spawn_local(async move {
            if let Err(error) = handle_connect_request(connect_request).await {
                println!("{error}")
            }
        });
    }
}

async fn handle_connect_request(connect_request: ConnectRequest) -> Result<()> {
    println!("Incoming request: {:#?}", connect_request.metadata);
    let mut metadata = HashMap::new();
    metadata.insert("HttpStatus".to_string(), "101".to_string());
    let mut digest = Sha1::new();
    digest.update(&connect_request.metadata["HttpHeader:Sec-Websocket-Key"]);
    digest.update("258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let accept = STANDARD_BASE64.encode(digest.finalize());
    metadata.insert("HttpHeader:Sec-Websocket-Accept".to_string(), accept);
    let (send_stream, recv_stream) = connect_request
        .respond_with(ConnectResponse::Metadata(metadata))
        .await?;
    let mut websocket = WebSocketStream::from_raw_socket(
        Stream {
            send_stream,
            recv_stream,
        },
        Role::Server,
        None,
    )
    .await;
    while let Some(Ok(message)) = websocket.next().await {
        websocket.send(message).await?
    }
    Ok(())
}

struct Stream {
    send_stream: QuinnSendStream,
    recv_stream: QuinnRecvStream,
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        AsyncWrite::poll_write(Box::pin(&mut self.send_stream).as_mut(), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_flush(Box::pin(&mut self.send_stream).as_mut(), cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_close(Box::pin(&mut self.send_stream).as_mut(), cx)
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        AsyncRead::poll_read(Box::pin(&mut self.recv_stream).as_mut(), cx, buf)
    }
}
