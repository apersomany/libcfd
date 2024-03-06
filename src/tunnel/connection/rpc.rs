use std::{
    env::consts::{ARCH, OS},
    pin::Pin,
    sync::Arc,
};

use anyhow::{anyhow, Error, Result};
use futures::{AsyncRead, AsyncWrite, Future};
use rpc::{
    capnp::{capability::FromClientHook, message::ReaderOptions},
    capnp_rpc::{rpc_twoparty_capnp::Side, twoparty::VatNetwork, RpcSystem},
    generated::{connection_response, registration_server},
};
use tokio::{
    select,
    sync::{mpsc, oneshot},
    task::LocalSet,
};
use uuid::Uuid;

use crate::tunnel::credential::TunnelCredential;

#[derive(Clone)]
pub struct RpcClient {
    sender: mpsc::Sender<Box<dyn Task>>,
}

impl RpcClient {
    pub fn new<R, W>(reader: R, writer: W) -> Arc<Self>
    where
        R: AsyncRead + Unpin + 'static,
        W: AsyncWrite + Unpin + 'static,
    {
        let mut rpc_system = RpcSystem::new(
            Box::new(VatNetwork::new(
                reader,
                writer,
                Side::Client,
                ReaderOptions::new(),
            )),
            None,
        );
        let (sender, mut receiver) = mpsc::channel::<Box<dyn Task>>(8);
        let local_set = LocalSet::new();
        local_set.spawn_local(async move {
            while let Some(mut call_runner) = receiver.recv().await {
                call_runner.run(&mut rpc_system).await;
            }
        });
        Arc::new(Self { sender })
    }

    pub async fn register_connection(
        &self,
        credential: &TunnelCredential,
        connection_index: u8,
    ) -> Result<ConnectionDetails> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(
                RegisterConnection {
                    credential: credential.clone(),
                    connection_index,
                }
                .task(sender),
            )
            .await?;
        receiver.await?
    }
}

trait Call: Send + Sync + Sized + 'static {
    type Client: FromClientHook;
    type Return: Send + Sync;

    fn call(&mut self, client: Self::Client) -> impl Future<Output = Result<Self::Return>>;

    fn task(self, sender: oneshot::Sender<Result<Self::Return>>) -> Box<dyn Task> {
        Box::new(CallTask {
            call: self,
            sender: Some(sender),
        })
    }
}

trait Task: Send + Sync {
    fn run<'a>(
        &'a mut self,
        rpc_system: &'a mut RpcSystem<Side>,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>>;
}

struct CallTask<T: Call> {
    call: T,
    sender: Option<oneshot::Sender<Result<T::Return>>>,
}

impl<T: Call> Task for CallTask<T> {
    fn run<'a>(
        &'a mut self,
        rpc_system: &'a mut RpcSystem<Side>,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        Box::pin(async move {
            if let Some(sender) = self.sender.take() {
                select! {
                    result = self.call.call(rpc_system.bootstrap(Side::Client)) => {
                        let _ = sender.send(result);
                    },
                    result = rpc_system => if let Err(error) = result {
                        let _ = sender.send(Err(error.into()));
                    }
                };
            }
        })
    }
}

struct RegisterConnection {
    credential: TunnelCredential,
    connection_index: u8,
}

impl Call for RegisterConnection {
    type Client = registration_server::Client;

    type Return = ConnectionDetails;

    async fn call(&mut self, client: Self::Client) -> Result<Self::Return> {
        let mut register_connection_request = client.register_connection_request();
        // Set tunnel authentication parameter.
        let mut tunnel_auth_builder = register_connection_request.get().init_auth();
        tunnel_auth_builder.set_account_tag(self.credential.account_tag.as_str());
        tunnel_auth_builder.set_tunnel_secret(self.credential.tunnel_secret.as_ref());
        // Set tunnel id parameter.
        register_connection_request
            .get()
            .set_tunnel_id(self.credential.tunnel_id.as_ref());
        // Set connection index parameter.
        register_connection_request
            .get()
            .set_conn_index(self.connection_index);
        // Set connection options parameter (some options have been omitted).
        let mut connection_options_builder = register_connection_request.get().init_options();
        let mut client_builder = connection_options_builder.reborrow().init_client();
        client_builder.set_client_id(Uuid::new_v4().as_ref());
        client_builder.set_features(&[
            "serialized_headers",
            "support_datagram_v2",
            "support_quic_eof",
            "management_logs",
        ])?; // todo: research what each feature does https://github.com/cloudflare/cloudflared/blob/master/features/features.go)
        client_builder.set_version(format!("libcfd_{}", env!("CARGO_PKG_VERSION")));
        client_builder.set_arch(format!("{OS}_{ARCH}")); // todo: map OS & ARCH to match go (https://github.com/golang/go/blob/master/src/go/build/syslist.go)
        connection_options_builder.set_replace_existing(true);
        connection_options_builder.set_compression_quality(0);
        let register_connection_response = register_connection_request.send().promise.await?;
        let register_connection_response = register_connection_response.get()?.get_result()?;
        match register_connection_response.get_result().which()? {
            connection_response::result::Which::ConnectionDetails(Ok(reader)) => {
                Ok(ConnectionDetails {
                    location_name: reader.get_location_name()?.to_string()?,
                    uuid: Uuid::from_slice(reader.get_uuid()?)?,
                })
            }
            connection_response::result::Which::ConnectionDetails(Err(error)) => {
                Err(Error::from(error))
            }
            connection_response::result::Which::Error(result) => {
                Err(anyhow!("{}", result?.get_cause()?.to_string()?))
            }
        }
    }
}

pub struct ConnectionDetails {
    pub location_name: String,
    pub uuid: Uuid,
}
