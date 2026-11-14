#!/usr/bin/env bash
#
# Load local docker images into every node of a kind cluster.
#
# `kind load docker-image` (kind v0.33+) imports with `ctr images import
# --all-platforms`, which fails when the image was pulled from a multi-arch
# manifest but only one platform is present locally. Importing the saved image
# directly into each node without that flag avoids the failure.
#
# Usage: kind-load-image.sh <cluster-name> <image> [image...]
set -euo pipefail

cluster="${1:?usage: $0 <cluster-name> <image> [image...]}"
shift

if [[ $# -eq 0 ]]; then
    echo "ERROR: no images specified" >&2
    exit 2
fi

nodes=$(kind get nodes --name "${cluster}")
if [[ -z "${nodes}" ]]; then
    echo "ERROR: no nodes found in kind cluster ${cluster}" >&2
    exit 1
fi

archive=$(mktemp)
trap 'rm -f "${archive}"' EXIT

for image in "$@"; do
    echo "Loading ${image} into kind cluster ${cluster}"
    docker save -o "${archive}" "${image}"
    while IFS= read -r node; do
        docker exec --privileged -i "${node}" \
            ctr --namespace=k8s.io images import --digests --snapshotter=overlayfs - < "${archive}"
    done <<< "${nodes}"
done
