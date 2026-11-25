# Build against an older glibc than the Debian Trixie runtime and avoid the
# amd64v3 toolchain, startup objects and loader of Ubuntu 26.04 cloud hosts.
FROM rust:1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang cmake ninja-build openssl pkg-config qemu-user \
    && rm -rf /var/lib/apt/lists/*

ENV CFLAGS_x86_64_unknown_linux_gnu="-march=x86-64" \
    CXXFLAGS_x86_64_unknown_linux_gnu="-march=x86-64" \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C target-cpu=x86-64"
