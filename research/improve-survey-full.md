# libcfd Code-Quality Improvement Campaign — Survey Report

Repo: `/home/aperso/libcfd` (workspace: `libcfd` + `rpc/` = `libcfd-rpc`). Project complete (Phases A–C, live-edge tested). All `cargo check` feature-matrix builds pass, `cargo test --workspace --all-features` passes (57 passed, 2 ignored live tests).

---

## 1. MODULE TREE + FILE SIZES

### src/lib.rs cfg gates (src/lib.rs:43-126)

| Module | Gate in lib.rs | Notes |
|---|---|---|
| `api` | `quick-tunnel` (lib.rs:44) | Hand-rolled HTTPS POST client |
| `connector` | `all(any(qt,nt), any(quic,h2))` (lib.rs:46-52) | Compound gate repeated verbatim 5× |
| `control` | same compound (lib.rs:54-59) | |
| `edge` | same compound (lib.rs:61-65) | |
| `error` | always (lib.rs:66) | **Holds unconditional `From<quiche::Error>` / `From<h2::Error>`** |
| `event` | same compound (lib.rs:68-74) | |
| `h2` | `all(h2-edge, any(qt,nt))` (lib.rs:76-80) | |
| `origin` | always (lib.rs:81); `origin::axum` under `axum-origin` | |
| `quic` | `all(quic-edge, any(qt,nt))` (lib.rs:82-87) | |
| `roots` | same compound (lib.rs:88-94) | |
| `run` | `all(quick-tunnel, quic-edge)` (lib.rs:95-97) | |
| `serve` | `all(quic-edge, any(qt,nt))` (lib.rs:98-103) | |
| `tunnel` | `any(qt,nt)` (lib.rs:104-105) | |
| `loopback_test` | `all(quick-tunnel, any(quic,h2))` (lib.rs:126) | `#![cfg(test)]` mock-edge e2e tests |

**Convoluted gating**: the `all(any(quick-tunnel,named-tunnel), any(quic-edge,h2-edge))` predicate appears 5 times (connector, control, edge, event, roots) plus `lib.rs:46-105` re-export block. An internal `tunnel-runtime = [..]` style meta-feature (or the same gate computed once) would remove the repetition. All feature combinations verified to compile (`cargo check` × 6 matrix runs below).

### File inventory (line counts)

| File | Lines | Contents |
|---|---|---|
| `src/lib.rs` | 126 | Crate root: docs, module gates, re-exports |
| `src/loopback_test.rs` | **924** | Mock quiche edge + 4 e2e loopback tests (**largest file; reorg candidate**) |
| `src/connector.rs` | **630** | `EdgeConnector`, `Transport`, `EdgeOptions`, `retry_delay`, `run_quic`/`run_h2`, `EdgeConnection` trait + 9 tests (**reorg candidate**) |
| `src/h2/mod.rs` | **521** | `H2EdgeConnection`, serve loop, `classify`, per-stream handlers, TLS config + 6 tests (**reorg candidate**) |
| `src/origin/mod.rs` | **476** | `Origin`, Http/WebSocket/Tcp traits, `Request`/`Response`/`Body`/`Duplex`, `pump`, `websocket_accept` + 1 test (**reorg candidate**) |
| `src/tunnel.rs` | 412 | `Tunnel` enum, `QuickTunnel`, `NamedTunnel`, quick-tunnel API client, `secret_codec` + 7 tests (borderline) |
| `src/serve.rs` | 396 | QUIC serve loop, per-stream handlers, `handle_rpc_stream`, `drain_unread` + 3 tests (borderline) |
| `src/edge.rs` | 350 | Hand-rolled DNS SRV resolver + 2 tests (borderline) |
| `src/quic/mod.rs` | 311 | `QuicConnection`, `Inner`, `drive` loop |
| `src/origin/axum.rs` | 229 | `AxumOrigin` adapter + 2 tests |
| `src/h2/stream.rs` | 199 | `RecvStreamReader`, `SendStreamWriter`, `H2Bidi` |
| `src/h2/headers.rs` | 183 | Cloudflared header serialization rules + 4 tests |
| `src/quic/stream.rs` | 173 | `QuicStream` AsyncRead/AsyncWrite |
| `src/api.rs` | 162 | Hand-rolled HTTP/1.1+TLS POST client + 2 tests |
| `src/control.rs` | 162 | `register`, `register_on_stream`, `unregister`, `RegistrationOptions` |
| `src/error.rs` | 150 | Typed `Error` enum + `From` impls + 4 tests |
| `src/run.rs` | 79 | `RunOptions`, `run_quick_tunnel` |
| `src/event.rs` | 52 | One-shot `Event` (fired flag + Notify) |
| `src/quic/tls.rs` | 45 | quiche Config from boring + 1 test |
| `src/roots.rs` | 33 | System + bundled Cloudflare CA assembly |
| `rpc/src/lib.rs` | 53 | Crate root + generated capnp module decls |
| `rpc/src/error.rs` | 49 | `RpcError` |
| `rpc/src/io.rs` | 113 | Message framing, `read_message`/`write_raw` |
| `rpc/src/quic.rs` | 254 | ConnectRequest/Response codec + 2 tests |
| `rpc/src/rpc.rs` | 344 | Minimal capnp RPC client (`RpcClient`, `read_incoming`) |
| `rpc/src/tunnel.rs` | 288 | `TunnelClient` typed facade, `RegistrationFailure` |
| `tests/live_edge.rs` | 304 | 2 ignored live-edge tests + HTTPS helper |
| `rpc/tests/wire.rs` | ~660 | Golden-byte verification vs capnp-go + 8 tests |
| `rpc/tests/rpc_exchange.rs` | ~470 | Mock-edge RPC client exchange + 4 tests |
| `rpc/build.rs` | 17 | capnpc codegen for 3 schemas |

**>400-line files to split (vector c):** `loopback_test.rs` (924), `connector.rs` (630), `h2/mod.rs` (521), `origin/mod.rs` (476), `tunnel.rs` (412).

---

## 2. DEPENDENCIES

### libcfd (Cargo.toml:17-46)

| Dep | Optional? | Feature that needs it | Actual users | Verdict |
|---|---|---|---|---|
| `quiche` 0.29.3 | **NO** | `quic-edge` | `src/quic/{mod,stream,tls}.rs`, `src/serve.rs:250,345`, `src/error.rs:63` | **BLOCKER: make `optional = true`, tie to `quic-edge` (vector a)** |
| `boring` 4.22 | **NO** | `quic-edge` (quiche backend) + **any transport** via RNG | `src/quic/tls.rs:8-9`, `src/quic/mod.rs:75`, `src/connector.rs:349` (`retry_delay`), `src/edge.rs:286` (`rand16`), `src/loopback_test.rs` (test) | Gate on `quic-edge` **only after** replacing `boring::rand::rand_bytes` in connector/edge with a lighter RNG (e.g. `rand` or hash-based). Heavy BoringSSL build otherwise paid in h2-only builds |
| `h2` 0.4.15 | **NO** | `h2-edge` | `src/h2/*`, `src/error.rs:69`, `src/loopback_test.rs` (test) | Make `optional`, tie to `h2-edge` |
| `bytes` | **NO** | `h2-edge` + `axum-origin` | `src/h2/mod.rs:17`, `src/h2/stream.rs:15`, `src/origin/axum.rs:17`, tests | `optional`, gate `any(h2-edge, axum-origin)` |
| `rustls` 0.23.43 | **NO** | `h2-edge` + `quick-tunnel` (api.rs) | `src/h2/mod.rs:20,454`, `src/api.rs`, loopback/live tests | `optional`, gate `any(h2-edge, quick-tunnel)` |
| `tokio-rustls` | **NO** | same as rustls | `src/h2/mod.rs:37,70`, `src/api.rs:29`, tests | `optional`, same gate |
| `rustls-pki-types` | **NO** | same as rustls | `src/h2/mod.rs:20,457`, `src/api.rs:10`, tests | `optional`, same gate |
| `webpki-roots` | **NO** | `quick-tunnel` only | `src/api.rs:93`, `tests/live_edge.rs:84` (dev) | `optional`, gate `quick-tunnel` |
| `serde` | **NO** | `any(qt,nt,h2-edge)` | `src/tunnel.rs:8`, `src/h2/mod.rs` (config JSON) | `optional`, gate `any(quick-tunnel, named-tunnel, h2-edge)` |
| `serde_json` | **NO** | same | `src/tunnel.rs:193,294`, `src/h2/mod.rs:425` | `optional`, same gate |
| `uuid` (no default features) | **NO** | `any(qt,nt)` | `src/tunnel.rs:210` | `optional`, gate `any(quick-tunnel, named-tunnel)` |
| `base64` | **NO** | always | `src/tunnel.rs:218` (codec), `src/h2/headers.rs:9`, `src/origin/mod.rs:351` | Keep unconditional (public `websocket_accept` + tunnel serde) |
| `sha1` | **NO** | always | `src/origin/mod.rs:351` (public `websocket_accept`) | Keep unconditional |
| `http` | **NO** | always | Public origin API, api.rs, tunnel.rs, h2, serve | Keep |
| `futures-io` | **NO** | always | `src/origin/mod.rs` (public `Duplex`), `rpc/src/io.rs` | Keep |
| `futures-util` (`io` feat) | **NO** | always | `src/origin/mod.rs` (`pump`), quic/h2 streams, serve, axum | Keep; verify only `io` feature needed (yes) |
| `tokio` | **NO** | always | see below | Feature set minimal? — see below |
| `tracing` / `thiserror` | **NO** | always | everywhere | Keep |
| `axum` | **YES** | `axum-origin` (Cargo.toml:23) | `src/origin/axum.rs` | Correct as-is |
| `tower` | **YES** | `axum-origin` (Cargo.toml:41) | `src/origin/axum.rs:53` | Correct as-is |
| `libcfd-rpc` | NO (path) | always | control.rs, serve.rs, error.rs | Keep |

**reqwest/hyper**: `reqwest` is **not** a dependency (0 matches in Cargo.lock). `hyper` appears only once (transitive via axum). The quick-tunnel API client is hand-rolled in `src/api.rs` (tokio-rustls + rustls + webpki-roots + http); nothing to tie `reqwest` to.

**tokio features** (`Cargo.toml:35`): `default-features = false` with `["net","rt","time","io-util","sync","macros","fs"]`. All verified used: `net` (TcpStream/UdpSocket/lookup_host), `rt` (`task::spawn`), `time` (sleep/timeout), `io-util` (copy/read_to_end/AsyncReadExt), `sync` (Notify/watch/mpsc/oneshot), `macros` (select!/pin!), `fs` (edge.rs `tokio::fs::read_to_string`). Minimal. **Note:** `macros` is exercised only via `tokio::select!`/`tokio::pin!` — keep.

**Dev-deps** (`Cargo.toml:44-47`): `rcgen` (loopback certs), `tokio` extra `rt-multi-thread`+`signal` (tokio::test), `tokio-util` `compat` (loopback + `Duplex::new` compat), `tracing-subscriber` (live test). All used. `tokio` "signal" — grep: not used by src; only dev feature set. Could trim `signal` (check: nothing uses `signal` — `shutdown` in examples is ctrl_c? examples use `tokio::signal::ctrl_c()` — need to verify; examples are built with dev-deps? Examples use dev-dependencies, yes). So `signal` is used by examples. Keep.

**rpc crate** (`rpc/Cargo.toml`): `capnp` + `futures-io` + `thiserror` + `tracing` (all used); build-deps `capnpc`, `capnpc-embedded` (used in build.rs); dev-deps `futures` (block_on in tests), `tokio` (duplex streams). All used.

**cargo-machete** (`nix run nixpkgs#cargo-machete`, run at workspace root): "didn't find any unused dependencies". Caveat: machete only detects deps never referenced in source; it does **not** detect deps compiled unconditionally but only needed under a feature. The real findings are the feature-gating opportunities above (verified via `cargo tree --no-default-features`, which still shows quiche, h2, boring, rustls, tokio-rustls, webpki-roots, bytes, serde, serde_json, uuid all compiled with **zero** features enabled).

---

## 3. FEATURES

`[features]` (Cargo.toml:8-15): `default = ["quick-tunnel","named-tunnel","quic-edge","h2-edge"]`; `quick-tunnel = []`, `named-tunnel = []`, `quic-edge = []`, `h2-edge = []`, `axum-origin = ["dep:axum","dep:tower"]`.

- Module gates all verified correct via feature-matrix `cargo check` (all pass): `--no-default-features`, `+quick-tunnel`, `+quick-tunnel,h2-edge`, `+quick-tunnel,quic-edge`, `+named-tunnel,h2-edge`, `+named-tunnel,quic-edge`, `+quick-tunnel,axum-origin`.
- **Module compiling under a feature that doesn't need it**: `src/error.rs` compiles `impl From<quiche::Error>` (error.rs:63-66) and `impl From<h2::Error>` (error.rs:69-72) **unconditionally**. This is the structural reason quiche/h2 can't be made optional today — the `From` impls must be `#[cfg(feature = "quic-edge")]` / `#[cfg(feature = "h2-edge")]` first.
- `Transport` enum + `select_transport` correctly cfg-gate variants (connector.rs:28-38, 232-272).
- `error.rs:87-99` `is_permanent` correctly gated on the compound condition.
- Feature names consistent; doc comments in lib.rs:23-40 describe them correctly.

---

## 4. TESTS INVENTORY (61 `#[test]`/`#[tokio::test]` attributes)

Totals per target (all-features run): libcfd lib unit 43 (45 attributes, 2 cfg'd out under all-features) + libcfd-rpc lib unit 2 + integration `live_edge` 2 (both `#[ignore]`) + `rpc/tests/rpc_exchange` 4 + `rpc/tests/wire` 8 = **59 tests, 57 pass, 2 ignored**.

### libcfd unit tests

| File | Test | Covers | Verdict |
|---|---|---|---|
| connector.rs | `quic_only_transport_never_falls_back` | select_transport | Keep (cheap) |
| connector.rs | `auto_falls_back_after_max_failures` | Auto fallback thresholds | Keep |
| connector.rs | `h2_stays_h2` | select_transport | Keep (cheap) |
| connector.rs | `default_transport_is_auto_when_both_edges_enabled` | Default policy | **Redundant-ish** — duplicates `default_backoff_matches_cloudflared` intent; keep or merge |
| connector.rs | `default_transport_is_quic_without_h2` | cfg branch | Keep (only cfg-specific coverage) |
| connector.rs | `default_transport_is_h2_without_quic` | cfg branch | Keep |
| connector.rs | `backoff_is_bounded_by_base_times_two_pow` | retry_delay bound | Keep |
| connector.rs | `zero_base_backoff_is_instant` | retry_delay edge | **Tautological-ish but 1-line; keep** |
| connector.rs | `default_backoff_matches_cloudflared` | Defaults | Keep |
| tunnel.rs | `quick_tunnel_url_prepends_scheme` / `keeps_scheme` | `url()` | Keep |
| tunnel.rs | `parses_api_response` / `parses_api_error` | Serde of API JSON | Keep (wire contract) |
| tunnel.rs | `named_tunnel_credentials_round_trip` | Credentials serde | Keep |
| tunnel.rs | `tunnel_enum_round_trip` | Tunnel serde | Keep |
| tunnel.rs | `tunnel_id_bytes_parse` | UUID parse | **Redundant with `tunnel_enum_round_trip`+parse_tunnel_id; keep (cheap) or drop** |
| h2/mod.rs | `classifies_{http,control_stream,websocket,tcp,configuration}` ×5 | StreamType classification | Keep (protocol contract) |
| h2/mod.rs | `remaps_switching_protocols` | 101→200 | Keep |
| h2/headers.rs | `serializes_and_deserializes_headers`, `empty_serialization_is_empty` | b64 header round-trip | Keep |
| h2/headers.rs | `response_headers_apply_cloudflared_rules` | Header rules | Keep |
| h2/headers.rs | `computes_websocket_accept` | RFC6455 | **Redundant** — identical to `origin/mod.rs::computes_websocket_accept` (same vector/key/expectation); drop one |
| error.rs | `every_public_error_path_is_typed_and_displayable` | Display of all variants | Keep |
| error.rs | `registration_failure_classifies_permanent` | is_permanent | Keep |
| error.rs | `io_error_converts_from_std` | From impl | **Redundant-ish** (derive `#[from]`); keep or drop |
| error.rs | `rpc_error_converts_into_control_variant` | From impl | **Redundant-ish**; same |
| serve.rs | `classifies_http_request`, `classifies_tcp_request`, `encodes_response_metadata` | QUIC metadata codec | Keep (protocol contract) |
| edge.rs | `encodes_srv_query_name`, `parses_srv_response` | DNS wire format | Keep |
| api.rs | `parses_http_response_with_content_length` / `without_content_length` | HTTP/1.1 parse | Keep |
| origin/mod.rs | `computes_websocket_accept` | RFC6455 | Keep (canonical one) |
| origin/axum.rs | `axum_router_serves_through_the_adapter`, `unknown_route_returns_404` | Adapter behavior | Keep |
| quic/tls.rs | `bundled_cloudflare_roots_parse` | bundled pem | Keep |
| loopback_test.rs | `quic_tunnel_end_to_end` | Full QUIC register+serve via mock edge | Keep (core e2e) |
| loopback_test.rs | `quic_websocket_tcp_round_trip` | QUIC ws/tcp | Keep |
| loopback_test.rs | `h2_tunnel_end_to_end` | Full H2 register+serve+config | Keep |
| loopback_test.rs | `h2_websocket_tcp_round_trip` | H2 ws/tcp | Keep |

### Integration tests

| Target | Test | Verdict |
|---|---|---|
| `tests/live_edge.rs` | `create_and_save_quick_tunnel` (#[ignore]) | Keep, on-demand |
| `tests/live_edge.rs` | `live_quick_tunnel_over_quic_serves_http` (#[ignore]) | Keep |
| `rpc/tests/wire.rs` | `golden_messages_match_go_reference`, `golden_messages_parse`, `golden_register_return_parses`, `golden_release_parses`, `golden_connect_messages_parse`, `framing_round_trip`, `connect_request_round_trip`, `read_message_rejects_oversized_segment` | Golden-byte + parse coverage | `golden_messages_match_go_reference` is the crown jewel; `golden_*_parses` partially overlap with rpc/src/quic.rs round-trips but test capnp decode — keep; `connect_request_round_trip` **duplicates** `rpc/src/quic.rs::connect_request_round_trip` — drop one |
| `rpc/tests/rpc_exchange.rs` | `tunnel_client_registers_with_mock_edge`, `tunnel_client_register_error_returns_connection_error`, `tunnel_client_unregisters_with_mock_edge`, `tunnel_client_close_releases_bootstrap_capability` | Client behavior vs mock | Keep; note `serve_mock`/`serve_mock_err`/`serve_mock_with_release` + `StubHook` + `TokioBridge` are **duplicated verbatim** with wire.rs — extract shared test util |

**Candidates for removal (vector e):**
1. `h2/headers.rs::computes_websocket_accept` — exact duplicate of `origin/mod.rs::computes_websocket_accept`.
2. `rpc/tests/wire.rs::connect_request_round_trip` — duplicates `rpc/src/quic.rs` unit round-trip.
3. `tunnel.rs::tunnel_id_bytes_parse` — overlaps `tunnel_enum_round_trip` (low value, not wrong).
4. `error.rs::{io_error_converts_from_std, rpc_error_converts_into_control_variant}` — test the derive itself (low value).
5. Connector default-transport tests — 3 cfg branches; the "auto" one is the only behaviorally meaningful; consider keeping as-is since each guards a cfg branch.
6. **Flag**: `tests/live_edge.rs` has **no `[[test]] required-features`** — `cargo test --no-default-features` fails to compile it (E0432 unresolved imports, verified). Add `required-features = ["quick-tunnel","quic-edge"]` to a `[[test]]` entry for live_edge (Cargo.toml currently only declares `[[example]]` blocks, lines 50-72).

Also: `src/loopback_test.rs` is a 924-line test-only module inside `src/` because it needs `crate::`-private access (control/quic/serve/h2/event). Splitting it requires either per-transport `#[cfg(test)]` modules under `src/quic/` and `src/h2/`, or a `pub(crate)` test-util seam.

---

## 5. DEAD CODE

**Never-constructed items:**
- `rpc/src/error.rs:22` `RpcError::Unimplemented` — declared, never constructed (the wire-level unimplemented reply is built directly in `rpc.rs::build_exception`). **Remove.**
- `rpc/src/quic.rs` `FLOW_ID_KEY` (line ~50) and `TRACE_ID_KEY` (line ~52) — unused anywhere. **Remove.**
- `rpc/src/quic.rs` `HTTP_HEADER_KEY` (line ~46) — re-exported in rpc/src/lib.rs:46 but never used by libcfd (`serve.rs` hardcodes its own `HEADER_KEY_PREFIX = "HttpHeader:"`). **Remove or adopt.**
- `rpc/src/quic.rs:191` `encode_connect_request_bytes` — pub, zero callers (the mock edge uses `write_connect_request`). **Remove.**
- `rpc/src/io.rs:55` `serialize_message` — used internally + tests; keep.
- `Error::Control` (src/error.rs:43) — not constructed by name but reached via `#[from] libcfd_rpc::RpcError`; keep. `Error::Tls` — reached via `From<boring::error::ErrorStack>`; keep.

**Duplicated helpers:**
- `build_tcp_request` — `src/serve.rs:281` and `src/h2/mod.rs:444` (near-identical; signatures differ slightly — QUIC takes `&ConnectRequest`, H2 takes `&str` host). Consolidate into one shared fn.
- Local-IP octets extraction duplicated: `src/connector.rs:380` `peer_ip_bytes` vs `src/h2/mod.rs:64-67` inline.
- Test helpers duplicated between `rpc/tests/wire.rs` and `rpc/tests/rpc_exchange.rs`: `TokioBridge`, `StubHook`, `serve_mock` family, `build_*_return` builders, `hex()` (also in `src/loopback_test.rs`).
- `tests/live_edge.rs::https_get` re-implements the hand-rolled HTTP client pattern of `src/api.rs` (status/header parse). Live test needs chunked + GET so not a straight reuse; note as intentional.

**TODO/FIXME/XXX/HACK**: none found in `src`, `rpc/src`, `tests`, `examples` (grep clean).

**Leftover structure**: `event.rs` is the only event primitive (no shutdown.rs vestige). `roots.rs` is shared correctly by both transports.

---

## 6. GIT STATE

- HEAD: `491dd3939a8a6ddcc3a834ca5da89fe1b1343139` on `master`.
- Working tree **clean** (no staged/unstaged changes).
- Recent commits (newest first): `491dd39` live-edge testing with reusable quick tunnel credentials; `93de7d3` fix feature-matrix gating and final review findings; `7ebb06f` docs/example feature requirements; `96db7bd` shared EdgeConnection abstraction; `7a10bf4` shared origin abstraction + AxumOrigin; `f64d866` typed errors at public boundaries; `cf3d10e` feature gates for tunnels and transports; `2bb22ec` phase B review fixes; `4018fcc` remove unused error variant; `030fea4` drop unused futures-util compat feature (i.e. some dep hygiene already done).
- `.test-creds/quick-tunnel.json` present (gitignored, real creds — do not touch/log).
- Submodule `cloudflared/` (reference checkout, read-only per AGENTS.md).
- `flake.nix` devshell provides cargo/rustfmt/clippy + cargo-deny/edit/watch; `.envrc` direnv.

---

## 7. STYLE

- **Naming**: no `ctx`/`mgr`-style abbreviations (only `SslContextBuilder`/`with_boring_ssl_ctx_builder` external API names; `"ctx"` hit in loopback_test.rs:87 is a comment string). No violations of the all-or-nothing abbreviation rule.
- **anyhow**: zero usage anywhere (all typed `thiserror` at boundaries; internal `Result` alias in error.rs:73).
- **unwrap/expect in non-test code** (candidates for replacement, none blocking):
  - `src/quic/mod.rs` and `src/quic/stream.rs`: ~14× `inner.lock().unwrap()` on `Mutex<Inner>` (quic/mod.rs:107,140,171,180,196,220,233,251,296; quic/stream.rs:42,72,138). Poisoned-mutex panic is defensible, but a `fn lock(&self)` helper or `.expect("quic inner lock poisoned")` would document intent.
  - `src/h2/mod.rs`:119 `reg_tx.take().expect("control stream handled once")` — invariant, fine.
  - `src/h2/mod.rs`:125, 338-342, 387, 409, 418-419 — `http::Response::builder().body(())`/`HeaderValue::from_str` on known-good values; could use `HeaderValue::from_static` where the literal is static (418-419).
  - `src/h2/headers.rs`:85,90,101 — `HeaderValue::from_str(...).unwrap()` on origin-supplied header values; could fall back or use `from_static` for the constant literals.
  - All other unwraps/expects are inside `#[cfg(test)]` modules (verified line ranges) or test files.

---

## Improvement-vector summary (for campaign planning)

**Vector a — quiche behind quic-edge (primary):**
1. Gate `From<quiche::Error>` (error.rs:63) and `From<h2::Error>` (error.rs:69) on their features.
2. `cargo add quiche --optional` → `quic-edge = ["dep:quiche"]` (keep `default-features = false` not needed; quiche default feature is only `boringssl-boring-crate` which is required).
3. `cargo add h2 --optional` → `h2-edge = ["dep:h2"]`.
4. Make `boring`, `rustls`, `tokio-rustls`, `rustls-pki-types`, `bytes`, `webpki-roots`, `serde`, `serde_json`, `uuid` optional with the per-dep gates in §2.
5. Replace `boring::rand::rand_bytes` in `connector.rs:349` and `edge.rs:286` with a light RNG so `boring` can be quic-edge-only.
6. Re-verify the 6-combination feature matrix + `cargo test --workspace --all-features`.

**Vector b — code reorganization:** split `connector.rs` (EdgeConnector orchestration vs per-transport run + transport selection); split `origin/mod.rs` (body.rs / request-response / traits / pump); split `h2/mod.rs` (connection.rs / serve.rs / tls.rs); dedupe `build_tcp_request`, local-ip octets, test helpers.

**Vector c — directory structure:** split `loopback_test.rs` into `src/quic/` + `src/h2/` test submodules (or move to `tests/` with a `pub(crate)` test seam); split `tunnel.rs` into `tunnel/mod.rs` + `quick.rs` + `named.rs` + `secret_codec.rs`.

**Vector d — dependencies:** items in §2 (quiche, h2, boring, bytes, webpki-roots, rustls trio, serde trio).

**Vector e — tests:** remove the 4-6 redundant tests in §4; add `[[test]] required-features` for `live_edge`; extract shared rpc test util.

**Residual risks:** `boring` remains heavy while shared by edge.rs RNG; quiche `boringssl-boring-crate` feature forces `boring` anyway in quic builds; `serde` gating on `h2-edge` couples config-JSON handling to tunnel serde — verify no public API regressions when `named-tunnel`+`h2-edge` with no `quick-tunnel`; loopback split must preserve `crate::`-private access or use `pub(crate)`.
