//! External-consumer style tests for the public API.
//!
//! These run offline with plain `cargo test`: no network access, no
//! credentials, no ignored tests. They use only the public surface of
//! `libcfd` the way an external consumer would, and add compile-time
//! `Send` checks for every public future.

#![cfg(any_tunnel)]

fn assert_send_sync<T: Send + Sync>() {}

/// A `Tunnel` for compile-time checks; defined for whichever tunnel feature
/// is enabled (never both, so no shadowing).
#[cfg(all(feature = "quick-tunnel", edge_conn))]
fn send_check_tunnel() -> libcfd::Tunnel {
    libcfd::Tunnel::quick(libcfd::QuickTunnel {
        tunnel_identifier: "6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f".into(),
        name: String::new(),
        hostname: "example.trycloudflare.com".into(),
        account_tag: "tag".into(),
        secret: vec![1, 2, 3],
    })
}

#[cfg(all(feature = "named-tunnel", not(feature = "quick-tunnel"), edge_conn))]
fn send_check_tunnel() -> libcfd::Tunnel {
    libcfd::Tunnel::named(libcfd::NamedTunnel {
        account_tag: "tag".into(),
        tunnel_secret: vec![1, 2, 3],
        tunnel_identifier: "6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f".into(),
        endpoint: None,
    })
}

/// Every public future must be `Send`; this is a compile-time guarantee the
/// crate commits to. The futures are constructed but never polled, so no
/// I/O happens.
#[cfg(any(edge_conn, feature = "quick-tunnel"))]
#[test]
fn public_futures_are_send() {
    fn assert_send<T: Send>(_value: T) {}

    #[cfg(feature = "quick-tunnel")]
    {
        let options = libcfd::QuickTunnelOptions::default();
        assert_send(libcfd::create_quick_tunnel(&options));
    }
    #[cfg(all(feature = "quick-tunnel", quic_any))]
    {
        let tunnel = libcfd::QuickTunnel {
            tunnel_identifier: "6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f".into(),
            name: String::new(),
            hostname: "example.trycloudflare.com".into(),
            account_tag: "tag".into(),
            secret: vec![1, 2, 3],
        };
        let origin = |_request: libcfd::Request, respond: libcfd::HttpResponder| {
            respond.send(libcfd::Response::new(
                http::StatusCode::OK,
                http::HeaderMap::new(),
                libcfd::Body::empty(),
            ));
        };
        let shutdown = async {};
        let options = libcfd::RunOptions::default();
        assert_send(libcfd::run_quick_tunnel(tunnel, origin, shutdown, &options));
    }
    #[cfg(edge_conn)]
    {
        let tunnel = send_check_tunnel();
        let origin = libcfd::Origin::http(
            |_request: libcfd::Request, respond: libcfd::HttpResponder| {
                respond.send(libcfd::Response::new(
                    http::StatusCode::OK,
                    http::HeaderMap::new(),
                    libcfd::Body::empty(),
                ));
            },
        );
        let shutdown = async {};
        let connector = libcfd::EdgeConnector::new(libcfd::EdgeOptions::default());
        assert_send(connector.run(tunnel, origin, shutdown));
    }
}

/// The public error and RPC types must be `Send + Sync`.
#[test]
fn public_error_types_are_send_and_sync() {
    assert_send_sync::<libcfd::Error>();
    assert_send_sync::<libcfd_rpc::RpcError>();
    assert_send_sync::<libcfd_rpc::tunnel::RegistrationFailure>();
}

/// Quick tunnel credentials deserialize exactly like cloudflared's cached
/// state, including the `Tunnel` enum tag.
#[cfg(feature = "quick-tunnel")]
#[test]
fn quick_tunnel_deserializes_like_cloudflared_state() {
    let json = r#"{"tunnel_id":"6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f","name":"","hostname":"x.trycloudflare.com","account_tag":"a","secret":"c2VjcmV0"}"#;
    let tunnel: libcfd::QuickTunnel = serde_json::from_str(json).unwrap();
    assert_eq!(tunnel.url(), "https://x.trycloudflare.com");
    assert_eq!(tunnel.tunnel_identifier_bytes().unwrap().len(), 16);

    let json = r#"{"type":"quick","tunnel_id":"6ea05ba1-9e0e-4f0d-9e9e-3d0f0f0f0f0f","name":"","hostname":"x.trycloudflare.com","account_tag":"a","secret":"c2VjcmV0"}"#;
    let tunnel: libcfd::Tunnel = serde_json::from_str(json).unwrap();
    assert_eq!(tunnel.hostname(), Some("x.trycloudflare.com"));
    assert_eq!(tunnel.account_tag(), "a");
}

/// The dashboard connector token and the credentials-file loader agree on
/// the same tunnel identity.
#[cfg(feature = "named-tunnel")]
#[test]
fn named_tunnel_token_and_credentials_file_flow() {
    use base64::Engine as _;
    let payload =
        br#"{"a":"abc123","s":"dG9wLXNlY3JldA==","t":"550e8400-e29b-41d4-a716-446655440000"}"#;
    let token = base64::engine::general_purpose::STANDARD.encode(payload);
    let tunnel = libcfd::NamedTunnel::from_token(&token).unwrap();
    assert_eq!(tunnel.account_tag, "abc123");
    assert_eq!(tunnel.tunnel_secret, b"top-secret");
    assert_eq!(tunnel.endpoint, None);

    let path = std::env::temp_dir().join("libcfd-public-api-named-credentials.json");
    std::fs::write(&path, serde_json::to_vec(&tunnel).unwrap()).unwrap();
    let reloaded = libcfd::NamedTunnel::from_credentials_file(&path).unwrap();
    assert_eq!(reloaded.tunnel_identifier, tunnel.tunnel_identifier);
    assert_eq!(reloaded.account_tag, tunnel.account_tag);
    assert_eq!(reloaded.tunnel_secret, tunnel.tunnel_secret);
    std::fs::remove_file(&path).ok();
}

/// The default local configuration payload is valid ingress JSON.
#[cfg(edge_conn)]
#[test]
fn default_configuration_json_is_valid_ingress() {
    let value: serde_json::Value =
        serde_json::from_str(libcfd::default_configuration_json()).unwrap();
    assert!(value["ingress"].is_array());
}

/// The default transport matches the enabled edge features.
#[cfg(edge_conn)]
#[test]
fn default_transport_matches_enabled_features() {
    let options = libcfd::EdgeOptions::default();
    #[cfg(all(quic_any, feature = "h2-edge"))]
    assert_eq!(options.transport, libcfd::Transport::Auto);
    #[cfg(all(quic_any, not(feature = "h2-edge")))]
    assert_eq!(options.transport, libcfd::Transport::Quic);
    #[cfg(all(not(quic_any), feature = "h2-edge"))]
    assert_eq!(options.transport, libcfd::Transport::H2);
}
