# libcfd

A Rust library port of the Cloudflare Tunnel client. Consumers create tunnels, connect to the edge, and serve origin traffic through their own handlers. Not a CLI or daemon. Its public API is runtime-neutral, but built-in network execution requires a consumer-provided Tokio runtime with I/O and time enabled; consumers own asynchronous origin scheduling.

The `research/cloudflared/` checkout is the behavioral and protocol reference. Treat it as read-only and never copy its source into this workspace; implement behavior independently in Rust.

# Constraints

- Keep the public API async-runtime agnostic; never expose concrete executor types (Tokio, async-std, ...).
- Every public future must be `Send`; the public RPC call boundary requires `Send` callbacks and decoded output.
- Shutdown signals retain ownership through bounded transport cleanup attempts. Cancellation/drop must close connections and abort library-owned tasks, not detach them; consumer-scheduled origin work remains consumer-owned. Do not claim an overall shutdown bound.
- Credential `Debug` must redact registration secrets; serialization and raw fields remain sensitive.
- Never use `capnp-rpc` (its futures are not `Send`). Only `libcfd-rpc` may depend on `capnp` crates.
- Avoid unnecessary payload copies; prefer borrowing, ownership transfer, or shared buffers.
- Use `tracing` for diagnostics; never initialize a global subscriber. Never log credentials, tunnel tokens, private keys, or request authorization data.
- Implement only the smallest surface needed for the task.
- Prefer safe Rust. If unsafe is unavoidable, isolate it and explain the invariant in a single concise comment.
- Use `thiserror` errors.
- Keep tests focused and minimal; test observable behavior, not implementation details.
- Use the Cargo CLI for dependency changes. Prefer the latest compatible release; minimize dependencies and features; remove unused ones before finishing a task.
- Do not modify generated files when the schema or generation step can be changed instead.

# Validation

Use `nix flake check -L` as the normal validation wrapper (CI runs the same command). It supplies the toolchain/native dependencies and runs secret hygiene, the four checks below, and a default-feature workspace check. For local iteration, use `nix develop` or `nix develop -c <command>`. Testing and opt-in live-test instructions are consolidated in [README.md](README.md).

For every Rust code change, run:

```text
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Narrower tests are fine during iteration but do not replace the final checks. Documentation-only changes do not require Rust validation. If an environmental or upstream issue prevents a check, report the exact command, failure, and remaining risk.
