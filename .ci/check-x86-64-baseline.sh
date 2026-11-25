#!/bin/bash
# Run every shipped x86_64 binary on an emulated baseline x86-64 CPU (no AVX, AVX2
# or BMI2) and fail if any of them dies with an illegal instruction.
#
# Release binaries must run on any x86-64 CPU. C dependencies (AWS-LC, vendored
# OpenSSL) inherit the build host compiler's default -march, so a build host with an
# x86-64-v3 default compiler ships binaries that crash with SIGILL on older CPUs.
#
# Usage: .ci/check-x86-64-baseline.sh <directory with the release binaries>
#
# Environment:
#   QEMU      qemu user-mode emulator (default: qemu-x86_64, then qemu-x86_64-static)
#   QEMU_CPU  emulated CPU model (default: Westmere, the last Intel model without AVX)
#   TIMEOUT   seconds allowed per binary (default: 120)
set -euo pipefail

BIN_DIR=${1:?usage: $0 <directory with kaniop, kaniop-crd-migrator, kaniop-webhook and kaniop-data-mover>}
QEMU_CPU=${QEMU_CPU:-Westmere}
TIMEOUT=${TIMEOUT:-120}

if [ -z "${QEMU:-}" ]; then
    QEMU=$(command -v qemu-x86_64 || command -v qemu-x86_64-static || true)
fi
for tool in "${QEMU:-qemu-x86_64}" openssl timeout; do
    if ! command -v "$tool" >/dev/null; then
        echo "error: $tool not found (install qemu-user and openssl)" >&2
        exit 2
    fi
done
for name in kaniop kaniop-crd-migrator kaniop-webhook kaniop-data-mover; do
    if [ ! -x "$BIN_DIR/$name" ]; then
        echo "error: $BIN_DIR/$name not found or not executable" >&2
        exit 2
    fi
done

# A crashing binary must not leave qemu core files behind.
ulimit -c 0

WORK_DIR=$(mktemp -d)
SERVER_PID=
cleanup() {
    if [ -n "$SERVER_PID" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

FAILED=0

# Run a binary under the emulated CPU. An ordinary exit status, success or error,
# passes unless an expected exit status is provided; signals and timeouts fail.
# An optional EXPECT_LOG pattern must appear to prove the code path ran.
check() {
    local name=$1
    local label="$name $2"
    shift
    local log="$WORK_DIR/$name.log"
    local rc=0
    timeout "$TIMEOUT" "$QEMU" -cpu "$QEMU_CPU" "$BIN_DIR/$name" "$@" >"$log" 2>&1 || rc=$?
    if [ "$rc" -eq 132 ] || grep -q 'uncaught target signal 4' "$log"; then
        echo "FAIL $label: SIGILL (exit $rc), uses instructions the $QEMU_CPU CPU lacks"
    elif [ "$rc" -eq 124 ]; then
        echo "FAIL $label: timed out after ${TIMEOUT}s"
    elif [ "$rc" -gt 128 ]; then
        echo "FAIL $label: killed by signal $((rc - 128))"
    elif [ -n "${EXPECT_EXIT:-}" ] && [ "$rc" -ne "$EXPECT_EXIT" ]; then
        echo "FAIL $label: exit $rc, expected $EXPECT_EXIT; the binary did not start successfully"
    elif [ -n "${EXPECT_LOG:-}" ] && ! grep -q "$EXPECT_LOG" "$log"; then
        echo "FAIL $label: exit $rc without '$EXPECT_LOG' in its output; the check did not run"
    else
        echo "ok   $label (exit $rc)"
        return 0
    fi
    sed 's/^/     | /' "$log" | tail -n 20
    FAILED=1
}

# The operator, webhook and CRD migrator initialise AWS-LC before main, so
# printing the version is enough to exercise its CPU dispatch.
EXPECT_EXIT=0 check kaniop --version
EXPECT_EXIT=0 check kaniop-crd-migrator --version
EXPECT_EXIT=0 check kaniop-webhook --version

# The data mover only reaches its TLS stack (vendored OpenSSL) on a request, so
# point a discover operation at a throwaway local TLS server. Its reply is an HTML
# status page, so the S3 XML parse error proves a full HTTPS round trip happened.
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=localhost \
    -addext subjectAltName=DNS:localhost \
    -keyout "$WORK_DIR/key.pem" -out "$WORK_DIR/cert.pem" 2>/dev/null
for _ in 1 2 3 4 5; do
    PORT=$((20000 + RANDOM % 40000))
    openssl s_server -quiet -www -accept "127.0.0.1:$PORT" \
        -cert "$WORK_DIR/cert.pem" -key "$WORK_DIR/key.pem" \
        >"$WORK_DIR/s_server.log" 2>&1 &
    SERVER_PID=$!
    sleep 1
    if kill -0 "$SERVER_PID" 2>/dev/null; then
        break
    fi
    SERVER_PID=
done
if [ -z "$SERVER_PID" ]; then
    echo "error: could not start a local TLS server" >&2
    cat "$WORK_DIR/s_server.log" >&2
    exit 2
fi

OPERATION=$(cat <<EOF
{"apiVersion": "backup.kaniop.rs/v1alpha1", "kind": "OperationDocument",
 "operation": "discover", "bucket": "baseline-check", "prefix": "kaniop",
 "endpoint": "https://localhost:$PORT", "region": "us-east-1", "forcePathStyle": true,
 "namespaceUid": "default", "kanidmUid": "baseline-check",
 "resultPath": "$WORK_DIR/discover-result.json", "maxRetries": 0}
EOF
)
SSL_CERT_FILE="$WORK_DIR/cert.pem" AWS_ACCESS_KEY_ID=baseline AWS_SECRET_ACCESS_KEY=baseline \
    EXPECT_LOG='serde xml' \
    check kaniop-data-mover discover --operation-doc "$OPERATION"

if [ "$FAILED" -ne 0 ]; then
    echo "x86-64 baseline check failed: verify compiler flags and build/runtime userspace"
    exit 1
fi
echo "x86-64 baseline check passed on $QEMU_CPU"
