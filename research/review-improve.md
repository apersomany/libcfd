# Independent final review — code-quality improvement campaign (9ea5256, c674689, 758627a)

Repo: `/home/aperso/libcfd` (workspace: `libcfd` + `rpc` = `libcfd-rpc`). Baseline before the campaign: `491dd39`. All evidence below was re-derived during this review, not taken from the survey or commit messages.

## 1. Feature guarding correctness

### Cargo tree proofs (all `-e normal`, top-level deps of libcfd)

| Feature combo | quiche | boring | h2 | axum | rustls trio |
|---|---|---|---|---|---|
| `--no-default-features --features named-tunnel,h2-edge` | absent | absent | present | absent | present |
| `--no-default-features --features h2-edge` | absent | absent | present | absent | present |
| `--no-default-features --features quick-tunnel,quic-edge` | present | present | absent | absent | present |
| `--no-default-features --features named-tunnel,quic-edge` | present | present | absent | absent | absent |
| default (4 features) | present | present | present | absent | present |
| default + `axum-origin` | present | present | present | present | present |

Verdict: quiche and boring are fully absent without `quic-edge` (h2-only builds no longer pay BoringSSL); h2 absent without `h2-edge`; axum absent without `axum-origin`. The `getrandom` swap (`src/edge.rs:283` `rand16`, `src/control.rs:69` `connector_client_id`, `src/connector/backoff.rs:28` `retry_delay`; `Cargo.lock` diff is +1 line: `getrandom 0.4.3`) removed the last non-quic-edge `boring` uses, so `boring` is now `quic-edge`-only (`Cargo.toml:12`).

### build.rs cfg emission (`build.rs:1-26`)

- `any_tunnel` = `quick-tunnel || named-tunnel`
- `any_edge` = `quic-edge || h2-edge`
- `edge_conn` = `any_tunnel && any_edge`
- `quic_any` = `quic-edge && any_tunnel`
- `h2_any` = `h2-edge && any_tunnel`
- All five names are declared via `cargo:rustc-check-cfg` (no unexpected-cfg warnings possible).

These match the pre-campaign predicates exactly (verified per module against `git show 491dd39:src/lib.rs`):

| Module | Old predicate | New cfg | Match |
|---|---|---|---|
| connector, control, edge, event, roots | `all(any(qt,nt), any(quic,h2))` | `edge_conn` | ✓ |
| h2 | `all(h2-edge, any(qt,nt))` | `h2_any` | ✓ |
| quic, serve | `all(quic-edge, any(qt,nt))` | `quic_any` | ✓ |
| tunnel | `any(qt,nt)` | `any_tunnel` | ✓ |
| loopback | `all(qt, any(quic,h2))` (+ in-file `#![cfg(test)]`) | `all(qt, any_edge)` + `#[cfg(test)]` | ✓ |
| run | `all(qt, quic-edge)` | unchanged explicit | ✓ |
| error `is_permanent` / `region_override` dead-code attrs | `any(quic,h2)` | `any_edge` | ✓ |

Empirical proof: all **16** feature combos compile (`cargo check --workspace --all-targets`), and the 11-combo clippy matrix (see §7) plus the review-c "closure-era" combos `(a) all-features, (b) quick-tunnel+quic-edge, (c) named-tunnel+h2-edge, (d) no-default, (e) axum-origin` all pass with zero warnings. The two gaps review-c found (h2-edge alone E0432; quic-edge alone dead-code noise) are closed by the `*_any` gating.

## 2. Public API stability

Crate-root re-exports of `libcfd` (`src/lib.rs:77-99`) are character-identical between `491dd39` and HEAD (only the `loopback` module declaration moved/gained `#[cfg(test)]`). Crate-root re-exports of `libcfd-rpc` (`rpc/src/lib.rs:44-52`) are identical. Item-level comparison of every `pub` item in connector, tunnel, origin, h2, error, event, run, quic, edge, control, api, serve, axum before/after: identical.

Only public changes found are the four **dead-code removals in libcfd-rpc** (declared campaign goal, not part of the reorganization):
- `RpcError::Unimplemented` (was `rpc/src/error.rs:22`) — removed variant of the crate-root-re-exported `RpcError`; never constructed; the wire-level reply is preserved via the capnp schema type `rpc_capnp::exception::Type::Unimplemented` (`rpc/src/rpc.rs:285`).
- `quic::{FLOW_ID_KEY, TRACE_ID_KEY}` (were `rpc/src/quic.rs:30,32`) — never used, not re-exported from the rpc crate root.
- `quic::encode_connect_request_bytes` (was `rpc/src/quic.rs:138`) — zero callers; the typed codec `encode_connect_request`/`decode_connect_request_bytes` remain public.

`Request::tcp` and `control::peer_ip_bytes` (dedupe replacements) are `pub(crate)` — internal. The getrandom swap and dep optionalization are internal (same public types, same signatures).

## 3. Reorganization quality

- Largest file now `src/serve.rs` at **386 lines**; nothing exceeds ~400 (was: loopback_test.rs 924, connector.rs 630, h2/mod.rs 521, origin/mod.rs 476, tunnel.rs 412).
- New layout is cohesive: `connector/{mod,options,runtime,backoff}` (orchestration / policy / connection abstraction / backoff), `tunnel/{mod,quick,named,secret}`, `h2/{mod,streams,register,tls,stream,headers}`, `origin/{mod,body,duplex,traits,pump,axum}`.
- Test reorg: `src/loopback/{mod,mock_edge,quic_tests,h2_common,h2_tests,h2_ws_tests}` (crate-private access preserved); `rpc/tests/common/mod.rs` shared harness (`TokioBridge`, `StubHook`, `serve_mock` family) used by both `wire.rs` and `rpc_exchange.rs`.
- Helper dedup confirmed by grep: `build_tcp_request` → gone, replaced by `Request::tcp` (`src/origin/body.rs:35`, used by `src/serve.rs:196,373` and `src/h2/streams.rs:93`); local-IP octets → single `control::peer_ip_bytes` (`src/control.rs:79`, used by `src/h2/mod.rs:70` and `src/connector/runtime.rs:98`); no duplicated `TokioBridge`/`StubHook`/`serve_mock`. The two `hex` fns (`src/loopback/mod.rs:26` hex→bytes vs `rpc/tests/wire.rs:9` bytes→hex) have opposite signatures in different targets — not a duplication.

## 4. Test pruning soundness

Removed tests (4) and why each was genuinely redundant:
- `rpc/tests/wire.rs::connect_request_round_trip` — exact duplicate of `rpc/src/quic.rs:212` `connect_request_round_trip` (same dest/type/metadata round-trip through the same codec; golden byte coverage remains in the 7 surviving wire tests incl. `golden_messages_match_go_reference`).
- `src/error.rs::{io_error_converts_from_std, rpc_error_converts_into_control_variant}` — these only exercised `#[from]` derive impls; the surviving `every_public_error_path_is_typed_and_displayable` still constructs `Error::Io` and `Error::Control` and asserts Display, so variant wiring is still observed.
- `src/h2/headers.rs::computes_websocket_accept` — byte-identical to the canonical test at `src/origin/mod.rs:74`.

Count check: 57 pass + 2 ignored (baseline) → 53 pass + 2 ignored (HEAD) = exactly the 4 removed. No other tests were dropped.

Core coverage confirmed present and passing in the `--all-features` run: QUIC e2e (`loopback::quic_tests::quic_tunnel_end_to_end`), H2 e2e (`loopback::h2_tests::h2_tunnel_end_to_end`), ws/tcp origins on both transports (`quic_websocket_tcp_round_trip`, `h2_websocket_tcp_round_trip`), wire goldens (7 tests, incl. go-reference), RPC exchange (4 `tunnel_client_*`), serde (`tunnel_enum_round_trip`, `named_tunnel_credentials_round_trip`, quick-tunnel API parse), backoff (bound, zero-base, cloudflared defaults), classification (5 h2 + 2 serve + connector transport selection).

### Residual risk: `serve_control` panics on unexpected control messages

The merged mock helper (`src/loopback/mod.rs:49-75`) is strictly test-only: `pub(crate)` inside `src/loopback/`, compiled only under `#[cfg(all(feature = "quick-tunnel", any_edge))]` + `#[cfg(test)]`. Assessment — **acceptable**:
- The old H2 helper (`run_control_rpc`) silently ignored unknown messages (`Some(_) => {}`), i.e. it could loop forever on a protocol mismatch; the new version fails loudly with the message in the panic text.
- The old QUIC helper returned `Err`; the new version panics. In the QUIC tests the helper runs in a spawned task whose `JoinHandle` is aborted without being checked, so neither the old `Err` nor the new panic fails the test directly — but a real protocol mismatch surfaces through the client-side register timeout or the response/body assertions anyway, and the tokio panic hook prints the offending message. No coverage is weakened versus the baseline.
- In the H2 tests the helper is awaited inline with `.expect("control rpc")`, so a panic fails the test there.
- One behavioral merge worth noting: the QUIC mock previously returned `Ok(())` on unregister (method_id 1) without writing a reply; the shared helper now writes `EMPTY_RETURN` before returning. The client's unregister therefore gets a proper reply — strictly closer to the real edge. All four loopback tests pass.

## 5. Dead code

Grep across `src/`, `rpc/src/`, `tests/`, `examples/`: `FLOW_ID_KEY`, `TRACE_ID_KEY`, `encode_connect_request_bytes`, `RpcError::Unimplemented`, `build_tcp_request` — all gone, none resurrected. Clippy `-D warnings` over all 16 combos reports zero dead-code warnings.

Kept-but-unused public items are justified:
- `rpc::quic::HTTP_HEADER_KEY` (`rpc/src/quic.rs:26`) is re-exported from the libcfd-rpc crate root (`rpc/src/lib.rs:46`); removing it would be a public-API break for consumers, and `libcfd`'s own `serve.rs` deliberately uses its transport-local `HttpHeader:` prefix. Keep as a documented constant of the wire-protocol crate.
- `default_config_json` is public and used by `EdgeOptions::default()`. `websocket_accept` is public API used by both transports and examples.

## 6. Dependencies

- `cargo-machete` (nixpkgs): "didn't find any unused dependencies" — clean.
- No BoringSSL in h2-only builds: `cargo tree --no-default-features --features named-tunnel,h2-edge` contains no `boring`/`boring-sys`/`quiche` (getrandom swap effective).
- tokio features in `lib` (`Cargo.toml:36`): `default-features = false` + `["net","rt","time","io-util","sync","macros","fs"]` — every one verified used in `src/` (net: TcpStream/UdpSocket/lookup_host; rt: task::spawn; time: sleep/timeout; io-util: AsyncRead/WriteExt; sync: Notify/watch/mpsc/oneshot; macros: select!/pin!; fs: edge.rs:96 read_to_string). Dev-deps all used (rcgen loopback certs; tokio-util compat; tracing-subscriber in live_edge + all 5 examples; `signal` via `tokio::signal::ctrl_c()` in examples; `rt-multi-thread` in loopback tests).

## 7. Full validation (run during this review)

| Command | Result |
|---|---|
| `cargo fmt --all --check` | passed |
| `cargo check --workspace --all-targets --all-features` | passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | passed, 0 warnings |
| `cargo test --workspace --all-features` | 53 passed, 2 ignored, 0 failed |
| 11-combo clippy matrix (`--no-default-features` + all 4 singles + all 6 pairs) `-D warnings` | passed, 0 warnings each |
| 5 remaining combos (4 triples + full) `cargo check` | passed |
| `cargo test --workspace --no-default-features --features quick-tunnel,quic-edge` | 24 lib + 4 exchange + 7 wire + 2 rpc passed, 2 live ignored |
| `cargo test --workspace --no-default-features --features named-tunnel,h2-edge` | 20 lib + 4 + 7 + 2 passed |
| `cargo test --workspace --no-default-features` (survey blocker E0432) | passed (live_edge skipped via `[[test]] required-features`, `Cargo.toml:84-85`) |
| `cargo doc --workspace --all-features --no-deps` | passed, no warnings |
| `cargo test --workspace --all-features -- --ignored` (real edge, network up) | **both live tests passed**: `create_and_save_quick_tunnel`, `live_quick_tunnel_over_quic_serves_http` |
| `nix run nixpkgs#cargo-machete` | clean |

## 8. Git state

- `git status --porcelain` empty; `.test-creds/` and `.pi-subagents/` gitignored (`.gitignore` untouched).
- Campaign = exactly 3 coherent commits on `master`: 9ea5256 (dep hygiene + feature guarding) → c674689 (module split + cfg factoring + dead code) → 758627a (test pruning + helper dedup). `flake.nix`, `.gitignore`, `AGENTS.md` unchanged across the campaign.
- Survey deliverables committed: `docs/research/improve-survey.md`, `docs/research/improve-survey-full.md` (in 9ea5256). Historical docs still referencing `loopback_test.rs` (review-a2.md) are dated records of the earlier state — not stale code references.

## Findings by severity

- **Blocker**: none.
- **Minor — libcfd-rpc dead-code removals are technically public-API removals**: `RpcError::Unimplemented` (variant of the root-re-exported enum) and `rpc::quic::{FLOW_ID_KEY, TRACE_ID_KEY, encode_connect_request_bytes}` were reachable public items of the 0.1.0 `libcfd-rpc` crate. All were never-constructed/never-called, the survey recommended their removal, and the wire protocol (capnp `exception::Type::Unimplemented`) is preserved. Semver-relevant in principle for external consumers of the rpc crate; acceptable for this pre-1.0 internal workspace, but worth a changelog line.
- **Note — `src/lib.rs:95`** still uses `#[cfg(any(feature = "quick-tunnel", feature = "named-tunnel"))]` for `pub use tunnel::Tunnel;` instead of the `any_tunnel` cfg; semantically identical (both combos verified), purely stylistic.
- **Note — `src/serve.rs` (386 lines)** is now the largest file, within the ~400 budget; a future split candidate, not a defect.
- **Note — `serve_control` panic channel**: in the QUIC tests the panic occurs in a spawned, unjoined task (diagnostic via tokio panic hook only); in the H2 tests it fails the test directly. Mock-only, acceptable per §4.

## Verdict

The campaign achieved all five goals — quiche (and its BoringSSL backend) fully gated behind `quic-edge` so h2-only builds drop them entirely; large files split into cohesive small modules (all < ~400 lines); dead code removed; dependencies stripped to feature-gated optionals with a minimal tokio feature set; redundant/tautological tests pruned with all core coverage retained — without regressions. Evidence: all 16 feature combos compile, all 11 clippy-matrix combos are zero-warning, the full suite passes, and both real-edge live tests pass against Cloudflare. The public API of every live item is unchanged; the only public changes are the four dead libcfd-rpc items removed by explicit campaign intent. The `serve_control` panic behavior is acceptable for test-only code.

## Follow-up recommendations

1. Emit a one-line note in the next release/changelog about the four libcfd-rpc removals (`RpcError::Unimplemented`, `FLOW_ID_KEY`, `TRACE_ID_KEY`, `encode_connect_request_bytes`) for any external consumers.
2. Optionally switch `src/lib.rs:95` to `#[cfg(any_tunnel)]` for consistency with the rest of the cfg factoring.
3. If file-size budgets stay a concern, `src/serve.rs` (386) is the next split candidate; not needed now.
4. Consider a `fn lock(&self)` helper for the ~14 `inner.lock().unwrap()` sites in `src/quic/{mod,stream}.rs` (pre-existing, out of campaign scope).
