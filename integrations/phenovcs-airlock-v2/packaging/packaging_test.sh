#!/bin/sh
# Dependency-free packaging smoke test. It verifies wrapper argument routing
# without touching the user's .airlock directory.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/airlock-v2-packaging.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/airlock-v2" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >"${AIRLOCK_V2_TEST_OUTPUT:?}"
EOF
chmod 0755 "$tmp/airlock-v2"
cp "$root/airlock-v2-autocommit.sh" "$root/airlock-v2-cleanup.sh" "$tmp/"
chmod 0755 "$tmp/airlock-v2-autocommit.sh" "$tmp/airlock-v2-cleanup.sh"

AIRLOCK_V2_TEST_OUTPUT="$tmp/out" "$tmp/airlock-v2-autocommit.sh" --dry-run
[ "$(cat "$tmp/out")" = "autocommit --dry-run" ]
AIRLOCK_V2_TEST_OUTPUT="$tmp/out" "$tmp/airlock-v2-cleanup.sh" --dry-run
[ "$(cat "$tmp/out")" = "cleanup --dry-run" ]

echo "packaging smoke passed"
