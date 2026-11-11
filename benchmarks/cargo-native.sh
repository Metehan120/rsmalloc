#!/bin/sh
set -eu

RUSTFLAGS='-C target-cpu=native -C link-arg=-Wl,-z,now -C force-unwind-tables=no' \
    cargo "$@"
