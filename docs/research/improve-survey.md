Survey complete and written to `/home/aperso/libcfd/.pi-subagents/artifacts/outputs/758b90da/context.md`. Headline findings:

1. **quiche is NOT optional** — `Cargo.toml:32` has `quiche = "0.29.3"` unconditional; `cargo tree --no-default-features` confirms quiche, h2, boring, rustls, tokio-rustls, webpki-roots, bytes, serde, serde_json, uuid all compile with zero features. Root cause: unconditional `From<quiche::Error>` (error.rs:63) and `From<h2::Error>` (error.rs:69) block optionalization.
2. **Reorg candidates >400 lines**: loopback_test.rs (924), connector.rs (630), h2/mod.rs (521), origin/mod.rs (476), tunnel.rs (412).
3. **Feature gates** all correct except error.rs From-impls; 5 modules repeat the same compound cfg predicate (convoluted).
4. **Dep gaps**: h2, bytes, boring, rustls trio, webpki-roots, serde trio, uuid all candidate for `--optional`; cargo-machete found no *unused* deps (it can't see feature-only usage).
5. **Tests**: 59 total (57 pass, 2 ignored live). Removal candidates: duplicate `computes_websocket_accept`, duplicate `connect_request_round_trip`, low-value error/tunnel tests. `tests/live_edge.rs` lacks `[[test]] required-features` (fails `cargo test --no-default-features`).
6. **Dead code**: `RpcError::Unimplemented`, `FLOW_ID_KEY`, `TRACE_ID_KEY`, `HTTP_HEADER_KEY`, `encode_connect_request_bytes`, duplicated `build_tcp_request`/test helpers.
7. **Git**: clean tree, HEAD `491dd39`. No anyhow, no ctx/mgr violations, no TODO/FIXME.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Report written to /home/aperso/libcfd/.pi-subagents/artifacts/outputs/758b90da/context.md with file:line evidence across all 7 requested sections: module tree with cfg gates (src/lib.rs:43-126), per-file line counts, dependency gate analysis with cargo-machete run, feature-matrix verification (7 cargo check combos all passed), full 59-test inventory with redundancy flags, dead-code items (RpcError::Unimplemented rpc/src/error.rs:22, FLOW_ID_KEY/TRACE_ID_KEY rpc/src/quic.rs:50-52, encode_connect_request_bytes rpc/src/quic.rs:191), git state (HEAD 491dd39, clean), and style findings (no ctx/mgr, no anyhow, 14 mutex unwraps in src/quic/{mod,stream}.rs)."
    }
  ],
  "changedFiles": [
    ".pi-subagents/artifacts/outputs/758b90da/context.md"
  ],
  "testsAddedOrUpdated": [],
  "commandsRun": [
    {
      "command": "git log --oneline -20 && git status && git rev-parse HEAD",
      "result": "passed",
      "summary": "HEAD 491dd39 on master, working tree clean"
    },
    {
      "command": "nix run nixpkgs#cargo-machete",
      "result": "passed",
      "summary": "no unused dependencies detected (feature-only usage not detected by machete; covered manually via cargo tree)"
    },
    {
      "command": "cargo tree --no-default-features -e normal",
      "result": "passed",
      "summary": "proves quiche/h2/boring/rustls/tokio-rustls/webpki-roots/bytes/serde/serde_json/uuid still compile with zero features"
    },
    {
      "command": "cargo check (7 feature-matrix combos)",
      "result": "passed",
      "summary": "no-default-features; +quick-tunnel; +quick-tunnel,h2-edge; +quick-tunnel,quic-edge; +named-tunnel,h2-edge; +named-tunnel,quic-edge; +quick-tunnel,axum-origin all compile"
    },
    {
      "command": "cargo test --workspace --all-features",
      "result": "passed",
      "summary": "57 passed, 2 ignored (live_edge), 0 failed"
    },
    {
      "command": "cargo test --no-default-features",
      "result": "failed",
      "summary": "tests/live_edge.rs fails E0432 unresolved imports (no [[test]] required-features gate) - flagged as campaign item"
    }
  ],
  "validationOutput": [
    "cargo test --workspace --all-features: 43 lib unit + 2 rpc lib unit + 4 rpc_exchange + 8 wire passed, 2 live_edge ignored",
    "cargo-machete: no unused deps; manual grep analysis produced the feature-gating table"
  ],
  "residualRisks": [
    "boring stays heavy while used by edge.rs:286/connector.rs:349 RNG (rand_bytes) in addition to quic-edge; needs RNG swap before boring can be quic-edge-only",
    "quiche's default feature boringssl-boring-crate forces boring in QUIC builds regardless",
    "serde/serde_json gating on any(quick-tunnel, named-tunnel, h2-edge) couples config-JSON handling to tunnel serde; verify public API under named-tunnel+h2-edge without quick-tunnel",
    "loopback_test.rs split (924 lines) must preserve crate::-private access to control/quic/serve/h2/event",
    "cargo-machete 0.9.2 does not accept --workspace flag; ran without it"
  ],
  "noStagedFiles": true,
  "diffSummary": "Survey-only run: wrote the context report artifact; no source files modified",
  "reviewFindings": [
    "blocker: Cargo.toml:32 - quiche is non-optional and untied to quic-edge; error.rs:63 From<quiche::Error> unconditional prevents optionalization",
    "blocker: Cargo.toml:30 - h2 is non-optional and untied to h2-edge; error.rs:69 From<h2::Error> unconditional",
    "major: tests/live_edge.rs has no [[test]] required-features; cargo test --no-default-features fails to compile it",
    "minor: dead code rpc/src/error.rs:22 (RpcError::Unimplemented), rpc/src/quic.rs:50-52 (FLOW_ID_KEY, TRACE_ID_KEY), rpc/src/quic.rs:191 (encode_connect_request_bytes)",
    "minor: duplicate websocket_accept test (h2/headers.rs vs origin/mod.rs), duplicate connect_request_round_trip (rpc/tests/wire.rs vs rpc/src/quic.rs)",
    "minor: duplicated build_tcp_request (serve.rs:281 vs h2/mod.rs:444) and duplicated test helpers TokioBridge/StubHook/serve_mock in rpc/tests"
  ],
  "manualNotes": "No source files were edited; this is a survey deliverable for the parent to plan the multi-worker campaign. All findings carry file:line evidence in the context.md artifact."
}
```
