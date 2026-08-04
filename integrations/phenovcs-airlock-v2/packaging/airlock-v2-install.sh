#!/bin/sh
# Build and stage the Rust v2 cycle binary and wrappers into the container's
# managed .airlock/bin directory. Activation remains an explicit operator step.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
crate_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
repos_root=${AIRLOCK_V2_REPOS_ROOT:-$(CDPATH= cd -- "$crate_dir/../../.." && pwd)}
bin_dir=${AIRLOCK_V2_BIN_DIR:-$repos_root/.airlock/bin}
manifest="$crate_dir/Cargo.toml"
build_bin=${AIRLOCK_V2_BUILD_BIN:-$crate_dir/target/release/airlock-v2}
managed_names='airlock-v2 airlock-v2-autocommit.sh airlock-v2-cleanup.sh .airlock-v2-managed'
stage_dir=
backup_dir=
installed_names=
committed=0

sha256_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        echo "no SHA-256 tool available (need shasum or sha256sum)" >&2
        return 1
    fi
}

inode_file() {
    if stat -f %i "$1" >/dev/null 2>&1; then
        stat -f %i "$1"
    else
        stat -c %i "$1"
    fi
}

rollback() {
    status=$?
    if [ "$committed" -eq 0 ] && [ -n "$backup_dir" ] && [ -d "$backup_dir" ]; then
        for name in $installed_names; do
            rm -f "$bin_dir/$name"
        done
        for name in $managed_names; do
            if [ -e "$backup_dir/$name" ] || [ -L "$backup_dir/$name" ]; then
                mv "$backup_dir/$name" "$bin_dir/$name"
            fi
        done
        echo "Airlock v2 installation rolled back; previous files were restored" >&2
    fi
    if [ -n "$stage_dir" ] && [ -d "$stage_dir" ]; then
        rm -rf "$stage_dir"
    fi
    if [ "$committed" -eq 1 ] && [ -n "$backup_dir" ] && [ -d "$backup_dir" ]; then
        rm -rf "$backup_dir"
    fi
    exit "$status"
}
trap rollback EXIT HUP INT TERM

if [ -e "$bin_dir/airlock-v2" ] && [ -L "$bin_dir/airlock-v2" ] && [ "${AIRLOCK_V2_ALLOW_REPLACE_SYMLINK:-0}" != 1 ]; then
    echo "refusing to replace symlink $bin_dir/airlock-v2; set AIRLOCK_V2_ALLOW_REPLACE_SYMLINK=1 after review" >&2
    exit 2
fi

mkdir -p "$bin_dir"
if [ "${AIRLOCK_V2_SKIP_BUILD:-0}" != 1 ]; then
    cargo build --release --manifest-path "$manifest"
fi
if [ ! -f "$build_bin" ] || [ ! -x "$build_bin" ]; then
    echo "refusing to install missing or non-executable build artifact: $build_bin" >&2
    exit 2
fi

# Keep staging and backup on the destination filesystem so each mv is atomic
# and the staged inode recorded in the marker remains the installed inode.
stage_dir=$(mktemp -d "$bin_dir/.airlock-v2-stage.XXXXXX")
backup_dir=$(mktemp -d "$bin_dir/.airlock-v2-backup.XXXXXX")
install -m 0755 "$build_bin" "$stage_dir/airlock-v2"
install -m 0755 "$script_dir/airlock-v2-autocommit.sh" "$stage_dir/airlock-v2-autocommit.sh"
install -m 0755 "$script_dir/airlock-v2-cleanup.sh" "$stage_dir/airlock-v2-cleanup.sh"

{
    printf '%s\n' 'format=1'
    printf '%s\n' 'source=airlock/integrations/phenovcs-airlock-v2'
    printf 'version=%s\n' "$(git -C "$crate_dir" rev-parse HEAD 2>/dev/null || printf unknown)"
    printf 'installed_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    for name in airlock-v2 airlock-v2-autocommit.sh airlock-v2-cleanup.sh; do
        printf 'file.%s.sha256=%s\n' "$name" "$(sha256_file "$stage_dir/$name")"
        printf 'file.%s.inode=%s\n' "$name" "$(inode_file "$stage_dir/$name")"
    done
} >"$stage_dir/.airlock-v2-managed"

for name in $managed_names; do
    if [ -e "$bin_dir/$name" ] || [ -L "$bin_dir/$name" ]; then
        mv "$bin_dir/$name" "$backup_dir/$name"
    fi
done
for name in $managed_names; do
    mv "$stage_dir/$name" "$bin_dir/$name"
    installed_names="$installed_names $name"
done

committed=1
echo "atomically staged airlock-v2 binary and cycle wrappers in $bin_dir"
echo "activation is explicit: review source-controlled service declarations, then reload the supervisor"
