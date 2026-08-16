# Total test refactor plan

## Objective

Replace the current collection of mostly inline helper tests with a layered test suite that verifies public behavior, RPC wire compatibility, transport behavior, and real Cloudflare edge interoperability.

The real edge suite is the primary integration test. Deterministic tests remain necessary for protocol framing, malformed input, error handling, feature selection, and failure paths that are difficult or expensive to reproduce against Cloudflare.

## Test layout

Use the following structure:

* `src/**` keeps only small unit tests for private pure functions and local data transformations.
* `rpc/tests/wire.rs` contains Cap'n Proto framing, size limits, malformed input, and golden wire tests.
* `rpc/tests/rpc_exchange.rs` exercises bootstrap, registration, error responses, unregister, finish, and release behavior over an in-memory stream.
* `tests/support/` contains shared test-only clients, HTTPS response parsing, state management, synchronization, and origin handlers.
* `tests/live_quick.rs` exercises quick tunnels against the real edge.
* `tests/live_named.rs` exercises named tunnels against the real edge.
* `tests/state/quick_tunnel.json` stores reusable quick-tunnel credentials locally.
* `tests/state/named_tunnel.json` stores normalized named-tunnel credentials locally.
* `scripts/live-test.sh` prepares eligible state and runs explicitly selected ignored tests.

The `research/cloudflared/` checkout remains read-only and is used only as a protocol reference. Go-generated wire values may be retained as test fixtures where they document compatibility.

## Unit-test refactor

1. Inventory every existing inline test and classify it as unit, protocol, component, or live-edge coverage.
2. Keep parser, serializer, header, DNS, configuration, transport-selection, and backoff tests close to their implementation.
3. Add negative cases for malformed JSON, invalid UUIDs, truncated DNS and HTTP messages, invalid Cap'n Proto framing, oversized segments, invalid headers, and dropped responders.
4. Replace weak self-round-trip assertions with exact expected values or golden vectors where the behavior is an external protocol contract.
5. Remove duplicate tests and tests that only verify derive-generated conversions.
6. Add compile-time checks for the `Send` requirement on public futures and external-consumer-style tests for the public API.

## RPC integration tests

Restore the RPC integration coverage that existed before the test-suite removal.

The tests must verify:

* bootstrap returns the expected capability;
* register connection encodes all credentials and connection options;
* successful registration decodes connection details;
* retryable and permanent registration failures remain distinguishable;
* remote exceptions and unexpected message kinds become typed errors;
* unregister sends the expected call and finish;
* closing releases the bootstrapped capability;
* partial reads and writes do not corrupt framing;
* EOF, wrong answer identifiers, malformed messages, and size-limit violations fail safely.

Use a small scripted peer rather than a general-purpose mock server. Keep Go-compatible golden messages for interoperability, but make the exchange tests exercise the Rust client behavior directly.

## Live state management

Implement a shared state manager in `tests/support/`.

### Quick tunnel state

* Load and validate `tests/state/quick_tunnel.json` as `QuickTunnel`.
* Validate required fields and the tunnel identifier before use.
* Check hostname resolution before starting the tunnel.
* Create a quick tunnel only when explicitly running the live suite and no usable state exists.
* Write state through a temporary file followed by an atomic rename.
* Serialize state access with a lock so concurrent test processes cannot create multiple tunnels.
* If registration or the public request proves the cached tunnel stale, remove it and retry creation once.
* Never print the secret or serialized credentials.

### Named tunnel state

* Require `NAMED_TUNNEL_TOKEN` for named live-test setup.
* Parse the token with `NamedTunnel::from_token`.
* Serialize the normalized credentials to `tests/state/named_tunnel.json`.
* Run the file-loader path with `NamedTunnel::from_credentials_file`.
* Do not store the raw token in generated state.
* Do not create, delete, or mutate the Cloudflare named tunnel or its routes.
* If the token is unavailable, the live-test runner must omit the named suite instead of silently passing a test.

All state files are secrets or contain secrets. They must remain ignored, must not be included in CI artifacts, and must not be logged.

## Real-edge test matrix

Use the same cached tunnel identity across transport tests to avoid unnecessary API calls.

### Quick tunnel tests

* Quick tunnel over the quinn QUIC backend with an HTTP origin.
* Quick tunnel over HTTP/2 with an HTTP origin.
* Quick tunnel over the quiche QUIC backend when that backend is enabled.
* Verify public HTTPS status, response body, request path, origin invocation, startup polling, and bounded shutdown.

### Named tunnel tests

* Named tunnel over QUIC with a remotely managed configuration.
* Named tunnel over HTTP/2 with a remotely managed configuration.
* Verify the remote configuration callback supplies routed hostnames.
* Use a callback-provided hostname for the public HTTPS request.
* Verify registration, origin invocation, response contents, and bounded shutdown.

### Origin protocol coverage

Add real-edge WebSocket coverage where a configured hostname and route support it. Add TCP coverage only where the named tunnel configuration exposes a reachable TCP route. Keep protocol-specific origin tests independent from HTTP tests so an unavailable route produces a clear prerequisite failure.

## Feature-matrix coverage

Do not rely on `--all-features` as the only feature test because the current configuration gives quiche precedence over quinn when both are enabled.

Add explicit CI or script jobs for:

* default features with quinn and HTTP/2;
* quick tunnel with quinn only;
* quick tunnel with HTTP/2 only;
* quick tunnel with quiche;
* named tunnel with quinn;
* named tunnel with HTTP/2;
* named tunnel with quiche;
* no-default-features and supported single-feature builds.

Every live test target must have appropriate Cargo feature requirements so no-default-features builds do not attempt to compile unavailable APIs.

## Test execution policy

* Normal `cargo test` remains offline, deterministic, and credential-free.
* Live tests remain `#[ignore]` and are run only by `scripts/live-test.sh`, an explicit developer command, or a scheduled CI job.
* The runner selects tests only when their credentials and feature prerequisites are present.
* Live tests use generous DNS, edge startup, request, and shutdown deadlines.
* A failed live test must include the last observed status and response without including credentials.
* The runner must clean up spawned tasks even when an assertion fails.

## Migration sequence

1. Move existing live state to `tests/state/` and keep the directory ignored.
2. Restore RPC wire and exchange integration targets.
3. Extract shared integration support without exposing test-only APIs in the public crate.
4. Refactor current inline tests into the unit and protocol categories.
5. Add quick-tunnel state loading, creation, validation, and live HTTP tests.
6. Add named-tunnel token normalization, file-loader coverage, remote-configuration handling, and live HTTP tests.
7. Add the transport and backend matrix.
8. Add WebSocket and TCP live coverage where the configured routes support it.
9. Add CI commands, feature requirements, secret checks, and live-test documentation.
10. Remove obsolete tests and helpers only after replacement coverage passes.

## Acceptance criteria

The refactor is complete when:

* normal workspace tests run without network access or credentials;
* RPC tests cover the complete registration lifecycle and wire limits;
* public API tests run as external consumers;
* quick live tests reuse cached state and create at most one replacement tunnel after stale-state detection;
* named live tests are selected only with an explicit token and normalized state;
* QUIC, HTTP/2, quinn, and quiche feature paths are explicitly compiled and tested;
* live HTTP tests pass against the real edge for each supported tunnel and transport combination;
* no credential file is tracked, logged, or uploaded;
* the required formatting, check, clippy, and test commands pass.

## Validation commands

```text
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --workspace --no-default-features
scripts/live-test.sh
```
