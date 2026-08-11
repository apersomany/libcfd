All checks complete. Let me compile the final review.

## Review

**Scope note:** `/home/aperso/libcfd/plan.md` and `/home/aperso/libcfd/progress.md` do not exist (no such files anywhere in the repo). I reviewed the 4 commits `77f4824, 4ef4204, 03dff95, 5d9b3da` against the cloudflared checkout and research briefs, and ran the full validation suite.

### 1. Protocol fidelity vs the checkout

| Item | Finding |
|---|---|
| (a) SRV query | `_v2-origintunneld._tcp.argotunnel.com` — exact (`edge.rs:12-14`, region prefix `{region}-v2-origintunneld` at `edge.rs:36-39`). Port from SRV record (`edge.rs:186-187`), 7844 fallback. Correct. |
| (a) Fallback | Deviates from cloudflared: checkout falls back to DoT `1.1.1.1:853` (`discovery.go:99,153-171`); libcfd queries resolv.conf UDP 53 then `1.1.1.1:53`, then hardcodes `region1/2.v2.argotunnel.com:7844` (`edge.rs:16,31-33`) — cloudflared has no hardcoded default list (only hidden `--edge`). Only differs when DNS fails entirely; ordering (first SRV target first ≈ region1-first) matches. |
| (b) ALPN/SNI | `argotunnel` + `quic.cftunnel.com` — exact (`quic/mod.rs:30-33`, matches `connection/protocol.go:38,74-78`). |
| (b) TLS CA pool | System + bundled 3 Cloudflare roots — but **the bundled PEM was unparseable** (see Blocker). Certs are byte-identical copies of `cloudflared/tlsconfig/cloudflare_ca.go` (verified by sha256). They are public root-CA data, not source code, so the AGENTS.md no-copy rule is not materially violated; still, the file has no provenance note. |
| (c) Control sequence | bootstrap→register→updateLocalConfiguration (if `connIndex==0 && !remotelyManaged`)→unregister→release — exact (`control.rs:56-91`, `run.rs:124-127`; matches `control.go:84-121`). Question ids 0/1/2/3 match. **But ConnectionOptions deviates** (see Major-1). |
| (d) Request framing | `[6B 0A 36 CD 12 A1 3E][2B "01"]` + capnp ConnectRequest + raw body; response mirror — byte-exact (`serve.rs:118-129`, verified against `tunnelrpc/quic/protocol.go` + `request_server_stream.go`). Metadata keys `HttpMethod`/`HttpHost`/`HttpHeader:`/`HttpStatus` match. `cf-trace-id` is not propagated (tracing-only loss). |
| (e) Keepalive | 1s ack-eliciting PING, 5s idle, 30MiB/6MiB windows, `max_streams_bidi=2^60`, no 0-RTT — matches `quic/constants.go` + `supervisor/tunnel.go`. No application-level ping ✓. |

### 2. Public API
- No tokio types in public signatures; every exposed future is `Send` (RPITIT `handle` + boxed `HttpOriginDyn` at `origin/mod.rs:95-125`). Sound object-safety split. Blanket `impl HttpOrigin for F` for closures.
- `Body` streams via `futures_util::io::AsyncRead`; `size_hint` present; content-length passthrough happens through `HttpHeader:content-length` metadata (`serve.rs:152-155`). Read-only body will need extension (writable half) for WS/TCP in Phase B — additive, non-breaking.
- `Error` is `thiserror`-based with `#[source]` (`error.rs`); String-based variants for Phase A are fine per AGENTS.md.
- Phase B extension (Tunnel trait, WS/TCP origins, H2, EdgeConnector) is additive: `QuickTunnel` is already Serde-serializable (`tunnel.rs:36`), and internal layers are `pub(crate)`.
- **Caveat:** docs say "callers drive the returned futures on their own executor" (`lib.rs:9-10`), but the futures require a tokio runtime (`tokio::net/time/spawn/sync` inside). Type-level runtime-agnosticism is satisfied; the doc claim is optimistic.

### 3. libcfd-rpc integration
- Root crate has zero direct `capnp` usage (only a comment). All RPC goes through `libcfd_rpc::*` ✓ (AGENTS.md constraint).
- **M0 fixes verified independently:** I regenerated all 9 goldens from `rpc/tests/wire.rs` with a Go program running the vendored capnp-go v2.18.0 from the checkout — **all 9 match byte-for-byte** (bootstrap, finish, release, empty-return, bootstrap-return, registerConnection call, register-return, connect-request, connect-response). The goldens are truthful.
- Size cap: Go `defaultDecodeLimit` = 64 MiB of segment data; Rust `MAX_TOTAL_WORDS = 8*1024*1024` words = 64 MiB ✓ (`io.rs:73-75`). Finish-on-exception sends `releaseResultCaps=true` ✓ (matches `rpc.go:468-471`). `close()` sends `release{id:0, refcount:1}` ✓ (matches `tables.go:importClient.Close`). Nit: Go allows up to 513 segments (`maxSeg ≤ 512`), Rust rejects `segment_count > 512` (`io.rs:22`) — off-by-one, harmless.

### 4. Loopback test
Genuine end-to-end over real QUIC loopback: handshake → register → updateLocalConfiguration → request stream → origin handler → response verified (`loopback_test.rs:215-286`). Registration replies are **literal capnp-go bytes** replayed via `write_raw` — independent of the client encoder. Independence limits: (1) data-stream framing shares the `libcfd_rpc::quic` codec with the client (codec correctness is independently anchored by the capnp-go goldens); (2) both endpoints run the same quiche driver, so a driver bug would be symmetric — real-edge interop is the remaining risk. The mock never validates the *content* of the registerConnection call.

### 5. Code quality
No `unsafe` anywhere ✓. No credential/token/account logging ✓ (verified by grep). `tracing` only, no global subscriber in library code ✓. Clippy clean ✓. Unused: `uuid` feature `v4` (`Cargo.toml:25`), tokio `signal` used only by the example. Naming: `conn`/`opts`/`reg_opts` are truncations that arguably violate the letter-per-word rule — nit. Removed a dead `#[allow(dead_code)] _assert_error_conversion` stub during the fix.

### 6. flake.nix
Change is a devshell-only `shellHook` (CC/CXX/CFLAGS for the boring-sys cmake build) — minimal, correct, repo-local. No NixOS system mutation (`~/dotfiles` untouched; `nixosConfiguration` absent).

### 7. Validation (all run inside `nix develop`, full suite twice — cached and fresh `CARGO_TARGET_DIR=/tmp/cfd-target`)
- `cargo fmt --all --check` — **passed**
- `cargo check --workspace --all-targets --all-features` — **passed** (fresh build, exit 0)
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — **passed** (fresh)
- `cargo test --workspace --all-features` — **24/24 passed** including `loopback_test::quic_tunnel_end_to_end`

### Findings by severity

- **Blocker (fixed):** `src/quic/cloudflare_origin_ca.pem:75` — stray `` `) `` bytes after the final `-----END CERTIFICATE-----`. `X509::stack_from_pem` fails with `BAD_END_LINE`, so the **default** production path (`RunOptions::default()`, `ca_cert_pem: None` → system roots + bundled CF roots, `tls.rs:25-27`) always failed before dialing. The loopback test masked it by passing its own CA. I removed the two stray bytes, deleted the dead `_assert_error_conversion` stub, and added `bundled_cloudflare_roots_parse` (`tls.rs:60-67`) asserting the 3 roots parse. Re-verified with a scratch boring program: parses 3 certs after fix.
- **Major:** `src/control.rs:37-48` — ConnectionOptions ≠ what control.go/client.go send for a quick tunnel: `features` is empty (cloudflared sends `["allow_remote_config","serialized_headers","support_datagram_v2","support_quic_eof","management_logs"]`, `features/features.go:5-26`), `origin_local_ip` is empty (cloudflared sends the resolved local socket IP), `num_previous_attempts` is always 0. Missing `support_quic_eof`/`serialized_headers` can change edge behavior; must be fixed before real-edge validation.
- **Minor:** `tls.rs:19-22` — `Some(ca_cert_pem)` replaces the entire trust store; cloudflared appends to system+CF roots.
- **Minor:** `control.rs:99-108` — unregister has no timeout (cloudflared bounds it by grace period, default 30s); shutdown can hang on a dead edge.
- **Minor:** `serve.rs:110-115` — no `CancelWrite(0)`/RST_STREAM when an error occurs after the response preamble is sent (cloudflared does this at `quic_connection.go:203-206`).
- **Minor:** edge discovery fallback differs from checkout (DoT vs UDP-53 + hardcoded hostnames); documented in `edge.rs` doc comment but the deviation is real.
- **Nit:** `io.rs:22` segment-count cap off-by-one vs capnp-go (513 allowed there, 512 here). `uuid` `v4` feature unused. `serve_requests` `active` set never shrinks (`serve.rs:27`). `conn`/`opts` naming vs the abbreviation rule. `default_config_json` sends `"warp-routing":{}` vs cloudflared's `{"connectTimeout":0,"tcpKeepAlive":0}` (functionally equivalent zero-values).

### Verdict
Phase A achieves the AGENTS.md parity goals (create quick tunnel; discover + connect over QUIC; register + maintain; deliver requests to origin handler; return responses), with the TLS default path now fixed. The 3 remaining substantive items before Phase B/real-edge validation: (1) populate ConnectionOptions to match cloudflared (feature list, originLocalIp, numPreviousAttempts), (2) decide unregister timeout semantics, (3) decide/implement DoT fallback parity. API shape is Phase-B-extensible without breaking changes.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Review was scoped to the 4 Phase A commits (77f4824, 4ef4204, 03dff95, 5d9b3da). No scope widening: the only edits I made are a 2-byte corrective fix to the bundled CA PEM (src/quic/cloudflare_origin_ca.pem:75, stray backtick+paren after the final END marker that broke X509::stack_from_pem with BAD_END_LINE), removal of the dead _assert_error_conversion stub, and one regression test (src/quic/tls.rs) asserting the 3 bundled roots parse. The implementation itself covers exactly the Phase A goals; ConnectionOptions fidelity gaps (features/originLocalIp) are flagged for Phase B, not silently widened."
    },
    {
      "id": "criterion-2",
      "status": "satisfied",
      "evidence": "Full validation suite run inside `nix develop` on a fresh CARGO_TARGET_DIR=/tmp/cfd-target: fmt, check, clippy -D warnings, and tests all pass (24/24). Independently regenerated all 9 RPC goldens with capnp-go v2.18.0 from the cloudflared checkout: all 9 match byte-for-byte, proving the golden tests are truthful. Reproduced the bundled-PEM parse failure with a scratch boring program (BAD_END_LINE) and confirmed it parses 3 certs after the fix. Every protocol claim is cited to cloudflared files (control.go, protocol.go, request_server_stream.go, features.go, mem.go, rpc.go, tables.go)."
    }
  ],
  "changedFiles": [
    "src/quic/cloudflare_origin_ca.pem",
    "src/quic/tls.rs",
    "src/api.rs",
    "src/control.rs",
    "src/edge.rs",
    "src/error.rs",
    "src/lib.rs",
    "src/loopback_test.rs",
    "src/origin/mod.rs",
    "src/quic/mod.rs",
    "src/quic/stream.rs",
    "src/run.rs",
    "src/serve.rs",
    "src/tunnel.rs",
    "examples/quick_tunnel.rs",
    "flake.nix",
    "rpc/src/error.rs",
    "rpc/src/io.rs",
    "rpc/src/lib.rs",
    "rpc/src/quic.rs",
    "rpc/src/rpc.rs",
    "rpc/src/tunnel.rs",
    "rpc/tests/rpc_exchange.rs",
    "rpc/tests/wire.rs",
    "rpc/schemas/rpc.capnp",
    "rpc/schemas/tunnelrpc.capnp",
    "rpc/schemas/quic_metadata_protocol.capnp"
  ],
  "testsAddedOrUpdated": [
    "src/loopback_test.rs (quic_tunnel_end_to_end)",
    "rpc/tests/wire.rs (9 goldens + parse/framing tests)",
    "rpc/tests/rpc_exchange.rs (4 RPC exchange tests)",
    "src/quic/tls.rs (bundled_cloudflare_roots_parse, added during review)",
    "src/edge.rs, src/tunnel.rs, src/api.rs, rpc/src/quic.rs (unit tests)"
  ],
  "commandsRun": [
    {
      "command": "nix develop -c 'cargo fmt --all --check'",
      "result": "passed",
      "summary": "formatting clean"
    },
    {
      "command": "nix develop -c 'cargo check --workspace --all-targets --all-features' (fresh CARGO_TARGET_DIR=/tmp/cfd-target)",
      "result": "passed",
      "summary": "fresh full-workspace check, exit 0"
    },
    {
      "command": "nix develop -c 'cargo clippy --workspace --all-targets --all-features -- -D warnings' (fresh target dir)",
      "result": "passed",
      "summary": "no clippy warnings, exit 0"
    },
    {
      "command": "nix develop -c 'cargo test --workspace --all-features' (fresh target dir)",
      "result": "passed",
      "summary": "24/24 tests passed incl. loopback e2e (was 23 before the added regression test)"
    },
    {
      "command": "go run (capnp-go v2.18.0 from cloudflared/vendor) regenerating all 9 RPC goldens",
      "result": "passed",
      "summary": "all 9 goldens in rpc/tests/wire.rs byte-identical to capnp-go output"
    },
    {
      "command": "scratch boring crate parsing src/quic/cloudflare_origin_ca.pem",
      "result": "failed",
      "summary": "before fix: X509::stack_from_pem -> BAD_END_LINE due to trailing '`)' bytes; after fix: parses 3 certs (re-run passed)"
    }
  ],
  "validationOutput": [
    "cargo fmt --all --check: passed",
    "cargo check --workspace --all-targets --all-features: passed (fresh build, 0.07s cached + 15.99s fresh)",
    "cargo clippy --workspace --all-targets --all-features -- -D warnings: passed",
    "cargo test --workspace --all-features: libcfd 10/10, libcfd-rpc 2/2 + 4/4 + 8/8, doc-tests 0; all ok",
    "capnp-go golden cross-check: 9/9 byte-identical (bootstrap, finish, release, empty-return, bootstrap-return, call, register-return, connect-request, connect-response)"
  ],
  "residualRisks": [
    "ConnectionOptions sent to the edge do not match cloudflared: features list is empty (cloudflared sends 5 default features incl. support_quic_eof and serialized_headers), origin_local_ip empty, num_previous_attempts always 0 (src/control.rs:37-48). Untested against a live edge.",
    "Real-edge interop is unverified: the loopback test runs the same quiche driver on both endpoints, and the mock never validates the registerConnection params content. Registration replies are genuine capnp-go byte replays, and the codec is golden-verified, so risk is confined to the driver and edge behavior.",
    "Edge discovery fallback differs from cloudflared (UDP-53 + hardcoded region1/2.v2.argotunnel.com instead of DoT 1.1.1.1:853) - only matters when DNS fails.",
    "Unregister RPC has no timeout (cloudflared bounds it by the 30s grace period); shutdown can hang on a dead edge (src/control.rs:99-108).",
    "No post-ack error signaling (CancelWrite/RST_STREAM) on data streams (src/serve.rs:110-115).",
    "The public futures require a tokio runtime to execute, despite the lib.rs doc claiming any executor works.",
    "PEM provenance (copied verbatim from cloudflared tlsconfig/cloudflare_ca.go) has no attribution note; public root-CA data, low risk."
  ],
  "noStagedFiles": true,
  "diffSummary": "Phase A: quick tunnel API client (api.rs, tunnel.rs), DNS-SRV edge discovery (edge.rs), quiche QUIC driver with boring TLS + bundled Cloudflare roots (quic/), capnp RPC registration control stream (control.rs, rpc/), request serving to a consumer HttpOrigin (serve.rs, origin/), run orchestration with reconnect/backoff (run.rs), loopback mock-edge e2e test, flake devshell fix. Reviewer fix: removed stray '`)' bytes from cloudflare_origin_ca.pem (was breaking default TLS path) + added regression test + removed dead code.",
  "reviewFindings": [
    "blocker (fixed during review): src/quic/cloudflare_origin_ca.pem:75 - trailing '`)' made X509::stack_from_pem fail (BAD_END_LINE), breaking the default RunOptions TLS path; bytes removed, regression test added, verified",
    "major: src/control.rs:37-48 - ConnectionOptions.features empty vs cloudflared's 5 default features; origin_local_ip empty; num_previous_attempts always 0",
    "minor: src/edge.rs:16,31-33 - fallback chain differs from checkout (hardcoded hostnames instead of DoT 1.1.1.1:853)",
    "minor: src/control.rs:99-108 - unregister has no timeout vs cloudflared grace-period bound",
    "minor: src/serve.rs - no RST_STREAM on post-preamble errors; cf-trace-id not propagated",
    "minor: src/quic/tls.rs - custom ca_cert_pem replaces trust store instead of appending",
    "nit: rpc/src/io.rs:22 segment cap 512 vs capnp-go 513; uuid v4 feature unused; active-set in serve_requests never shrinks; conn/opts naming vs letter-per-word rule",
    "no blockers remain after the PEM fix"
  ],
  "manualNotes": "plan.md and progress.md do not exist in the repo (the task referenced them; no such files found anywhere). The 4 reviewed commits are the 4 most recent (77f4824, 4ef4204, 03dff95, 5d9b3da); commit 14f7df5 (capnp tunnel registration RPC) is the 5th and was read as context. Working tree was clean before review; my 2 corrective edits (PEM, tls.rs) are unstaged. Before Phase B: populate ConnectionOptions per cloudflared (features, originLocalIp, numPreviousAttempts), bound the unregister timeout, and decide DoT-fallback parity. The API is Phase-B-extensible without breaking changes."
}
```
