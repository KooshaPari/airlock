#!/bin/sh
# Remove only artifacts whose exact hash and inode match this installer marker.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
crate_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
repos_root=${AIRLOCK_V2_REPOS_ROOT:-$(CDPATH= cd -- "$crate_dir/../../.." && pwd)}
bin_dir=${AIRLOCK_V2_BIN_DIR:-$repos_root/.airlock/bin}
marker="$bin_dir/.airlock-v2-managed"
managed_names='airlock-v2 airlock-v2-autocommit.sh airlock-v2-cleanup.sh'

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

marker_value() {
    awk -F= -v wanted="$1" '$1 == wanted { print substr($0, length(wanted) + 2); exit }' "$marker"
}

if [ ! -f "$marker" ]; then
    echo "refusing to remove unmanaged Airlock files (missing $marker)" >&2
    exit 2
fi
if [ "$(marker_value format)" != 1 ]; then
    echo "refusing to remove files with an unknown Airlock lifecycle marker format" >&2
    exit 2
fi

for name in $managed_names; do
    path="$bin_dir/$name"
    expected_hash=$(marker_value "file.$name.sha256")
    expected_inode=$(marker_value "file.$name.inode")
    if [ ! -f "$path" ] || [ -z "$expected_hash" ] || [ -z "$expected_inode" ]; then
        echo "refusing to remove incomplete managed artifact $path" >&2
        exit 2
    fi
    if [ "$(sha256_file "$path")" != "$expected_hash" ] || [ "$(inode_file "$path")" != "$expected_inode" ]; then
        echo "refusing to remove changed managed artifact $path; preserve it for recovery" >&2
        exit 2
    fi
done

for name in $managed_names; do
    rm -f "$bin_dir/$name"
done
rm -f "$marker"
echo "removed verified Airlock v2 managed files from $bin_dir"
