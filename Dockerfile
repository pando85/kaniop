FROM debian:trixie-20240904-slim AS base
LABEL maintainer=pando855@gmail.com

ARG CARGO_TARGET_DIR=target
ARG CARGO_BUILD_TARGET=
ARG CARGO_RELEASE_PROFILE=release

FROM base AS kaniop

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY ${CARGO_TARGET_DIR}/${CARGO_BUILD_TARGET}/${CARGO_RELEASE_PROFILE}/kaniop /bin/kaniop
ENTRYPOINT ["/bin/kaniop"]

FROM base AS kaniop-webhook

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY ${CARGO_TARGET_DIR}/${CARGO_BUILD_TARGET}/${CARGO_RELEASE_PROFILE}/kaniop-webhook /bin/kaniop-webhook
ENTRYPOINT ["/bin/kaniop-webhook"]
