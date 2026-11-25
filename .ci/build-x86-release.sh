#!/bin/bash
# Build and exercise the production x86 profile in the same portable userspace
# on PRs and when publishing images. No host libraries enter this build.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

cargo_home="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$cargo_home"
docker build --pull -f .ci/x86-release.Dockerfile -t kaniop-x86-release-builder .ci

docker run --rm \
    --user "$(id -u):$(id -g)" \
    -e HOME=/tmp -e CARGO_HOME=/cargo-cache \
    -v "$cargo_home:/cargo-cache" \
    -v "$PWD:/workspace" -w /workspace \
    kaniop-x86-release-builder \
    bash -euc '
        make CARGO_TARGET=x86_64-unknown-linux-gnu CARGO_TARGET_DIR=target-x86_64-unknown-linux-gnu CARGO_RELEASE_PROFILE=release release
        .ci/check-x86-64-baseline.sh target-x86_64-unknown-linux-gnu/x86_64-unknown-linux-gnu/release
    '
