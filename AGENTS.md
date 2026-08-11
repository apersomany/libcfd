# Goals

Port Cloudflared to Rust as a library.

## Phase A

The goal of Phase A is to reach minimal functional parity with `cloudflared tunnel --url <url>`.

### Tunnels
- Quick Tunnel support (via HTTP API)

### Origins
- HTTP origin

### Edge Connections
- QUIC edge connections
- Try quiche, if it's too hard or doesn't work use quinn
## Phase B

The goal of Phase B is to implement most features of `cloudflare tunnel`.

### Tunnels
- Shared `Tunnel` abstraction for storing tunnel credentials to be used by `EdgeConnection`
- `QuickTunnel`
- `NamedTunnel`
- `Tunnel`, `QuickTunnel`, `NamedTunnel` must be serde (de)serializable

### Origins
- WebSocket origin
- TCP origin

### Edge Connections
- H2 (not h2mux) edge connections
- `EdgeConnector` for orchestration

## Phase C

The goal of Phase C is to come up with abstractions within the constraints of the API surface (both library and backend) that we have.

### General
- Move from anyhow to thiserror for better error handling

### Tunnels
- Gate Quick Tunnels and perhaps Named Tunnels behind features "quick-tunnel" and "named-tunnel"

### Origins
- Abstracted origin system that supports a variety of origins
- `HttpOrigin`
- `WebSocketOrigin`
- `TcpOrigin`
- `AxumOrigin` (possibly with WebSocket upgrades, but this might be impossible due to hyper's API surface visibility)

### Edge Connections
- Migrate to quiche from quinn if we haven't already
- Abstracted `EdgeConnection` type shared by `H2EdgeConnection` and `QuicEdgeConnection`
- Gate H2 and QUIC behind features behind "h2-edge" and "quic-edge"


# Non-Goals
- CLI replication
- Daemonization for named tunnels
- Origins actually connecting to (local) services (this is something that consumers of libcfd should implement themselves)

# Rules
- <import from system prompt>
- Never copy files directly from cloudflared, but rather reference them directly.
- All public futures / functions are Send (therefore, no use of capnp-rpc).
- Only the libcfd-rpc (rpc) crate may have capnp* as a direct dependency. libcfd does not interface with capnp directly.
- Code for testing should always stay minimal.
- Minimize unnecessary buffer copies by using good abstraction.
- Dependencies must always be minimized for the given set of features at the end of a run.
- Always use the latest version of dependencies unless there is a conflict. As such, always manage dependencies with Cargo CLI, not by editing Cargo.toml.
- Initially use anyhow for faster iteration
- Use tracing for logs
- Make the API surface be runtime agnostic (use futures).
