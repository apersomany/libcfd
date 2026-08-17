# libcfd

A Rust library port of the Cloudflare Tunnel client. Consumers create tunnels, connect to the edge, and serve origin traffic through their own handlers. Not a CLI or daemon, and it imposes no async runtime on consumers.

The `research/cloudflared/` checkout is the behavioral and protocol reference. Treat it as read-only and never copy its source into this workspace; implement behavior independently in Rust.

# Constraints

- Keep the public API async-runtime agnostic; never expose concrete executor types (Tokio, async-std, ...).
- Every public future must be `Send`.
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

For every Rust code change, run:

```text
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Narrower tests are fine during iteration but do not replace the final checks. Documentation-only changes do not require Rust validation. If an environmental or upstream issue prevents a check, report the exact command, failure, and remaining risk.
