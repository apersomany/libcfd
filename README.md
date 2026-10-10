# LibCFD

A Rust port of [cloudflared](https://github.com/cloudflare/cloudflared) (the Cloudflare Tunnel client), as a **library**, not a CLI or daemon. Consumers connect tunnels to the edge and supply origin handlers without spawning a separate tunnel-client process.

## Status and workspace

Version 0.3.0 supports quick and named tunnels, QUIC and HTTP/2 edge transports, edge discovery, retries and reconnection, HTTP/WebSocket/TCP origins, and an optional HTTP-only axum adapter. Public types are runtime-neutral, public futures are `Send`, and errors use `thiserror`.

- [`crates/libcfd`](crates/libcfd): public tunnel, edge, and origin APIs; examples and consumer/live tests.
- [`crates/libcfd-rpc`](crates/libcfd-rpc): Cap'n Proto schemas, wire handling, registration RPC, and wire/exchange tests. Only this crate depends on Cap'n Proto crates; it does not use `capnp-rpc`.

Use matching crate versions (the RPC crate is normally reached through `libcfd`):

```toml
[dependencies]
libcfd = "0.3.0"
# Only for direct low-level RPC consumers:
libcfd-rpc = "0.3.0"
```

## Features

Defaults are `quick-tunnel`, `named-tunnel`, `quic-edge`, and `h2-edge`. Use `--no-default-features` to select a smaller surface; `axum-origin` and `quic-edge-quiche` are opt-in.

| Feature | Provides |
|---|---|
| `quick-tunnel` | Quick tunnel API client and `QuickTunnel` |
| `named-tunnel` | `NamedTunnel`, credentials-file loading, and connector-token parsing |
| `quic-edge` | Enables `quic-edge-quinn` (quinn with rustls/ring) |
| `quic-edge-quinn` | Direct selection of the quinn backend |
| `quic-edge-quiche` | quiche with BoringSSL; takes precedence over quinn |
| `h2-edge` | HTTP/2 edge transport |
| `axum-origin` | HTTP-only axum `Router` adapter |

QUIC feature flags can coexist; only one backend is selected. Default builds use quinn; `--all-features` selects quiche, not both backends. `EdgeConnector` requires a tunnel feature and an edge transport. `Transport::Auto` is available only with QUIC and HTTP/2 enabled; QUIC-only runs do not fall back to HTTP/2.

## Origin and runtime contracts

`HttpOrigin::handle` and `StreamOrigin::connect` are intentionally synchronous dispatch methods. Respond immediately or transfer the owned, single-use responder to work scheduled by the consumer; do not block waiting for origin I/O. HTTP responds with `Response`, WebSocket with a `WebSocketConnection` (101 handshake plus raw bidirectional stream), and TCP with a `Stream` alone (the transport sends the acknowledgement). Dropping a responder without responding reports a missing-response error. Bodies and streams use `futures_io` traits and require `Send`; handlers are `Send + Sync`.

Runtime-neutral types do **not** mean executor-independent network execution: quick-tunnel creation and edge runs require an active Tokio runtime with I/O and time enabled. The library uses Tokio sockets, timers, and internal tasks; it does not create a runtime for callers. Consumers own scheduling of their asynchronous origin work. The optional `AxumOrigin` adapter schedules router work with `tokio::spawn`. Examples use `#[tokio::main]`. Diagnostics use `tracing`; the library never installs a global subscriber.

The typed RPC `TunnelClient` facade and lower-level `libcfd_rpc::RpcClient::call` return `Send` futures. Version 0.3.0 tightens `call` to require `Send` fill/decode callbacks and decoded output; non-`Send` calls accepted by 0.2.0 no longer compile.

Discovery and connection establishment are interruptible. Once connected, a shutdown signal retains ownership through transport unregister/drain/close attempts, with grace-period timeouts for those phases. Remaining library-owned request/control tasks are aborted and joined; cancellation/drop aborts owned tasks and closes connections rather than detaching cleanup. This does not guarantee every request completes, impose an overall wall-clock shutdown bound, or stop consumer-scheduled origin work. Dropping the run future is not graceful shutdown.

Credential types (`QuickTunnel`, `NamedTunnel`, `Tunnel`, and RPC `TunnelAuth`) redact registration secrets in `Debug`, retaining nonsecret metadata. Their raw fields and serialization still contain credentials; never serialize them into diagnostics or log tokens, private keys, or request authorization data.

TLS trust differs by connection: the quick-tunnel HTTPS API uses bundled `webpki-roots`, not the OS store. Edge transports load the first readable PEM bundle from `/etc/ssl/certs/ca-certificates.crt`, `/etc/pki/tls/certs/ca-bundle.crt`, or `/etc/ssl/cert.pem`, then append bundled Cloudflare origin roots and optional `ca_cert_pem`. With no readable system bundle, only bundled and supplied edge roots remain; there is no native platform trust-store API.

## Examples

Run from the workspace root inside `nix develop` (or prefix with `nix develop -c`). These commands use the network and serve until Ctrl-C:

```sh
cargo run -p libcfd --example quick_tunnel
cargo run -p libcfd --example h2_tunnel
cargo run -p libcfd --example origin_ws_tcp
cargo run -p libcfd --example axum_tunnel --features axum-origin
cargo run -p libcfd --example named_tunnel -- /path/to/credentials.json
```

The quick examples create a new tunnel each run; they do not use the live-test cache. `origin_ws_tcp` echoes raw streams (loopback TCP for WebSocket, an in-process stream pair for TCP). The axum adapter does not bridge WebSocket or TCP.

The named example also accepts a dashboard connector token as its argument, but a credentials-file path avoids putting a token in shell history or process arguments. Its default file is `credentials.json`. For remotely-managed tunnels it collects routed hostnames from `EdgeOptions::on_remote_configuration`, polls them to verify the origin, and continues serving. It does not read `NAMED_TUNNEL_TOKEN` or the live-test cache.

## Validation and testing

Nix owns normal validation; [CI](.github/workflows/ci.yml) is a thin wrapper running the same command:

```sh
nix flake check -L
```

The [flake](flake.nix) provides the Rust toolchain, Cap'n Proto compiler, and native QUIC build prerequisites. Its checks run secret hygiene and the four mandatory Cargo checks below, plus `cargo check --workspace --all-targets` for default features. Builds use offline Cargo after dependency provisioning; Nix may still fetch inputs/dependencies. The filtered validation source excludes research, hidden paths, live state, and build outputs. CI does not fetch the reference submodule or run live tests. This is not an exhaustive feature matrix: all-features exercises quiche; the extra default check compiles quinn.

For local iteration, enter `nix develop` and run:

```sh
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

`cargo test --workspace --all-features` runs offline unit, RPC wire/exchange, and external-consumer API tests (including compile-time `Send` checks). Ignored live-edge tests are compiled but not executed. Plain `cargo test` uses the default member `libcfd` and default features, not the entire workspace. No credentials or live-edge access are needed for the offline suite.

### Opt-in live tests

The integration target is `live` at [`crates/libcfd/tests/live/main.rs`](crates/libcfd/tests/live/main.rs); its live-edge cases are `#[ignore]` (helper unit tests remain offline). The removed scripts and `tests/README.md` are not entry points. Explicit examples below **use real networks and create/register tunnels**; never add `--ignored` to offline validation:

```sh
# Default features: quinn HTTP/WebSocket and HTTP/2 HTTP tests.
nix develop -c cargo test -p libcfd --test live quick:: -- --ignored --test-threads=1
# Named suite: requires NAMED_TUNNEL_TOKEN already set securely in the environment.
nix develop -c cargo test -p libcfd --test live named:: -- --ignored --test-threads=1
# Only the quiche quick-tunnel HTTP test (quiche takes precedence over quinn).
nix develop -c cargo test -p libcfd --test live --features quic-edge-quiche quick::live_quick_quic_quiche_serves_http -- --ignored --exact --test-threads=1
```

Use filters deliberately: an unfiltered ignored run includes named tests. Quick tests need `quick-tunnel` plus the selected transport; named tests need `named-tunnel` plus the transport. WebSocket and TCP-route tests currently compile only for quinn. Named HTTP tests require a remotely-managed tunnel with routed ingress hostnames; WebSocket tests require a WebSocket-capable route; TCP-route tests require a `tcp://` ingress service exposed through WebSocket. Missing prerequisites fail rather than skip. `RUST_LOG` controls the live helpers' maximum tracing level (default `INFO`).

Live support reads only `NAMED_TUNNEL_TOKEN` for named credentials; it does not read `tests/state/named-token.txt` or accept a credentials-file environment variable. It normalizes the token into workspace-root `tests/state/named_tunnel.json` and reloads that file. Quick credentials are cached in `tests/state/quick_tunnel.json`, reused while resolving, and replaced once after a stale cached run fails. Both suites hold `tests/state/.live.lock`; still run with `--test-threads=1`. These paths remain at the workspace root despite crate relocation, are gitignored, and contain real secrets: never print, attach, or commit their contents or tokens. Test helpers fall back to public DNS-over-HTTPS if local hostname resolution fails.

## Documentation and research

Generate API docs without executing examples:

```sh
nix develop -c cargo doc --workspace --all-features --no-deps
```

Entry points include `create_quick_tunnel`, `run_quick_tunnel` (HTTP-only over QUIC), and `EdgeConnector` (full origin/transport selection). [`research/README.md`](research/README.md) indexes historical protocol briefs from the read-only `research/cloudflared/` reference checkout; they describe the reference, not a current implementation checklist. Contributor invariants and validation requirements are in [AGENTS.md](AGENTS.md).
