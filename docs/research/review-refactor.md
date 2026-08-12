# Independent review — source-layout refactor (ae571ca + c8bb523)

Repo: /home/aperso/libcfd (workspace: libcfd + rpc). Reviewed tree: HEAD c8bb523
(parent ae571ca; pre-refactor baseline 1c5b1f2). Method: static review of the
current tree plus the exact pre-refactor file contents and recorded validation
outputs from the refactor worker session
(.pi-subagents/artifacts/3bbd1906_worker_transcript.jsonl). This reviewer had
no shell tool, so git/cargo were not re-run; every recorded result below is
cited to the transcript.

## 1. Public API stability

Crate-root re-exports before (1c5b1f2) vs after (HEAD), compared item by item:

| Re-export | Before | After | Change |
|---|---|---|---|
| EdgeConnector, EdgeOptions, Transport, default_config_json | `pub use connector::{...}` (src/lib.rs) | `pub use edge::{...}` (src/lib.rs:67) | source path only |
| Error | `pub use error::Error` | same (src/lib.rs:68) | none |
| AxumOrigin | `#[cfg(axum-origin)] pub use origin::axum::AxumOrigin` | same (src/lib.rs:70) | none |
| Body, Duplex, HttpOrigin, HttpOriginDyn, Origin, ReadHalf, Request, Response, TcpOrigin, TcpOriginDyn, WebSocketConnection, WebSocketOrigin, WebSocketOriginDyn, WriteHalf, websocket_accept | `pub use origin::{...}` | same (src/lib.rs:71-75) | none |
| RunOptions, run_quick_tunnel | `pub use run::{...}` | same (src/lib.rs:77) | none |
| NamedTunnel / Tunnel / QuickTunnel, QuickTunnelOptions, create_quick_tunnel | `pub use tunnel::{...}` | same (src/lib.rs:79-83) | none |

Public function return types unchanged (all `Result<_, libcfd::Error>` via
crate::error::Result): create_quick_tunnel, run_quick_tunnel,
EdgeConnector::run, NamedTunnel::from_credentials_file, tunnel_id_bytes x3,
HttpOrigin::handle, WebSocketOrigin::connect, TcpOrigin::connect,
AxumOrigin::handle, websocket_accept, default_config_json. libcfd-rpc crate
root re-exports untouched (rpc/src/lib.rs).

Additive surface (not breaking, intentional for the composed error design):
- src/lib.rs:55 `pub mod edge`, :60 `pub mod origin`, :64 `pub mod tunnel`
  (were private modules with root re-exports);
- new public paths origin::{http,websocket,tcp} (src/origin/mod.rs:14-16),
  origin::http::body (src/origin/http/mod.rs:3);
- new public per-level errors libcfd::tunnel::Error (src/tunnel/mod.rs:4),
  libcfd::edge::Error (src/edge/mod.rs:11), libcfd::origin::Error
  (src/origin/mod.rs:9).

Error variant shape changed as requested by c8bb523 (flat -> nested), e.g.
Error::QuickTunnelApi(s) -> Error::Tunnel(tunnel::Error::QuickTunnelApi(s)),
Error::Origin(String) -> Error::Origin(origin::Error::Handler(String)),
Error::Io(e) -> Error::Edge(edge::Error::Io(e)). Return types unaffected.

## 2. Layout matches the request

- tunnel/{mod,named,quick,secret,error}; edge/{mod,discovery,control,serve,
  event,roots,error,connector/{mod,options,runtime,backoff},quic/{mod,stream,
  tls},h2/{mod,headers,register,stream,streams,tls}}; origin/{mod,error,
  duplex,pump,http/{mod,body},websocket,tcp,axum}; loopback/{mod,mock_edge,
  quic_tests,h2_common,h2_tests,h2_ws_tests}; run.rs; error.rs; lib.rs.
- origin/traits.rs split by protocol into origin/{http,websocket,tcp}/mod.rs.
- No file over ~400 lines: max src/edge/serve.rs = 386 (probed; worker wc -l
  confirms: 386/348/311/270/259/243/228/220/214/208/199 top ten).
- tunnel/secret.rs kept at tunnel level (shared by QuickTunnel.secret and
  NamedTunnel.tunnel_secret; moving under named/ would break quick-only).

## 3. Error design

- tunnel::Error (src/tunnel/error.rs): QuickTunnelApi/QuickTunnelResponse/
  QuickTunnelRequest (quick-tunnel), NamedTunnelCredentials (named-tunnel),
  InvalidTunnelId (ungated; enum only exists under any_tunnel, both tunnel
  features need it). Doc comments on every variant; useful Display strings;
  no credentials in messages.
- edge::Error (src/edge/error.rs): EdgeDiscovery, Registration(#[source]),
  DuplicateConnection, Control(#[from] RpcError), Io(#[from] io::Error)
  ungated; Quic + Tls under quic_any; H2 under h2_any. From impls for
  boring::ErrorStack and quiche::Error (quic_any), h2::Error (h2_any).
- origin::Error (src/origin/error.rs): Handler(String), Io(#[from] io::Error).
- crate Error (src/error.rs:9-21) composes Tunnel(any_tunnel)/Edge(edge_conn)/
  Origin with #[from]; From<String> -> Origin::Handler preserves the old
  `Err("...".into())` ergonomics; From<io::Error>/From<RpcError> route to
  Edge under edge_conn; pub(crate) constructors (edge_discovery, quic, h2,
  registration, duplicate_connection, edge_io, quick_tunnel_api, ...) keep
  call sites explicit and cfg-consistent. is_permanent() retained.
- No dead variants: every variant is constructed somewhere (verified by
  grep); the cfg_attr allow(dead_code) sites (region_override, pump,
  websocket_accept, Origin struct) are feature-conditional, not dead.
- Consistency: idiomatic thiserror usage; the only cosmetic gap is
  src/error.rs having no `//!` module doc (module is private, missing_docs
  does not apply).

## 4. Gating

- build.rs unchanged: any_tunnel/any_edge/edge_conn/quic_any/h2_any emitted
  and check-cfg'd. New tree uses them correctly: lib.rs (edge_conn at :54,
  any_tunnel at :63, loopback all(qt,any_edge) at :57-59), edge/mod.rs
  (h2_any :13, quic_any :15,18), error.rs (any_tunnel/edge_conn/quic_any/
  h2_any), edge/error.rs, tunnel/error.rs.
- Cargo.toml unchanged by the commits: quiche/boring only under quic-edge,
  h2/bytes only under h2-edge, axum/tower/bytes only under axum-origin; the
  previous review verified the 16-combo compile matrix and cargo-tree absence
  at this exact feature graph; 11-combo clippy matrix re-run green after M2
  (recorded), plus full-features clippy -D warnings 0.

## 5. Regression

- All prior tests intact and passing (53 pass, 2 ignored, recorded): QUIC
  e2e http + ws/tcp (src/loopback/quic_tests.rs), H2 e2e + ws/tcp
  (src/loopback/h2_tests.rs, h2_ws_tests.rs), wire goldens and rpc exchange
  (rpc/tests, untouched), serde round-trips (tunnel/mod.rs,
  tunnel/named/mod.rs, tunnel/quick/mod.rs), backoff (connector/backoff.rs),
  classification (edge/serve.rs, edge/h2/mod.rs, connector/options.rs),
  error display test (src/error.rs).
- 2 live tests intact with #[ignore] (tests/live_edge.rs). Live run passed
  (200 live-ok/hello, 0.49s) after deleting an expired tunnel credential; the
  first attempt failed on DNS of the stale hostname — environmental, not a
  code issue.
- Examples (quick_tunnel, h2_tunnel, named_tunnel, origin_ws_tcp, axum_tunnel)
  compile via --all-targets with their required-features (Cargo.toml:78-95).

## 6. Quality

- Module docs present on all level roots; intra-doc links resolve under
  --all-features (worker fixed 7 rustdoc warnings mid-session in edge/mod.rs,
  error.rs, origin/axum/mod.rs). The --no-default-features doc warnings
  (unresolved create_quick_tunnel/run_quick_tunnel/EdgeConnector/Tunnel/...)
  are the known pre-existing crate-doc-link issue, not introduced here.
- No dead code (clippy 0 warnings, 11 combos); no duplicated helpers
  (single hex, serialize_headers, websocket_accept, pump, peer_ip_bytes).
- CHANGELOG.md has no entry for the re-layout or per-level errors; the
  existing entries remain accurate. Recommendation: add one line.

## 7. Validation (recorded, not re-run by this reviewer)

| Command (nix develop) | Recorded result |
|---|---|
| cargo fmt --all --check | passed |
| cargo check --workspace --all-targets --all-features | passed (incl. examples) |
| cargo clippy --workspace --all-targets --all-features -- -D warnings | passed, 0 warnings |
| cargo test --workspace --all-features | 53 passed, 2 ignored |
| 11-combo clippy matrix -D warnings | passed, 0 warnings each |
| cargo test qt,quic-edge / nt,h2-edge (no-default) | passed (24+2+4+7 / 20+2+4+7) |
| cargo doc --workspace --all-features --no-deps | passed, no warnings |
| cargo test --workspace --all-features -- --ignored live | passed (after creds refresh) |

Supervisor re-run commands: the exact commands above, plus
`cargo tree --no-default-features --features named-tunnel,h2-edge` (no
quiche/boring) and `--features h2-edge`/`--features quick-tunnel` variants.

## 8. Git state

HEAD = c8bb523 (split errors into per-level types), parent ae571ca
(reorganize source tree), grandparent 1c5b1f2. Both commit messages match the
task; working tree clean (git status empty, recorded); .test-creds/ and
.pi-subagents/ gitignored. Note: ae571ca swept the untracked
docs/research/review-improve.md in via git add -A.

## Findings by severity

- Blocker: none.
- Minor: additive public module/error paths (edge/tunnel/origin became pub);
  Error variant reshape breaks consumer match arms (intended, per-level
  design); CHANGELOG lacks a refactor entry.
- Note: origin::Error::Io unreachable internally under edge_conn (io errors
  route to Edge::Io); live-test DNS flake with stale creds (environmental);
  review-improve.md swept into ae571ca.

## Verdict

The tree matches the request, the per-level error types are in place and
sound (cfg-gated, composed, documented, no credentials), and no regressions
were found in API names/signatures, test coverage, feature gating, or docs.
Validation was recorded green on this exact tree; recommend re-running the
suite once from a shell-capable session as final attestation.

## Follow-up recommendations

1. Add a CHANGELOG line for the re-layout and the per-level error restructure
   (variant paths changed for consumers).
2. Acknowledge the additive public module paths (edge/origin/tunnel) in the
   next release notes; they are intentional and required by the error design.
3. Consider documenting that origin-handler io errors surface as Edge::Io
   under edge_conn configs (or routing them to Origin::Io), whichever is the
   intended classification.
