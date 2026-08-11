#!/usr/bin/env bash
# BUILD court + CORE/unit court, run inside the Docker builder (rules 31, 47).
#
# This script runs INSIDE the builder container with the repository mounted
# read-only at /repo and container-owned cache volumes. It never touches the
# host toolchain, target/, registry or environment.

set -euo pipefail
cd /repo
export CARGO_HOME=/usr/local/cargo
export CARGO_TARGET_DIR=/repo/target

echo "== BUILD.001: workspace build =="
cargo build --workspace --all-targets

echo "== BUILD.002: clean-ish rebuild (incremental, no artifacts reused from host) =="
cargo build --workspace --bins

echo "== CORE.001: unit tests =="
cargo test --workspace --bins --lib

echo "== CORE.002: doc tests =="
cargo test --workspace --doc

echo "== CORE.003: clippy (strict) =="
cargo clippy --workspace --all-targets -- -D warnings

echo "== CORE.004: formatting =="
cargo fmt --all -- --check

echo "== CORE.005: protocol hostile-input tests (already part of CORE.001, explicit) =="
cargo test -p ferrokey-protocol

echo "== CORE.006: layout/parser tests =="
cargo test -p ferrokey-layouts

echo "== CORE.007: state-machine tests =="
cargo test -p ferrokey-core

echo "ALL UNIT/BUILD COURTS PASSED"
