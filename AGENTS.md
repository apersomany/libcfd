# Project objective

Port the Cloudflare Tunnel behavior needed by library consumers from `cloudflared` to Rust.

`libcfd` is a library, not a CLI or daemon. Its public API must let consumers create tunnels, establish edge connections, and handle origin traffic without adopting a particular async runtime.

# Reference implementation

The `research/cloudflared/` checkout is the behavioral and protocol reference.

- Inspect its source to understand behavior, data formats, state transitions, and protocol details.
- Compile, run, instrument, and test it when static analysis is insufficient.
- Do not copy its files or substantial source into this workspace. Implement the behavior independently in Rust.
- Conventional build-integration boilerplate, such as a `capnpc` build script, may be adapted from authoritative documentation or examples. Preserve attribution and licensing when required.
- Treat `research/cloudflared/` as read-only unless the task explicitly asks to modify it.

# Roadmap

Complete phases in order. Later-phase requirements should inform current designs, but must not cause speculative abstractions or broaden the active phase.

## Phase A: Quick Tunnel over QUIC

Reach library-level functional parity with `cloudflared tunnel --url <url>` for HTTP traffic. Parity means the library can:

- create a Quick Tunnel through the HTTP API;
- discover and connect to a Cloudflare edge over QUIC;
- register and maintain the tunnel connection;
- deliver incoming HTTP requests to a consumer-provided origin handler; and
- return the handler's HTTP responses to the edge.

Use `quiche` first. Switch to `quinn` only after identifying a concrete blocker and confirming the change with the user.

## Phase B: Named tunnels and additional transports

### Tunnels

- Introduce a shared `Tunnel` abstraction containing the credentials needed by edge connections.
- Add `QuickTunnel` and `NamedTunnel`.
- Make `Tunnel`, `QuickTunnel`, and `NamedTunnel` serializable and deserializable with Serde.

### Origins

- Add consumer-provided WebSocket and TCP origin handlers.

### Edge connections

- Add HTTP/2 edge connections, not h2mux.
- Add `EdgeConnector` to orchestrate edge discovery, connection establishment, retries, and transport selection.

## Phase C: Stable abstractions

Refine the API after the supported backend behavior and constraints are understood.

### General

- Replace broad `anyhow` errors with typed `thiserror` errors at stable boundaries.
- Preserve compatibility where practical while consolidating abstractions.

### Tunnels

- Gate Quick Tunnel support behind `quick-tunnel`.
- Evaluate whether Named Tunnel support should be gated behind `named-tunnel`.

### Origins

- Introduce a shared origin abstraction.
- Provide `HttpOrigin` and `StreamOrigin` (websocket and TCP responder variants) implementations or adapters.
- Explore an `AxumOrigin`, including WebSocket upgrades, only if Hyper and Axum expose the required API safely.

### Edge connections

- Use `quiche` instead of `quinn` unless Phase A established a documented blocker.
- Introduce a shared `EdgeConnection` abstraction for `H2EdgeConnection` and `QuicEdgeConnection`.
- Gate transports behind `h2-edge` and `quic-edge`.

# Non-goals

- Replicating the `cloudflared` CLI.
- Daemonizing named tunnels.
- Dialing local origin services from `libcfd`. Consumers own origin I/O and provide handlers or adapters to the library.
- Porting unrelated `cloudflared` features before the active phase requires them.

# Architecture constraints

- Keep the public API async-runtime agnostic. Do not expose Tokio, async-std, or another executor's concrete types.
- Every future exposed or returned by a public API must implement `Send`.
- Do not use `capnp-rpc`, because its futures do not satisfy the `Send` requirement.
- Only `libcfd-rpc` may depend directly on `capnp` crates. The main `libcfd` crate must interact with RPC through types and APIs exposed by `libcfd-rpc`.
- Keep origin handling transport-neutral where the active phase permits it, but do not introduce Phase C abstractions prematurely.
- Avoid unnecessary payload copies. Prefer borrowing, ownership transfer, or shared buffers across protocol and origin boundaries.
- Use `tracing` for diagnostics. Library code must not initialize a global subscriber.
- Never log credentials, tunnel tokens, private keys, or request authorization data.

# Implementation rules

- Implement only the active phase and the smallest supporting surface needed for the task.
- Prefer safe Rust. If unsafe code is unavoidable, keep it isolated and explain the invariant in a single concise comment.
- Use `anyhow` for rapid internal iteration during Phases A and B. Do not expose internal context strings as a compatibility contract.
- Keep tests focused and their support code minimal. Test observable behavior rather than duplicating implementation details.
- Use the Cargo CLI for dependency changes, including `cargo add` and `cargo remove`; do not edit dependency entries in `Cargo.toml` manually.
- Add the latest compatible dependency release unless a concrete compatibility issue requires a pin or older version.
- Minimize dependencies and feature activation. Before finishing a task, remove unused dependencies and avoid enabling features not required by the implemented behavior.
- Do not modify generated files when the schema or generation step can be changed instead.

# Validation

For every Rust code change, run:

```text
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Run narrower tests during iteration when useful, but they do not replace the final checks. Documentation-only changes do not require Rust validation. If an environmental or upstream issue prevents a check, report the exact command, failure, and remaining risk.
