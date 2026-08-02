#!/bin/sh
# Remove only files previously staged by airlock-v2-install.sh.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
crate_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
repos_root=${AIRLOCK_V2_REPOS_ROOT:-$(CDPATH= cd -- "$crate_dir/../../.." && pwd)}
bin_dir=${AIRLOCK_V2_BIN_DIR:-$repos_root/.airlock/bin}
marker="$bin_dir/.airlock-v2-managed"

if [ ! -f "$marker" ]; then
    echo "refusing to remove unmanaged Airlock files (missing $marker)" >&2
    exit 2
fi

rm -f \
    "$bin_dir/airlock-v2" \
    "$bin_dir/airlock-v2-autocommit.sh" \
    "$bin_dir/airlock-v2-cleanup.sh" \
    "$marker"
echo "removed Airlock v2 managed files from $bin_dir"
