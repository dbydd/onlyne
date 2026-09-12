#!/usr/bin/env bash
# Load .onlyne.example/spec.toml through the shipped parser.
#
# `.onlyne.example/spec.toml` is the tree's only full-coverage spec file: it carries every key in
# docs/v1-PLAN.md §5 lines 220-268, and an operator copies it as the starting point for a real
# <server-root>/.onlyne/spec.toml. onlyne-config parses with deny_unknown_fields
# (crates/onlyne-config/src/spec.rs:29 and :42), so one key outside the closed set refuses the
# start. That failure lands inside every e2e script that copies the example, where it reads as a
# server, client, or routing problem and sends the reader to the wrong file.
#
# Usage: sh packaging/check-example.sh [path]
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
path=${1:-$repo/.onlyne.example/spec.toml}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target/packaging}
cd "$repo"
cargo run -q -p onlyne-config --bin check-spec -- "$path"
echo "check-example: $path loaded by onlyne-config"
