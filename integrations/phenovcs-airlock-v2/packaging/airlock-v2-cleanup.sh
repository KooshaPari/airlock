#!/bin/sh
# Managed single-shot cleanup entrypoint for launchd/systemd.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "$script_dir/airlock-v2" cleanup "$@"
