FROM debian:trixie-20240904-slim
LABEL maintainer=pando855@gmail.com

ARG CARGO_TARGET_DIR=target
ARG CARGO_BUILD_TARGET=
ARG CARGO_RELEASE_PROFILE=release

COPY ${CARGO_TARGET_DIR}/${CARGO_BUILD_TARGET}/${CARGO_RELEASE_PROFILE}/kaniop /bin/kaniop

ENTRYPOINT ["/bin/kaniop"]
