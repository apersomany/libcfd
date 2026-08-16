# Phase C Final Review

Reviewed: the 6 Phase C commits `2bb22ec` (start) through `7ebb06f` (HEAD):
`2bb22ec` phase B review fixes, `cf3d10e` feature gates, `f64d866` typed
errors, `7a10bf4` origin abstraction + AxumOrigin, `96db7bd` shared
EdgeConnection, `7ebb06f` docs/examples polish. Ground truth: the
cloudflared checkout at `/home/aperso/libcfd/research/cloudflared` (read-only) and
the prior reviews `review-a2.md` / `review-b.md`. `plan.md` and
`progress.md` do not exist in the repo (as in both prior phases); this
review is scoped by the commit list.

All commands ran inside `nix develop`. Exact results in section 9.

## 1. Feature gates

`Cargo.toml:9-20`: `default = ["quick-tunnel", "named-tunnel", "quic-edge",
"h2-edge"]`; `axum-origin = ["dep:axum", "dep:tower"]`.

Modules are gated as follows (`src/lib.rs`): `api` on `quick-tunnel`
(`:43`); `connector`/`control`/`edge` on
`any(quick-tunnel, named-tunnel) && any(quic-edge, h2-edge)` (`:45-59`);
`event` on `any(quic-edge, h2-edge)` (`:61`); `h2` on `h2-edge` alone
(`:63`); `quic` on `quic-edge` (`:66`); `roots` on
`any(quic-edge, h2-edge)` (`:68`); `run` on `quick-tunnel && quic-edge`
(`:70`); `serve` on `quic-edge` alone (`:72`); `tunnel` on
`any(quick-tunnel, named-tunnel)` (`:74`). Re-exports match the gates.

Required matrix — all pass (fresh `CARGO_TARGET_DIR` for the important
ones):

| Combo | Result |
|---|---|
| (a) `--all-features` | pass |
| (b) `--no-default-features --features quick-tunnel,quic-edge` | pass (fresh build) |
| (c) `--no-default-features --features named-tunnel,h2-edge` | pass (fresh build) |
| (d) `--no-default-features` | pass (fresh build) |
| (e) `--features axum-origin` | pass (fresh build; also isolated `--no-default-features --features axum-origin` passes) |

Extra single-feature combos (diligence beyond the required matrix):
`quick-tunnel` alone, `named-tunnel` alone, `quick-tunnel,named-tunnel`,
`quick-tunnel,h2-edge`, `named-tunnel,quic-edge`,
`quick-tunnel,quic-edge,h2-edge`, `quick-tunnel,named-tunnel,quic-edge`,
`named-tunnel,quic-edge,h2-edge` — all pass.

**Two gaps found in combos the developer's "feature matrix" claim did not
cover:**

- **Major — `h2-edge` alone does not compile.**
  `cargo check --workspace --all-targets --no-default-features --features h2-edge`
  fails with `E0432`: `src/h2/mod.rs:23` (`use crate::control::{...}`) and
  `src/h2/mod.rs:28` (`use crate::tunnel::Tunnel`) cannot find those
  modules, which are gated behind `any(quick-tunnel, named-tunnel)`
  (`src/lib.rs:50-54,75`). `mod h2` is gated on `h2-edge` alone
  (`src/lib.rs:63`). This is exactly the "references to gated items from
  ungated code" case the review focus calls out. Practical impact is low
  (with no tunnel feature there is no `Tunnel`/`EdgeConnector` public API),
  but the declared feature should not break the build. Fix: gate
  `mod h2` on `all(feature = "h2-edge", any(feature = "quick-tunnel",
  feature = "named-tunnel"))`.
- **Minor — `quic-edge` alone compiles but is dead-code-noisy.**
  `cargo check --workspace --all-targets --no-default-features --features quic-edge`
  emits 43 warnings (`serve` module gated on `quic-edge` alone at
  `src/lib.rs:72` is never called without a tunnel feature; likewise
  `event::Event`, `origin::Origin::http`, `pump`, the `quic` module
  constants `EDGE_SNI`/`EDGE_ALPN`/windows). `cargo clippy ... -D warnings`
  fails for that combo. Same root cause as above: transport modules assume a
  tunnel feature. Fix alongside the `mod h2` gate (gate `mod serve`
  similarly and add the missing `cfg_attr(allow(dead_code))` for the
  transport-only case).

Examples' `required-features` (`Cargo.toml:52-70`) match their imports:
`quick_tunnel` → `quick-tunnel,quic-edge`; `h2_tunnel` →
`quick-tunnel,h2-edge`; `named_tunnel` → `named-tunnel,quic-edge`;
`origin_ws_tcp` → `quick-tunnel,quic-edge`; `axum_tunnel` →
`quick-tunnel,quic-edge,axum-origin`. All examples build under
`--all-features` (verified) and are correctly skipped when their features
are absent (cargo's required-features semantics).

Tests are correctly gated: `loopback_test` on `quick-tunnel && any(edge)`
(`src/lib.rs:101`), per-test `#[cfg(feature = "...")]` for quic vs h2
bodies, `connector`/`tunnel`/`error` unit tests on their feature
intersections. No test references a gated item from ungated code under any
passing combo.

## 2. Typed errors

- `anyhow` is absent from `src/`, `rpc/src/`, `examples/`, and both
  `Cargo.toml`s (grep). It appears only transitively in `Cargo.lock`
  (capnpc-embedded/wasmtime codegen deps used by libcfd-rpc's build
  script). Verified.
- Every public entry point returns the typed thiserror `Error`
  (`src/error.rs`): `create_quick_tunnel` → `Result<QuickTunnel>`,
  `NamedTunnel::from_credentials_file` → `Result<NamedTunnel>`,
  `EdgeConnector::run` → `Result<()>`, `run_quick_tunnel` → `Result<()>`,
  all origin traits → `crate::error::Result`. `Error` has 14 variants,
  each with `#[error(...)]` display and `#[source]` where the cause is a
  real error.
- `From` conversions: `std::io::Error` → `Io`, `boring::ErrorStack` →
  `Tls`, `quiche::Error` → `Quic`, `h2::Error` → `H2`,
  `libcfd_rpc::RpcError` → `Control` (`#[from]`), `String` → `Origin`
  (convenience for consumer handlers). `error.rs:28-62`.
- Permanent-vs-retryable: `Error::is_permanent` (`error.rs:85`) maps
  `Registration(Permanent)`; the connector aborts the run only on that
  variant (`connector.rs:271`), and `EDUPCONN` is a separate
  `DuplicateConnection` variant that triggers "try next edge" instead
  (`connector.rs:265-268`). Unit-tested
  (`registration_failure_classifies_permanent`).
- No internal context strings as a compatibility contract: variants are
  typed; the String payloads are transport-level diagnostics, not anyhow
  chains. Acceptable per AGENTS.md Phase C.
- Nit: `Error::Shutdown` (`error.rs:48`) is never constructed by library
  code — shutdown completes with `Ok(())`. Dead public variant (only the
  test touches it).

## 3. Origin abstraction

- `Origin` (`src/origin/mod.rs:159-217`) is the shared transport-neutral
  dispatcher: required `http` handler, optional `websocket`/`tcp`,
  builder-style `with_websocket`/`with_tcp`. Both transports dispatch
  through it: QUIC in `src/serve.rs` (`handle_quic_http`/`_websocket`/
  `_tcp`, `serve.rs:126-186`) and H2 in `src/h2/mod.rs`
  (`handle_h2_http`/`_websocket`/`_tcp`, `h2/mod.rs:236-301`).
- `HttpOrigin`/`WebSocketOrigin`/`TcpOrigin` traits + object-safe `*Dyn`
  variants with `Send` boxed futures (`origin/mod.rs:97-155,413-432`); blanket
  `impl HttpOrigin for F` for closures. All `Send + Sync`.
- `Duplex` (`origin/mod.rs:28-58`) is runtime-agnostic: `ReadHalf`/
  `WriteHalf` are `Pin<Box<dyn futures_io::AsyncRead/AsyncWrite + Send>>`;
  split/rejoin available. No tokio types anywhere in the public origin
  surface.
- Consumers own origin I/O: `WebSocketOrigin::connect` and `TcpOrigin::connect`
  run the origin-side handshake/dial and return the `Duplex`; libcfd never
  dials a local origin service (documented in trait docs and the
  `origin_ws_tcp` example). Matches AGENTS.md.
- `pump` (`origin/mod.rs:350-405`) was rewritten with explicit
  half-close semantics and backpressure (write_all on both directions,
  `tokio::select!` gated branches): each direction closes only the
  destination write half on EOF, matching cloudflared's
  `PipeBidirectional`. Review-b's pump concern is resolved.

## 4. AxumOrigin

- `AxumOrigin` (`src/origin/axum.rs`, gated `axum-origin`) wraps an
  `axum::Router` as an `HttpOrigin` via body bridges:
  `BodyReadStream` (libcfd `Body` → `Result<Bytes>` chunks) and
  `AxumBodyReader` (`BodyDataStream` → `AsyncRead`). No `unsafe`, no
  panics on malformed input: the only `expect` (`axum.rs:104`) is on
  `body.as_mut()` where `body` is never actually set to `None` (EOF
  returns `Poll::Ready(None)` without clearing), so it is unreachable
  dead-defensive code. Malformed header values are handled with
  `.map_err`/`unwrap_or` paths. Backpressure: the request bridge reads
  on demand (axum pulls `poll_next` only when it can buffer), the
  response bridge is pull-driven via `futures_io::copy` bounded by the
  edge's flow control (quiche window / h2 `reserve_capacity`). The
  `content-length` hint is inserted from `Body::size_hint()` so axum
  body extractors see a length.
- Websocket-infeasibility documentation is **accurate**, verified against
  axum 0.8.9 source: `WebSocketUpgrade::on_upgrade(self, callback)` takes
  `C: FnOnce(WebSocket) -> Fut`, spawns its own task that drives the
  upgraded socket, and hands the callback a `WebSocket` whose API is
  message-framed (`recv() -> Option<Result<Message>>`, `send(Message)`,
  `Message` being the Text/Binary/Ping/Pong/Close enum). The raw byte
  duplex is never exposed to the caller, so it cannot feed libcfd's
  `WebSocketOrigin`. The adapter doc comment and the `axum_tunnel`
  example doc say exactly this.
- Gating and example: `axum-origin` is optional (`Cargo.toml:20,23-24`);
  `AxumOrigin` re-export gated (`lib.rs:83`); `axum_tunnel` has
  `required-features = ["quick-tunnel", "quic-edge", "axum-origin"]`
  (`Cargo.toml:70`) and compiles with them (verified). Tests
  (`axum_router_serves_through_the_adapter`, `unknown_route_returns_404`)
  pass.

## 5. EdgeConnection abstraction

- `trait EdgeConnection: Send` (`src/connector.rs:146-153`) with
  `run(self: Box<Self>, params: EdgeRunParams) ->
  Pin<Box<dyn Future<Output = ServeAttempt> + Send + 'static>>` — fully
  dyn-compatible. Implemented by `QuicConnection` (`connector.rs:359-360`)
  and `H2EdgeConnection` (`connector.rs:411-412`); `build_connection`
  returns `Box<dyn EdgeConnection>` (`connector.rs:334-357`) and
  `EdgeConnector::run` drives it, handling `registered_at` backoff reset,
  `quic_timed_out` fallback, EDUPCONN edge rotation, permanent-error abort,
  and shutdown.
- The QUIC implementation is the **quiche** one: `src/quic/mod.rs`
  (`quiche::connect`, `quiche::Connection`, driver task over a
  `UdpSocket`). No `quinn` anywhere in `Cargo.toml`, `Cargo.lock`, or
  `src/` (grep). AGENTS.md's quiche preference holds.
- Crate-internal is acceptable: `EdgeConnection` is a private impl detail
  behind the public `EdgeConnector`. Exposing it would drag
  `EdgeRunParams`/`ServeAttempt`/`Event` into the public API for no
  consumer benefit; AGENTS.md requires the abstraction to exist for the
  two transports, which it does. Design observation, not a defect.

## 6. Phase B review fixes (review-b.md minors)

| review-b finding | Status | Evidence |
|---|---|---|
| backoff base 10s vs 1s; no threshold match | **Fixed** | `EdgeOptions::default().backoff = 1s`, `max_quic_failures = 5` (`connector.rs:75-77`); tests `default_backoff_matches_cloudflared`, `auto_falls_back_after_max_failures` |
| no grace-period backoff reset | **Fixed** | `attempt = 0` when a connection survived ≥ `grace_period` (`connector.rs:281-284`) |
| discovery failure aborts run | **Fixed** | `connector.rs:210-224`: warn + exponential retry, never aborts |
| graceful shutdown closes immediately | **Fixed** | QUIC: `sleep(grace_period)` after unregister before `conn.close()` (`connector.rs:437`); H2: serve waits out in-flight streams + control task bounded by grace (`h2/mod.rs:196-207`, `connector.rs:443-467`) |
| NamedTunnel.endpoint unused | **Fixed** | `region_override()` (`tunnel.rs:96-100`) feeds region selection (`connector.rs:205-209`) |
| pump rewrite | **Fixed** | `origin/mod.rs:350-405` half-close pump (see §3) |
| QUIC abandoned-upload drain without reset | **Fixed** | `drain_unread` now issues `stream_shutdown(Read)` on give-up (`serve.rs:305-345`) |
| H2 control-task leak on connection close | **Fixed** | `control_shutdown` notified on close/error paths (`h2/mod.rs:137-158`) so the control task unregisters instead of blocking forever |
| config-update `"err":""` vs `null` | **Fixed** | reply emits `"err":null` (`h2/mod.rs:372`); H2 loopback test asserts the literal string |
| registration timeout 15s vs 5s | **Fixed** | `RPC_TIMEOUT = 5s` (`control.rs:21`), applied on both transports |
| per-registration random client_id | **Fixed** | persistent `OnceLock` connector id (`control.rs:68-74`) |

The one review-b item still open is not in scope for Phase C: edge-initiated
session/config-manager RPCs are answered `unimplemented`
(`serve.rs:255-268`; HA/remote-config are non-goals).

## 7. QUIC version negotiation

- **cloudflared offers v1 + v2.** The checkout pins quic-go v0.52.0
  (`go.mod`); `vendor/.../internal/protocol/version.go:31`:
  `SupportedVersions = []Version{Version1(0x1), Version2(0x6b3343cf)}`;
  the tunnel's `quic.Config` (`supervisor/tunnel.go:584-592`) does not set
  `Versions`, so `config.go:63-65` falls back to `SupportedVersions` — the
  client offers both. The Initial goes out with v1 (first in the list).
- **libcfd offers v1 only.** quiche 0.29.3: `PROTOCOL_VERSION =
  PROTOCOL_VERSION_V1` and `version_is_supported` matches only
  `PROTOCOL_VERSION_V1`. quiche does handle VersionNegotiation packets as a
  client, but with nothing else supported it would fail with "no supported
  versions" if the edge ever answered VN with a v2-only list.
- Risk assessment: **low for live-edge parity today.** The Cloudflare edge
  must keep serving v1 — cloudflared itself sends v1 Initials and the edge
  fleet has supported v1 since 2021. VN-vs-v1 on the edge is a protocol
  downgrade attack surface, so the edge will not drop v1. Document as a
  residual risk (v2 support arrives only if/when quiche implements RFC
  9369).

## 8. Public API quality

- Runtime-agnostic: no tokio types in any public signature. Origin traits,
  `Duplex`, `Body` use `futures_io`; `Request`/`Response` use `http`
  types; `EdgeConnector::run` takes `impl Future<Output = ()> + Send`;
  `EdgeOptions`/`RunOptions`/`QuickTunnelOptions` are plain data.
  `AxumOrigin::new(axum::Router)` — axum, not tokio. `lib.rs:11-31` docs
  now state honestly that execution requires a Tokio runtime (Phase A/B
  doc caveat resolved).
- Every public future is `Send`: origin traits return `impl Future +
  Send` or boxed `Send` futures; `run_quick_tunnel`/`EdgeConnector::run`
  compose `Send` futures.
- `#![warn(missing_docs)]` (`lib.rs:1`); `cargo doc --workspace
  --all-features --no-deps` with `RUSTDOCFLAGS="-D warnings"` is clean for
  both crates. Nit: libcfd-rpc does not enable the lint itself, so its
  public docs are not enforced.
- No credential logging: all `tracing` calls log errors, addresses,
  attempts, durations — never secrets/tokens/authorization data (grep of
  every `tracing::` call in `src/`).
- Naming: letter-per-word rule mostly held; `conn`, `opts`, `reg_opts`
  truncations persist (review-b carryover nit).
- Minimal deps/features: axum/tower optional; `uuid` `default-features =
  false` (v4 dropped, review-a nit fixed); tokio `signal` moved to
  dev-dependencies (review-b nit fixed); `rt-multi-thread` in main deps is
  only needed by examples/tests (nit); axum's `query` feature is enabled
  but unused (no `Query` extractor anywhere) — nit.
- No `capnp` in the root crate: `src/` and `examples/` reference capnp
  only in a comment (`loopback_test.rs:42`); all RPC goes through
  `libcfd_rpc` (AGENTS.md constraint holds). `unsafe`: none anywhere.

## 9. Validation (all inside `nix develop`)

- `cargo fmt --all --check` — **passed**.
- `cargo check --workspace --all-targets --all-features` — **passed**
  (re-verified on a fresh `CARGO_TARGET_DIR=/tmp/cfd-review-full`, exit 0).
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  — **passed**.
- `cargo test --workspace --all-features` — **57/57 passed**: libcfd 43
  (incl. the 4 loopback e2e), libcfd-rpc 2, rpc_exchange 4, wire 8,
  doc-tests 0.
- Feature combos: (a)-(e) above all pass (fresh target dirs for b/c/d/e).
- Single-feature extras: `h2-edge` alone **fails** (E0432, see §1);
  `quic-edge` alone passes check but has 43 dead-code warnings; all
  other single/two/three-feature combos pass.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features
  --no-deps` — **passed** for both crates.
- `cargo build --workspace --all-features --examples` — **passed**.

Loopback coverage (what each test exercises):
- `quic_tunnel_end_to_end` — real QUIC handshake over loopback UDP
  (client quiche driver vs mock-edge quiche server), registration RPC on
  stream 0 with literal capnp-go golden replies, HTTP request dispatch to
  a consumer `HttpOrigin`, response metadata + body back to the edge.
- `quic_websocket_tcp_round_trip` — websocket stream (101 status +
  `Sec-WebSocket-Accept` over QUIC metadata) and TCP stream (bare ack,
  no HttpStatus) with raw byte echo through consumer-dialed loopback
  sockets; EOF handling.
- `h2_tunnel_end_to_end` — real TLS (rustls server) + genuine h2 crate
  *client* role: control-stream RPC registration over the stream body
  with golden replies, data request round trip, `cf-cloudflared-response-meta`
  assertion, config-update stream with `{"lastAppliedVersion":7,"err":null}`
  assertion.
- `h2_websocket_tcp_round_trip` — H2 websocket (101→200 remap,
  base64-serialized user headers incl. `Sec-WebSocket-Accept`) and H2 TCP
  (bare 101→200 ack) with echo payloads.

Independence limits (unchanged from Phase B): the QUIC tests share the
quiche driver and the RPC codec with the client (codec correctness is
anchored by the capnp-go goldens, `rpc/tests/wire.rs`); the H2 tests use
an independent h2 client. Real-edge interop remains unverified.

## 10. Git state

Working tree is clean (nothing staged, nothing modified). The 6 Phase C
commits are coherent and each scoped to its message (gates → errors →
origin/Axum → EdgeConnection → docs/polish). `docs/research/` is fully
committed (the 4 briefs + review-a2 + review-b); `.pi-subagents/` and
`target/` are git-ignored; no stray untracked files. One dead tracked
file: `src/shutdown.rs` is not declared in `lib.rs` and referenced
nowhere (it duplicates `event.rs`'s `Event`) — should be deleted.

## Findings by severity

- **Blocker:** none.
- **Major:**
  1. `src/lib.rs:63` + `src/h2/mod.rs:23,28` — `--no-default-features
     --features h2-edge` fails to compile (unresolved `crate::control` /
     `crate::tunnel`); the transport-only combo is a cfg-gating hole. One
     line fixes it (gate `mod h2` on `h2-edge && any(tunnel feature)`).
- **Minor:**
  1. `src/lib.rs:72` — `quic-edge` alone compiles but yields 43
     dead-code warnings (serve/event/origin/quic unused without a tunnel
     feature); clippy `-D warnings` fails for that combo. Same fix family.
  2. `src/shutdown.rs` — tracked, dead, duplicate of `event.rs`; delete.
  3. `src/error.rs:48` — `Error::Shutdown` never constructed by library
     code; dead public variant.
  4. `examples/named_tunnel.rs:7` — doc claims "auto-selects QUIC and
     falls back to HTTP/2", but `required-features =
     ["named-tunnel","quic-edge"]` (`Cargo.toml:62`) makes the default
     transport `Quic` with no fallback; either add `h2-edge` to the
     example's required-features or fix the doc.
  5. `Cargo.toml:23` — axum `query` feature enabled but unused; tokio
     `rt-multi-thread` needed only by examples/tests (could be dev-only).
  6. libcfd-rpc lacks `#![warn(missing_docs)]`; its public docs are not
     lint-enforced.
- **Nit:**
  1. QUIC version negotiation: libcfd v1-only vs cloudflared v1+v2
     (quiche `version_is_supported` = `{V1}`); low live-edge risk, but
     document it (§7).
  2. `src/tunnel.rs:190` — credentials-file read failure surfaces as
     `Error::Io`, not `NamedTunnelCredentials`; typed but slightly
     inconsistent.
  3. `parse_tunnel_id` maps bad IDs to `NamedTunnelCredentials` even for
     quick tunnels (variant naming).
  4. `examples/origin_ws_tcp.rs:63-73` duplicates `websocket_accept`
     because `origin/mod.rs` keeps it `pub(crate)`.
  5. `conn`/`opts`/`reg_opts` naming vs the letter-per-word rule
     (carryover).
  6. Edge-discovery fallback remains hardcoded hostnames vs cloudflared's
     DoT (documented in `edge.rs`; only matters when DNS fails).
  7. `default_config_json` sends `"warp-routing":{}` vs cloudflared's
     explicit zero-value fields (functionally equivalent; carryover).

## Verdict against AGENTS.md

Phase A goals — all met and preserved: create quick tunnel via HTTP API
(`api.rs`, `tunnel.rs`), discover + connect over QUIC (quiche,
`edge.rs`+`quic/`), register + maintain (`control.rs`, reconnect/backoff
in `EdgeConnector`, EDUPCONN handling), deliver to a consumer HTTP origin,
return responses (`serve.rs`). `run_quick_tunnel` still works on top of
`EdgeConnector` with `Transport::Quic`.

Phase B goals — all met: shared `Tunnel` abstraction + Serde
(`QuickTunnel`/`NamedTunnel`, byte-compatible credentials JSON),
consumer-provided WebSocket/TCP origins with a runtime-agnostic `Duplex`,
HTTP/2 edge connection (server role, SNI `h2.cftunnel.com`, no ALPN,
byte-exact classify/header rules, RPC on the control-stream body), and
`EdgeConnector` with discovery/retries/transport selection.

Phase C goals — met with two feature-matrix gaps:
- Typed thiserror errors at all stable boundaries, no anyhow: met.
- `quick-tunnel` gated; `named-tunnel` gated: met.
- Shared origin abstraction (`HttpOrigin`/`WebSocketOrigin`/`TcpOrigin`,
  `AxumOrigin` HTTP bridge): met; websocket-infeasibility doc verified
  accurate against axum 0.8.9.
- `EdgeConnection` abstraction over `H2EdgeConnection`/`QuicEdgeConnection`,
  quiche retained: met.
- `h2-edge`/`quic-edge` gates: met for every combination that includes a
  tunnel feature; **`h2-edge` alone fails to compile and `quic-edge`
  alone is clippy-noisy** (major/minor above) — the only items that must
  change before the project is done.

## Must change before done

1. Gate `mod h2` (and `mod serve`) on the presence of a tunnel feature
   (`src/lib.rs:63,72`) so the declared transport features never break the
   build; add the missing `allow(dead_code)` for the transport-only case.
2. Delete the dead `src/shutdown.rs`.
3. Either add `h2-edge` to `named_tunnel`'s required-features or correct
   its doc claim.
4. Decide whether `Error::Shutdown` and the unused axum `query` feature
   stay (remove or wire up).
5. Optional: real-edge smoke test to close the loopback-only interop
   gap; document the QUIC v1-only stance in the crate docs.
