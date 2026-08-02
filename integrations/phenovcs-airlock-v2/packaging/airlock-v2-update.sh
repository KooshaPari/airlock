#!/bin/sh
# Update is the same deterministic build/stage operation as install.
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "$script_dir/airlock-v2-install.sh" "$@"
