#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

(
  cd "$root/web"
  npm ci
  npm run build
)

cd "$root"
cargo build --release
mkdir -p dist
cp target/release/openllm dist/openllm
printf '\nBuilt: %s/dist/openllm\n' "$root"

