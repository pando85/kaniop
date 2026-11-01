#!/usr/bin/env bash
#
# End-to-end test for the CRD upgrade path with `helm upgrade`.
#
# Helm installs CRDs from crds/ on first install only and never updates them. The chart's
# `crdApply` pre-install/pre-upgrade hook server-side applies the current CRDs instead. This
# script:
#   1. Creates a Kind cluster
#   2. Installs the previous published chart from the OCI registry
#   3. Deploys a Kanidm cluster and person resources
#   4. Makes every Kaniop CRD stale with the hook's field manager
#   5. Runs `helm upgrade` to the current local chart (crdApply enabled)
#   6. Verifies every live CRD matches charts/kaniop/crds/crds.yaml and existing and new
#      resources reconcile
#   7. Makes the CRDs stale again, upgrades again, and repeats the checks
#
# See crd-upgrade-common.sh for environment variables. KIND_CLUSTER_NAME defaults to
# kaniop-crd-upgrade.
#
set -euo pipefail

KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME:-kaniop-crd-upgrade}"

# shellcheck source=tests/e2e/scripts/crd-upgrade-common.sh disable=SC1091
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/crd-upgrade-common.sh"

HELM_LOGGING_LEVEL="${E2E_LOGGING_LEVEL//,/\\,}"

install_previous_chart() {
    log "Installing previous chart ${PREVIOUS_CHART_VERSION}"
    preload_published_images
    helm upgrade --install "${RELEASE_NAME}" "${CHART_REF}" \
        --namespace "${KANIOP_NAMESPACE}" \
        --create-namespace \
        --version "${PREVIOUS_CHART_VERSION}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1" \
        --set "logging.level=${HELM_LOGGING_LEVEL}"
    kubectl -n "${KANIOP_NAMESPACE}" rollout status "deploy/${RELEASE_NAME}" --timeout=180s
}

run_helm_upgrade() {
    local version
    version="$(current_version)"
    log "Running helm upgrade to the current chart (version=${version})"
    helm upgrade "${RELEASE_NAME}" "${REPO_ROOT}/charts/kaniop" \
        --namespace "${KANIOP_NAMESPACE}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set-string "image.tag=${version}" \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1" \
        --set "logging.level=${HELM_LOGGING_LEVEL}" \
        --set "webhook.enabled=true" \
        --set-string "webhook.image.tag=${version}" \
        --set "webhook.logging.level=${HELM_LOGGING_LEVEL}" \
        --set "crdApply.enabled=true"
    log "helm upgrade completed"
}

main() {
    check_prereqs kind helm kubectl docker git jq openssl
    trap 'cleanup_kind $?' EXIT

    resolve_previous_chart_version
    setup_kind
    build_and_load_current_images
    generate_crds
    install_previous_chart
    setup_kanidm_and_persons

    make_crds_stale
    run_helm_upgrade
    verify_crds_current
    verify_resources_after_upgrade "${RELEASE_NAME}" upgrade

    log "Re-running helm upgrade (the hook must apply CRDs on every upgrade)"
    make_crds_stale
    run_helm_upgrade
    verify_crds_current
    verify_resources_after_upgrade "${RELEASE_NAME}" rerun

    log "SUCCESS: Helm CRD upgrade e2e test passed (from ${PREVIOUS_CHART_VERSION})"
}

main "$@"
