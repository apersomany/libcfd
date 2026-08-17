#!/usr/bin/env bash
# Verifies credential hygiene: every live-test state file must be gitignored
# and no source file may print the token or a tunnel secret.
#
# Used by CI to guarantee PLAN.md's "no credential file is tracked, logged,
# or uploaded" acceptance criterion.

set -euo pipefail

cd "$(dirname "$0")/.."

failed=0

# Every file under tests/state is either a secret or contains secrets.
for entry in tests/state tests/state/*; do
    if [[ -e "$entry" ]] && ! git check-ignore -q "$entry"; then
        echo "error: $entry contains credentials but is not gitignored" >&2
        failed=1
    fi
done

# No logging or printing macro may emit a secret or token as a value.
# String literals (e.g. error messages that say "credentials") are stripped
# first, so only real field/argument usage is flagged.
find src rpc tests scripts -type f \( -name "*.rs" -o -name "*.sh" \) -print0 | while IFS= read -r -d '' file; do
    while IFS= read -r line; do
        case "$line" in
            *"tracing::"* | *"println!"* | *"print!"* | *"eprintln!"*)
                stripped=$(printf '%s' "$line" | sed 's/"[^"]*"//g')
                if printf '%s' "$stripped" | grep -qE '\b(secret|token)\b'; then
                    echo "error: $file logs a secret or token value: $line" >&2
                    failed=1
                fi
                ;;
        esac
    done < "$file"
done

if [[ "$failed" != "0" ]]; then
    exit 1
fi
echo "secret hygiene ok"
