#!/usr/bin/env bash
# Runs the live-edge integration tests against the real Cloudflare edge.
#
# Selection rules (per PLAN.md):
#   - Quick-tunnel state is cached in tests/state/quick_tunnel.json and the
#     tests themselves create one only when no usable cached state exists.
#   - The named suite runs only when a connector token is available:
#     NAMED_TUNNEL_TOKEN, or the local tests/state/named-token.txt file.
#     Otherwise it is omitted loudly, never silently passed.
#   - The quiche backend matrix runs only when LIBCFD_LIVE_QUICHE=1 (it
#     builds BoringSSL via cmake/libclang).
#   - Live tests must run single-threaded because they reuse one tunnel
#     identity.
#
# Credentials are never printed; the state directory is gitignored.

set -euo pipefail

cd "$(dirname "$0")/.."

STATE_DIR="tests/state"
TOKEN_FILE="$STATE_DIR/named-token.txt"

# Named suite eligibility.
if [[ -n "${NAMED_TUNNEL_TOKEN:-}" || -f "$TOKEN_FILE" ]]; then
    if [[ -z "${NAMED_TUNNEL_TOKEN:-}" ]]; then
        export NAMED_TUNNEL_TOKEN="$(cat "$TOKEN_FILE")"
        echo "using NAMED_TUNNEL_TOKEN from $TOKEN_FILE"
    fi
    NAMED=yes
else
    echo "note: NAMED_TUNNEL_TOKEN is unset and $TOKEN_FILE is missing; omitting the named suite."
    NAMED=no
fi

# Runs `cargo test -p libcfd [cargo args] -- --ignored --test-threads=1`.
run_live() {
    local description="$1"
    shift
    echo
    echo "== $description =="
    cargo test -p libcfd "$@" -- --ignored --test-threads=1
}

# 1. Default features: quinn QUIC and HTTP/2 transports.
run_live "quick tunnel: quinn QUIC + HTTP/2 (default features)" --test live_quick
if [[ "$NAMED" == "yes" ]]; then
    run_live "named tunnel: quinn QUIC + HTTP/2 (default features)" --test live_named
fi

# 2. Quiche backend (when explicitly requested and the toolchain can build
#    BoringSSL). The named suite additionally needs quick-tunnel for the
#    shared HTTPS test client.
if [[ "${LIBCFD_LIVE_QUICHE:-0}" == "1" ]]; then
    run_live "quick tunnel: quiche" \
        --no-default-features --features quick-tunnel,quic-edge-quiche --test live_quick
    if [[ "$NAMED" == "yes" ]]; then
        run_live "named tunnel: quiche" \
            --no-default-features --features named-tunnel,quick-tunnel,quic-edge-quiche --test live_named
    fi
else
    echo
    echo "note: quiche live runs skipped (set LIBCFD_LIVE_QUICHE=1 to enable)."
fi

echo
echo "live suite finished"
