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

# Lifecycle smoke: exercise install/update/uninstall against a private bin
# directory using a prebuilt fixture. This never touches the live .airlock
# tree and does not require Cargo or a running supervisor.
fixture=$(mktemp -d "${TMPDIR:-/tmp}/airlock-v2-lifecycle.XXXXXX")
trap 'rm -rf "$tmp" "$fixture"' EXIT
mkdir -p "$fixture/artifact" "$fixture/bin"
cat >"$fixture/artifact/airlock-v2" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 0755 "$fixture/artifact/airlock-v2"

AIRLOCK_V2_BIN_DIR="$fixture/bin" \
AIRLOCK_V2_BUILD_BIN="$fixture/artifact/airlock-v2" \
AIRLOCK_V2_SKIP_BUILD=1 \
  "$root/airlock-v2-install.sh"

marker="$fixture/bin/.airlock-v2-managed"
[ -f "$marker" ]
grep '^format=1$' "$marker" >/dev/null
grep '^file\.airlock-v2\.sha256=[0-9a-f][0-9a-f]*$' "$marker" >/dev/null
grep '^file\.airlock-v2\.inode=[0-9][0-9]*$' "$marker" >/dev/null

# Ownership verification must reject a changed managed file and leave every
# managed artifact intact for recovery.
printf 'tampered\n' >>"$fixture/bin/airlock-v2"
if AIRLOCK_V2_BIN_DIR="$fixture/bin" "$root/airlock-v2-uninstall.sh"; then
    echo "uninstall unexpectedly accepted tampered binary" >&2
    exit 1
fi
[ -f "$fixture/bin/airlock-v2" ]
[ -f "$marker" ]

# A fresh install into a second directory proves the normal uninstall path.
fresh="$fixture/fresh"
mkdir -p "$fresh"
AIRLOCK_V2_BIN_DIR="$fresh" \
AIRLOCK_V2_BUILD_BIN="$fixture/artifact/airlock-v2" \
AIRLOCK_V2_SKIP_BUILD=1 \
  "$root/airlock-v2-install.sh"
AIRLOCK_V2_BIN_DIR="$fresh" "$root/airlock-v2-uninstall.sh"
[ ! -e "$fresh/airlock-v2" ]
[ ! -e "$fresh/airlock-v2-autocommit.sh" ]
[ ! -e "$fresh/airlock-v2-cleanup.sh" ]
[ ! -e "$fresh/.airlock-v2-managed" ]

echo "lifecycle ownership smoke passed"
