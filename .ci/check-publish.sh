#!/usr/bin/env bash
set -euo pipefail

mapfile -t crates < <(
    cargo metadata --format-version 1 --no-deps \
        | jq -r '.packages[] | select(.publish == null) | .name'
)

for crate in "${crates[@]}"; do
    echo "==> Checking package: ${crate}"
    cargo package --locked --allow-dirty --no-verify -p "${crate}"
done
