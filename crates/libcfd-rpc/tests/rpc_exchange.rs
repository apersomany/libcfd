//! The complete registration lifecycle over an in-memory stream: bootstrap,
//! register (success, permanent/retryable rejection, remote exceptions,
//! wrong answer ids, unexpected message kinds, EOF, garbage), unregister,
//! and capability release on close.

use libcfd_rpc::RpcClient;
use libcfd_rpc::tunnel::{
    ClientInformation, ConnectionOptions, ConnectionResponse, RegistrationFailure, TunnelAuth,
    TunnelClient,
};

mod common;
use common::{
    Received, client_stream, serve_mock, serve_mock_bootstrap_exception,
    serve_mock_close_after_bootstrap, serve_mock_exception, serve_mock_garbage,
    serve_mock_register_error, serve_mock_unexpected_kind, serve_mock_with_release,
    serve_mock_wrong_answer,
};

fn auth() -> TunnelAuth {
    TunnelAuth {
        account_tag: "account-tag-123".into(),
        tunnel_secret: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
    }
}

fn options() -> ConnectionOptions {
    ConnectionOptions {
        client: ClientInformation {
            client_identifier: b"0123456789abcdef".to_vec(),
            features: vec!["allow_remote_config".into(), "support_datagram_v2".into()],
            version: "2026.7.3".into(),
            arch: "linux/amd64".into(),
        },
        origin_local_ip: vec![10, 0, 0, 1],
        replace_existing: false,
        compression_quality: 0,
        number_previous_attempts: 1,
    }
}

#[test]
fn tunnel_client_registers_with_mock_edge() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client
                .register_connection(auth(), &(0..16).collect::<Vec<u8>>(), 0, &options())
                .await
                .unwrap()
        };
        let server_fut = async move { serve_mock(server_stream).await };

        let (client_res, (received, decoded)) = futures::future::join(client_fut, server_fut).await;

        match client_res {
            ConnectionResponse::Details(details) => {
                assert_eq!(details.uuid, (1..=16).collect::<Vec<u8>>());
                assert_eq!(details.location_name, "lhr");
                assert!(!details.tunnel_is_remotely_managed);
            }
            other => panic!("expected connection details, got {other:?}"),
        }

        // The client should have done bootstrap(0) -> finish(0) -> call(0,
        // registerConnection) -> finish(1).
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

        // The register call must carry every credential and option.
        let decoded = decoded.expect("mock server decoded register parameters");
        assert_eq!(decoded.account_tag, "account-tag-123");
        assert_eq!(
            decoded.tunnel_secret,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(decoded.tunnel_identifier, (0..16).collect::<Vec<u8>>());
        assert_eq!(decoded.connection_index, 0);
        assert_eq!(
            decoded.options.client.client_identifier,
            b"0123456789abcdef"
        );
        assert_eq!(
            decoded.options.client.features,
            vec!["allow_remote_config", "support_datagram_v2"]
        );
        assert_eq!(decoded.options.client.version, "2026.7.3");
        assert_eq!(decoded.options.client.arch, "linux/amd64");
        assert_eq!(decoded.options.origin_local_ip, vec![10, 0, 0, 1]);
        assert!(!decoded.options.replace_existing);
        assert_eq!(decoded.options.compression_quality, 0);
        assert_eq!(decoded.options.number_previous_attempts, 1);
    });
}

#[test]
fn tunnel_client_permanent_registration_failure_is_typed() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client
                .register_connection(auth(), &[0u8; 16], 0, &ConnectionOptions::default())
                .await
                .unwrap()
        };
        let server_fut =
            async move { serve_mock_register_error(server_stream, "EDUPCONN", 0, false).await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        let failure = client_res.into_result().unwrap_err();
        match failure {
            RegistrationFailure::Permanent(cause) => assert_eq!(cause, "EDUPCONN"),
            other => panic!("expected permanent EDUPCONN, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_retryable_registration_failure_is_typed() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client
                .register_connection(auth(), &[0u8; 16], 0, &ConnectionOptions::default())
                .await
                .unwrap()
        };
        let server_fut = async move {
            serve_mock_register_error(server_stream, "UPSTREAM", 5_000_000_000, true).await
        };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        let failure = client_res.into_result().unwrap_err();
        match failure {
            RegistrationFailure::Retryable { cause, retry_after } => {
                assert_eq!(cause, "UPSTREAM");
                assert_eq!(retry_after, 5_000_000_000);
            }
            other => panic!("expected retryable UPSTREAM, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_remote_exception_becomes_typed_error() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client
                .register_connection(auth(), &[0u8; 16], 0, &ConnectionOptions::default())
                .await
        };
        let server_fut = async move { serve_mock_exception(server_stream, "boom").await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        match client_res {
            Err(libcfd_rpc::RpcError::RemoteCall(reason)) => assert_eq!(reason, "boom"),
            other => panic!("expected remote call error, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_bootstrap_exception_becomes_typed_error() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await
        };
        let server_fut =
            async move { serve_mock_bootstrap_exception(server_stream, "denied").await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        match client_res {
            Err(libcfd_rpc::RpcError::RemoteCall(reason)) => assert_eq!(reason, "denied"),
            other => panic!("expected remote call error, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_wrong_answer_identifier_fails_safely() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client
                .register_connection(auth(), &[0u8; 16], 0, &ConnectionOptions::default())
                .await
        };
        let server_fut = async move { serve_mock_wrong_answer(server_stream).await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        match client_res {
            Err(libcfd_rpc::RpcError::Protocol(message)) => {
                assert!(
                    message.contains("answer id"),
                    "unexpected message {message}"
                );
            }
            other => panic!("expected protocol error, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_unexpected_message_kind_fails_safely() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client
                .register_connection(auth(), &[0u8; 16], 0, &ConnectionOptions::default())
                .await
        };
        let server_fut = async move { serve_mock_unexpected_kind(server_stream).await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        match client_res {
            Err(libcfd_rpc::RpcError::Protocol(message)) => {
                assert!(
                    message.contains("expected return"),
                    "unexpected message {message}"
                );
            }
            other => panic!("expected protocol error, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_garbage_after_bootstrap_fails_safely() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client
                .register_connection(auth(), &[0u8; 16], 0, &ConnectionOptions::default())
                .await
        };
        let server_fut = async move { serve_mock_garbage(server_stream, &[0xff; 32]).await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        match client_res {
            Err(libcfd_rpc::RpcError::Protocol(_)) => {}
            other => panic!("expected protocol error, got {other:?}"),
        }
    });
}

#[test]
fn tunnel_client_eof_after_bootstrap_fails_safely() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            client
                .register_connection(auth(), &[0u8; 16], 0, &ConnectionOptions::default())
                .await
        };
        let server_fut = async move { serve_mock_close_after_bootstrap(server_stream).await };

        let (client_res, _received) = futures::future::join(client_fut, server_fut).await;
        match client_res {
            Err(libcfd_rpc::RpcError::Eof) => {}
            other => panic!("expected eof, got {other:?}"),
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
            client.unregister_connection().await
        };
        let server_fut = async move { serve_mock(server_stream).await };

        let (client_res, (received, _decoded)) =
            futures::future::join(client_fut, server_fut).await;
        client_res.expect("unregister should succeed");

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

#[test]
fn tunnel_client_into_inner_skips_release() {
    futures::executor::block_on(async {
        let (client_stream, server_stream) = client_stream();

        let client_fut = async move {
            let rpc = RpcClient::new(client_stream);
            let mut client = TunnelClient::new(rpc);
            client.bootstrap().await.unwrap();
            let stream = client.into_inner().into_inner();
            drop(stream);
        };
        let server_fut = async move { serve_mock_with_release(server_stream).await };

        let (_client_res, received) = futures::future::join(client_fut, server_fut).await;
        // into_inner must hand the stream back without releasing the
        // bootstrapped capability.
        assert_eq!(received, vec![Received::Bootstrap,]);
    });
}
