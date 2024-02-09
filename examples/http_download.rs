use std::collections::HashMap;

use anyhow::Result;
use libcfd::{
    connection::{ConnectRequest, ConnectResponse, Connection, DstAddr, SrcAddr},
    tunnel_config::TunnelConfig,
};
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
    println!("Quick tunnel created at https://{}", config.hostname);
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
    metadata.insert("HttpStatus".to_string(), "200".to_string());
    metadata.insert(
        "HttpHeader:Content-Disposition".to_string(),
        "attachment".to_string(),
    );
    let (mut send_stream, _) = connect_request
        .respond_with(ConnectResponse::Metadata(metadata))
        .await?;
    let mut buffer = [0; 4096];
    loop {
        send_stream.write(&mut buffer).await?;
    }
}
