# Cloudflared Reference Research Briefs

Collected research from four scout runs against the `research/cloudflared/` reference checkout (commit `61a0b0b3`, 2026-08-11) at `/home/aperso/libcfd/research/cloudflared`. Source: async subagent workflow run `6d821c8c-a64c-4996-924c-f9a7dd58e5b4` (scout run ids: `ae969f7b` quic, `b0092078` quicktunnel, `f5347342` rpc, `b9985400` h2origins).

| File | Topic |
|---|---|
| [quic.md](quic.md) | QUIC edge connection transport |
| [quicktunnel.md](quicktunnel.md) | Quick Tunnel creation and edge discovery |
| [rpc.md](rpc.md) | Tunnel registration RPC protocol (capnp) |
| [h2origins.md](h2origins.md) | HTTP/2 edge transport, origin handlers, credentials, reconnect |

## Summaries

### quic.md
Documents the QUIC edge transport end-to-end: ALPN `argotunnel` with SNI `quic.cftunnel.com`, QUIC v1 with no version pinning or custom transport parameters, and the TLS client config used to dial the edge. Covers the control stream registration flow (first client stream, capnp RPC with no signature bytes — asymmetric with data streams), request stream framing (6-byte magic signature `{0x0A,0x36,0xCD,0x12,0xA1,0x3E}` + 2-byte version `"01"` + raw capnp `ConnectRequest`), and QUIC config values (keepalive 1s, idle 5s, max streams 2^60, 30MiB/6MiB flow-control windows, no 0-RTT). Notes that quic-go here is a vendored fork of v0.45, which matters for feature matching in a Rust stack.

### quicktunnel.md
Covers quick tunnel creation via the HTTP API (`RunQuickTunnel` in `cmd/cloudflared/tunnel/quick_tunnel.go`, `POST /tunnels` with a JSON account request and `cf-ray`-based try-later handling) and edge discovery via DNS SRV (`_quick-tunnel._tcp.cfargotunnel.com`). Notes what is absent from this checkout: no `run.go`, no `/cdn-cgi/trace` flow, and no `TryCloudflare` API usage. Includes the `QuickTunnel` credential/token response shape and per-region edge address selection.

### rpc.md
Details the tunnel registration RPC protocol: the three `.capnp` schemas (`tunnelrpc.capnp`, `quic_metadata_protocol.capnp`, plus the vendored `rpc.capnp`), the RPC message framing and transport (capnp RPC over raw QUIC streams with no HTTP/3), and the message order on the control stream (`registerConnection` → `updateLocalConfiguration` → block → `unregisterConnection`). Documents the standard capnp RPC bootstrap exchange, method naming (ABI-stable `"registerConnection"`), per-method timeouts (5s default), `EDUPCONN` duplicate detection, and `ConnectionOptions`/`RegistrationOptions` field sets including feature strings like `support_datagram_v3_2`.

### h2origins.md
Covers the HTTP/2 edge transport (cloudflared serves x/net/http2 server side over raw TLS to port 7844 with SNI `h2.cftunnel.com` and no ALPN), origin HTTP request handling (`connection.go` buildOriginRequest / serveHTTP, `proxy/` hostname rules), WebSocket upgrades via `Upgrade:` handling and the `cf-connectio`-style hop-by-hop header stripping, and TCP origin handling via `proxy/tcp.go` (streams wrapped in raw capnp `ConnectRequest` with TCP metadata, `tcpOverWS` opt-in flag). Includes tunnel credentials shape (`TunnelSecret` as base64 `[]byte`, JSON field names) and reconnect/backoff behavior in `supervisor/tunnel.go` (retry with 1s initial backoff).
