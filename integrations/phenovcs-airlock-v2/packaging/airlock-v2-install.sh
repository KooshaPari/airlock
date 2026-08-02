#!/bin/sh
# Build and stage the Rust v2 cycle binary and wrappers into the container's
# managed .airlock/bin directory. This script deliberately does not load or
# restart launchd/systemd units; activation is an explicit operator action.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
crate_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
repos_root=${AIRLOCK_V2_REPOS_ROOT:-$(CDPATH= cd -- "$crate_dir/../../.." && pwd)}
bin_dir=${AIRLOCK_V2_BIN_DIR:-$repos_root/.airlock/bin}
manifest="$crate_dir/Cargo.toml"
build_bin="$crate_dir/target/release/airlock-v2"

if [ -e "$bin_dir/airlock-v2" ] && [ -L "$bin_dir/airlock-v2" ] && [ "${AIRLOCK_V2_ALLOW_REPLACE_SYMLINK:-0}" != 1 ]; then
    echo "refusing to replace symlink $bin_dir/airlock-v2; set AIRLOCK_V2_ALLOW_REPLACE_SYMLINK=1 after review" >&2
    exit 2
fi

mkdir -p "$bin_dir"
cargo build --release --manifest-path "$manifest"

install -m 0755 "$build_bin" "$bin_dir/airlock-v2"
install -m 0755 "$script_dir/airlock-v2-autocommit.sh" "$bin_dir/airlock-v2-autocommit.sh"
install -m 0755 "$script_dir/airlock-v2-cleanup.sh" "$bin_dir/airlock-v2-cleanup.sh"

{
    printf '%s\n' "source=airlock/integrations/phenovcs-airlock-v2"
    printf 'version=%s\n' "$(git -C "$crate_dir" rev-parse HEAD 2>/dev/null || printf unknown)"
    printf 'installed_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >"$bin_dir/.airlock-v2-managed"

echo "staged airlock-v2 binary and cycle wrappers in $bin_dir"
echo "activation is explicit: review your launchd/systemd unit, then reload it"
