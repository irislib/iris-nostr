#!/bin/sh
set -eu
[ "$#" -eq 1 ] || { echo 'Usage: build-release.sh TARGET' >&2; exit 2; }
: "${CARGO_HOME:=${HOME:?}/.cargo}"
# Keep source and dependency checkout paths out of distributed executables.
CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$(pwd -P)=/src/iris-nostr$(printf '\037')--remap-path-prefix=$CARGO_HOME=/cargo"
export CARGO_ENCODED_RUSTFLAGS
exec cargo build --release --locked --bin iris-nostr --target "$1" -j 2
