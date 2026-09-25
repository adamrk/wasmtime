#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

cargo build \
    --manifest-path "$SCRIPT_DIR/middleware/Cargo.toml" \
    --target wasm32-wasip2 \
    --release

echo "$SCRIPT_DIR/middleware/target/wasm32-wasip2/release/wasmtime_samply_middleware.wasm"
