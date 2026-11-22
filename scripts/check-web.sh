#!/usr/bin/env bash
# Web checks, shared by local development and CI. Run after `pnpm install`.
set -euo pipefail

cd "$(dirname "$0")/.."

for task in typecheck test build; do
  pnpm run "$task"
done
