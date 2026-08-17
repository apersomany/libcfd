# LibCFD

A port of [cloudflared](https://github.com/cloudflare/cloudflared) (the Cloudflare Tunnel client) to Rust, as a **library**.

# Purpose

LibCFD intends to be a lightweight, programmable replacement for cloudflared and its command-line wrappers. The main advantage of LibCFD is that a consumer application does not have to spawn a whole new process (with a garbage-collected runtime) to connect to Cloudflare, avoiding the IPC and GC overheads of an out-of-process tunnel client.

# Status

Current version: 0.2.0. The supported feature set is:

- [x] Quick tunnels (trycloudflare.com HTTP API)
- [x] Named tunnels (cloudflared credentials file or dashboard connector token; routed hostnames discovered from the edge's remote-configuration push via `EdgeOptions::on_remote_configuration`)
- [x] QUIC edge transport (quinn by default; quiche via the `quic-edge-quiche` feature)
- [x] HTTP/2 edge transport
- [x] Edge discovery, connection retries, transport selection, and reconnection with exponential backoff
- [x] Origin handlers: HTTP, WebSocket, TCP, and an axum `Router` adapter
- [x] Async-runtime-agnostic public API (`Send` futures, no Tokio types exposed)
- [x] Typed `thiserror` errors at the public boundary

# Feature gates

All features are enabled by default; disable them to slim the dependency tree.

| Feature | Provides |
|---|---|
| `quick-tunnel` | Quick tunnel API client and `QuickTunnel` type |
| `named-tunnel` | `NamedTunnel` and the credentials-file loader |
| `quic-edge` | QUIC edge transport. Defaults to the quinn backend (pure-Rust rustls/ring); enable `quic-edge-quiche` to use quiche (BoringSSL) instead. The backends are mutually exclusive; quiche wins when both are enabled |
| `h2-edge` | HTTP/2 edge transport |
| `axum-origin` | Adapter letting an axum `Router` serve as an HTTP origin |

# Examples

## Quick tunnel over QUIC

Creates a tunnel via trycloudflare.com and serves HTTP requests through a simple origin.

```sh
cargo run --example quick_tunnel
```

## Quick tunnel over HTTP/2

Same as above, using the HTTP/2 edge transport.

```sh
cargo run --example h2_tunnel
```

## Named tunnel

Runs a tunnel from a cloudflared credentials file or a dashboard connector token (the same token the Zero Trust dashboard shows for `cloudflared tunnel run --token`). For remotely-managed tunnels the edge pushes the tunnel's configuration after registration, so the example discovers the routed public hostnames via RPC and verifies them end-to-end before serving until Ctrl-C.

```sh
cargo run --example named_tunnel -- /path/to/credentials.json
cargo run --example named_tunnel -- <connector-token>
```

## WebSocket and TCP origins

Quick tunnel whose websocket and TCP origin handlers echo raw bytes.

```sh
cargo run --example origin_ws_tcp
```

## axum origin

Quick tunnel served through an axum `Router` (HTTP only).

```sh
cargo run --example axum_tunnel --features axum-origin
```

# Runtime notes

- The public API does not expose Tokio or any other executor's concrete types; callers drive the returned futures on their own runtime (execution uses Tokio internally).
- Every public future is `Send`.
- `tracing` is used for diagnostics; the library never installs a global subscriber.

# Documentation

## Testing

`cargo test` runs the offline suite: unit tests, RPC wire/exchange tests,
and external-consumer API tests (including compile-time `Send` checks on
public futures). No network access or credentials are needed.

Live-edge tests are `#[ignore]`d and run via:

```text
scripts/live-test.sh        # quick + named tunnels against the real edge
scripts/feature-matrix.sh   # compile/test every feature combination
scripts/check-secrets.sh    # verify credential hygiene in CI
```

See [`tests/README.md`](tests/README.md) for the full suite documentation
and the feature requirements of each live test.

The crate-level docs (`cargo doc --open`) describe the entry points (`create_quick_tunnel`, `run_quick_tunnel`, `EdgeConnector`) and the QUIC v1 / HTTP/2 edge protocol details. `research/` collects protocol research briefs gathered from the `research/cloudflared/` reference checkout.
