FROM debian:trixie-20240904-slim AS base
LABEL maintainer=pando855@gmail.com

ARG CARGO_TARGET_DIR=target
ARG CARGO_BUILD_TARGET=
ARG CARGO_RELEASE_PROFILE=release

FROM base AS kaniop

COPY ${CARGO_TARGET_DIR}/${CARGO_BUILD_TARGET}/${CARGO_RELEASE_PROFILE}/kaniop /bin/kaniop
ENTRYPOINT ["/bin/kaniop"]

FROM base AS kaniop-webhook

COPY ${CARGO_TARGET_DIR}/${CARGO_BUILD_TARGET}/${CARGO_RELEASE_PROFILE}/kaniop-webhook /bin/kaniop-webhook
ENTRYPOINT ["/bin/kaniop-webhook"]
