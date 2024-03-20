# LibCFD

A port of [cloudflared](https://github.com/cloudflare/cloudflared) (Cloudflare Tunnel Client) to Rust.

# Note

The current version is merely a proof of concept, and I am currently in the process of rewriting the whole crate with documentation, and a much more reasonable interface based on http-rs. My exams end on May 16, so I'll try to get it done by the end of may. :)
# Purpose

LibCFD intends to be lightweight and programmable replacement for cloudflared and its command line wrappers.
The main advantage of LibCFD is that we don't have to spawn a whole new process (that uses a garbage collected runtime) to connect to Cloudflare.
Thanks to this, we can save on resources by removing the IPC and GC overheads.

# Features

The currently supported features are

- [ ] Named tunnel (Zero Trust Network Access)
- [x] Quick tunnel (TryCloudflare)
- [x] QUIC tunnel connection
- [ ] HTTP tunnel connection (non-priority)
- [ ] Proper client information reporting (priority)
- [ ] Tunnel reconnection
- [x] HTTP connection (kind of works, but is a footgun)
- [ ] HTTP connection wrapper 
- [x] Websocket connection (kind of works, but is an even bigger footgun)
- [ ] Websocket connection wrapper 
- [ ] TCP connection (gated by named tunnel)
- [ ] Remote management (gated by named tunnel)

# To Do

Aside form those already listed above, these two are some to dos I am lookning forward to

- Fix async (some futures are !Send due to capnp, there may be a need to modify capnp)
- Decouple Tokio
- Clean up dependencies

# Performance

Work in progress. Seems to be able to hit 1gbps at least.

# Examples

## Http Hello World

Creates a HTTP server that sends simple "hello world" response.

```sh
cargo run --example http_hello_world
```

## Http Download

Creates a HTTP server that sends data as fast as possible.

```sh
cargo run --example http_download
```

## Http Download

Creates a HTTP server that receives data as fast as possible.

```sh
cargo run --example http_upload
```

## Websocket Echo

Creates a WebSocket server that echos all received Websocket messages.

```sh
cargo run --example websocket_echo
```
