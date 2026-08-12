use libcfd_rpc::RpcClient;
use libcfd_rpc::tunnel::{ClientInfo, ConnectionOptions, TunnelAuth, TunnelClient};

mod common;
use common::{Received, client_stream, serve_mock, serve_mock_err, serve_mock_with_release};

#[test]
fn tunnel_client_registers_with_mock_edge() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            let auth = TunnelAuth {
                account_tag: "account-tag-123".into(),
                tunnel_secret: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            };
            let tunnel_id: Vec<u8> = (0..16).map(|i| i as u8).collect();
            let options = ConnectionOptions {
                client: ClientInfo {
                    client_id: b"0123456789abcdef".to_vec(),
                    features: vec!["allow_remote_config".into(), "support_datagram_v2".into()],
                    version: "2026.7.3".into(),
                    arch: "linux/amd64".into(),
                },
                origin_local_ip: vec![10, 0, 0, 1],
                replace_existing: false,
                compression_quality: 0,
                num_previous_attempts: 1,
            };
            client
                .register_connection(auth, &tunnel_id, 0, &options)
                .await
                .unwrap()
        };
        let server_fut = async move { serve_mock(server_stream).await };

        let (_client_res, received) = futures::future::join(client_fut, server_fut).await;

        // The client should have done bootstrap(0) -> finish(0) -> call(0, register) -> finish.
        assert_eq!(
            received,
            vec![
                Received::Bootstrap,
                Received::Finish { question_id: 0 },
                Received::Call {
                    interface_id: 0xf71695ec7fe85497,
                    method_id: 0
                },
                Received::Finish { question_id: 1 },
            ]
        );
    });
}

#[test]
fn tunnel_client_register_error_returns_connection_error() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            let auth = TunnelAuth {
                account_tag: "tag".into(),
                tunnel_secret: vec![1; 16],
            };
            client
                .register_connection(auth, &[0u8; 16], 0, &ConnectionOptions::default())
                .await
        };
        let server_fut = async move { serve_mock_err(server_stream).await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        let resp = client_res.unwrap();
        let failure = resp.into_result().unwrap_err();
        match failure {
            libcfd_rpc::tunnel::RegistrationFailure::Permanent(cause) => {
                assert_eq!(cause, "EDUPCONN");
            }
            other => panic!("expected permanent EDUPCONN, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_unregisters_with_mock_edge() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client.unregister_connection().await.unwrap();
        };
        let server_fut = async move { serve_mock(server_stream).await };

        let (_client_res, received) = futures::future::join(client_fut, server_fut).await;

        assert_eq!(
            received,
            vec![
                Received::Bootstrap,
                Received::Finish { question_id: 0 },
                Received::Call {
                    interface_id: 0xf71695ec7fe85497,
                    method_id: 1
                },
                Received::Finish { question_id: 1 },
            ]
        );
    });
}

#[test]
fn tunnel_client_close_releases_bootstrap_capability() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            let stream = client.close().await.unwrap();
            drop(stream);
        };
        let server_fut = async move { serve_mock_with_release(server_stream).await };

        let (_client_res, received) = futures::future::join(client_fut, server_fut).await;

        assert_eq!(received, vec![Received::Bootstrap, Received::Release,]);
    });
}
