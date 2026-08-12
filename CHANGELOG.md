# Changelog

## Unreleased

### Code-quality improvement campaign

- Feature-gated optional dependencies: `quiche` (and its BoringSSL backend) is
  now only compiled with the `quic-edge` feature, `h2` only with `h2-edge`, and
  `axum` only with `axum-origin`. H2-only builds no longer pull in BoringSSL;
  `getrandom` replaced the last non-QUIC use of `boring::rand`.
- Stripped unused dependencies and trimmed the tokio feature set to what libcfd
  actually uses (verified with cargo-machete).
- Reorganized modules into smaller files (the largest is now `src/serve.rs` at
  386 lines) and factored the repeated feature predicates into build.rs-emitted
  cfgs (`any_tunnel`, `any_edge`, `edge_conn`, `quic_any`, `h2_any`).
- Pruned redundant tests and deduplicated the loopback mock-edge helpers.

### Live-edge testing

- Added `tests/live_edge.rs` (ignored by default): it creates a quick tunnel
  through the real Cloudflare API or reuses the last one, whose credentials are
  stored gitignored in `.test-creds/quick-tunnel.json`, and verifies
  registration and HTTP serving against the real edge. `scripts/live-test.sh`
  runs the suite.

### libcfd-rpc 0.1

- Removed four never-used public items (possible break for any external
  consumers of libcfd-rpc): `RpcError::Unimplemented`,
  `rpc::quic::FLOW_ID_KEY`, `rpc::quic::TRACE_ID_KEY`, and
  `rpc::quic::encode_connect_request_bytes`. The wire-level `Unimplemented`
  reply is preserved via the capnp schema type.
