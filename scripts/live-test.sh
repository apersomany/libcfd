#!/usr/bin/env bash
# Live-edge test runner for libcfd.
#
# Ensures quick tunnel credentials exist (creating and saving them on first
# run), then runs the end-to-end live test against the real Cloudflare edge.
# Reuses saved credentials across runs to keep API requests low.
set -euo pipefail
cd "$(dirname "$0")/.."

CREDS="$PWD/.test-creds/quick-tunnel.json"

run_test() {
  nix develop -c cargo test --test live_edge "$1" -- --ignored --nocapture
}

if [[ ! -f "$CREDS" ]]; then
  echo "No saved credentials at $CREDS; creating a quick tunnel via libcfd..."
  run_test create_and_save_quick_tunnel
else
  echo "Reusing saved quick tunnel credentials from $CREDS."
fi

echo "Running live end-to-end test against the real Cloudflare edge..."
run_test live_quick_tunnel_over_quic_serves_http
