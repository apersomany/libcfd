#!/usr/bin/env bash
# Compiles and tests every supported feature combination (PLAN.md
# feature-matrix coverage).
#
# The quiche backend matrix builds BoringSSL (needs cmake and libclang);
# those jobs are skipped with a note when the toolchain is unavailable.

set -euo pipefail

cd "$(dirname "$0")/.."

check() {
    local description="$1"
    shift
    echo "== $description =="
    cargo check --workspace --all-targets "$@"
}

# 1. Default features: quinn QUIC and HTTP/2 together.
check "default features (quinn + http/2)"

# 2. Quick tunnel with each transport alone.
check "quick tunnel: quinn only" --no-default-features --features quick-tunnel,quic-edge-quinn
check "quick tunnel: http/2 only" --no-default-features --features quick-tunnel,h2-edge

# 3. Named tunnel with each transport alone.
check "named tunnel: quinn only" --no-default-features --features named-tunnel,quic-edge-quinn
check "named tunnel: http/2 only" --no-default-features --features named-tunnel,h2-edge

# 4. Single transport features without any tunnel feature still build.
check "quic-edge alone" --no-default-features --features quic-edge
check "h2-edge alone" --no-default-features --features h2-edge
check "bare quick tunnel" --no-default-features --features quick-tunnel
check "bare named tunnel" --no-default-features --features named-tunnel
check "no default features" --no-default-features

# 5. Quiche backend (BoringSSL; needs cmake/libclang).
if command -v cmake >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
    check "quick tunnel: quiche" --no-default-features --features quick-tunnel,quic-edge-quiche
    check "named tunnel: quiche" --no-default-features --features named-tunnel,quic-edge-quiche
else
    echo "note: quiche matrix skipped (cmake/clang not found)"
fi

# 6. Full test runs for the two ends of the feature space.
echo
echo "== full test run (--all-features) =="
cargo test --workspace --all-features
echo
echo "== bare test run (--no-default-features) =="
cargo test --workspace --no-default-features

echo
echo "feature matrix finished"
