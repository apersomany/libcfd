# Test suite

The workspace is tested in layers, from offline unit tests up to real
Cloudflare edge interoperability. See `PLAN.md` for the full design.

## Layout

| Path | Coverage |
|---|---|
| `src/**` unit tests | Pure functions and local transformations: parsers, serializers, headers, DNS encoding/decoding, configuration parsing, transport selection, backoff, websocket accept, dropped responder handling. |
| `rpc/tests/wire.rs` | Cap'n Proto framing, size limits, malformed input, and golden wire tests. The golden bytes were produced by capnp-go v2.18.0 (the wire format cloudflared uses) and the crate's own encoders are asserted byte-identical to them. |
| `rpc/tests/rpc_exchange.rs` | The complete registration lifecycle over an in-memory stream: bootstrap, register (success, permanent/retryable rejection, remote exceptions, wrong answer ids, unexpected message kinds, EOF, garbage), unregister, and capability release on close. Uses a scripted mock edge. |
| `tests/public_api.rs` | External-consumer style tests that use only the public API, including compile-time `Send` checks on every public future. Runs offline. |
| `tests/live_quick.rs` | Quick tunnels against the real edge: quinn QUIC, HTTP/2, quiche, and a websocket echo. |
| `tests/live_named.rs` | Named tunnels against the real edge: remotely-managed configuration via the edge push, HTTP over quinn/HTTP/2/quiche, plus websocket and raw TCP echo tests. |
| `tests/support/` | Shared test-only support (never exposed by the `libcfd` crate): the HTTPS client, the quick/named state managers, the origin handlers, and the run/poll/shutdown scaffolding. |
| `tests/state/` | Live credentials, gitignored. `quick_tunnel.json` caches quick-tunnel credentials; `named_tunnel.json` holds normalized named credentials; `named-token.txt` is the local dashboard connector token. |

## Offline commands

```text
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --workspace --no-default-features
```

Plain `cargo test` never touches the network and never needs credentials.
The live tests are `#[ignore]`d and only run on demand.

## Feature matrix

`scripts/feature-matrix.sh` compiles every supported combination: default
features (quinn + HTTP/2), quick tunnel with quinn only, quick tunnel with
HTTP/2 only, quick tunnel with quiche, named tunnel with each transport,
single transport features, bare tunnel features, and no default features.
The quiche jobs need `cmake`/`clang` (BoringSSL) and are skipped with a note
when the toolchain lacks them.

Every live-test target carries explicit feature requirements, so
`--no-default-features` builds compile the integration binaries as empty
files rather than failing on unavailable APIs.

## Live tests

```text
scripts/live-test.sh
```

The runner:

- runs the quick suite under default features (quinn + HTTP/2);
- runs the named suite only when a token is available — `NAMED_TUNNEL_TOKEN`
  or `tests/state/named-token.txt` — and otherwise omits it loudly, never
  silently;
- runs the quiche backend when `LIBCFD_LIVE_QUICHE=1`;
- always passes `--test-threads=1`, because the tests reuse one tunnel
  identity per suite.

State rules:

- Quick-tunnel credentials are cached in `tests/state/quick_tunnel.json`
  and validated (fields + hostname resolution) before reuse. A stale cache
  is removed and a fresh tunnel created once; a cached tunnel that fails to
  serve is replaced at most once per run.
- All state access is serialized with an exclusive lock over
  `tests/state/.live.lock`, so concurrent test processes cannot create or
  register duplicate tunnels.
- The named token is normalized into `tests/state/named_tunnel.json` and
  loaded through `NamedTunnel::from_credentials_file` so the file-loader
  path is exercised. The raw token is never stored in generated state.
- State files are written through a temporary file followed by an atomic
  rename, with owner-only permissions.

Requirements per test:

- `live_named_*` need the tunnel to be remotely managed with an ingress
  route so the edge pushes a configuration with a routed hostname. The
  websocket test needs a websocket-capable route; the TCP test needs a
  `tcp://` ingress route, whose hostname is discovered from the remote
  configuration's service entries (Cloudflare carries the route's bytes as
  a websocket connection, which the tunnel's websocket origin handler
  serves). Missing prerequisites fail loudly with an actionable message.
- A failed test reports the last observed status and body, never
  credentials.

## Secret hygiene

`scripts/check-secrets.sh` verifies that every `tests/state` file is
gitignored and that no source file logs a secret or token as a value. Run
it in CI to guarantee the "no credential file is tracked, logged, or
uploaded" acceptance criterion.
