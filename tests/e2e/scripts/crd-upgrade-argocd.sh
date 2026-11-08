#!/usr/bin/env bash
#
# End-to-end test for the CRD upgrade path with an Argo CD sync.
#
# Argo CD runs the chart's `crdApply` Helm hook as a PreSync hook. This script:
#   1. Creates a Kind cluster and installs Argo CD
#   2. Serves a local git repo with the previous published chart and syncs it with Argo CD
#   3. Deploys a Kanidm cluster and person resources
#   4. Makes every Kaniop CRD stale with the hook's field manager
#   5. Pushes the current local chart (crdApply enabled) and triggers a full sync
#   6. Verifies every live CRD matches charts/kaniop/crds/crds.yaml and existing and new
#      resources reconcile
#   7. Makes the CRDs stale again, syncs again, and verifies that the crdApply PreSync hook
#      applied the current CRDs
#
# See crd-upgrade-common.sh for common environment variables. Additionally:
#   ARGOCD_VERSION     - Argo CD version to install (default: v2.14.6)
#   ARGOCD_NAMESPACE   - Argo CD namespace (default: argocd)
#   KIND_CLUSTER_NAME  - Kind cluster name (default: kaniop-crd-upgrade-argocd)
#
set -euo pipefail

KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME:-kaniop-crd-upgrade-argocd}"

# shellcheck source=tests/e2e/scripts/crd-upgrade-common.sh disable=SC1091
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/crd-upgrade-common.sh"

ARGOCD_VERSION="${ARGOCD_VERSION:-v2.14.6}"
ARGOCD_NAMESPACE="${ARGOCD_NAMESPACE:-argocd}"
APP_NAME="kaniop"
GIT_REPO_DIR="${WORK_DIR}/git"
GIT_REPO_NAME="kaniop-chart-repo.git"
GIT_WORK_TREE="${WORK_DIR}/chart-work"
GIT_DAEMON_CONTAINER="${KIND_CLUSTER_NAME}-git-daemon"
GIT_DAEMON_IMAGE="alpine:3.23"
GIT_REPO_URL=""
CHART_PATH_IN_REPO="charts/kaniop"

setup_local_git_repo() {
    log "Setting up local git repo with chart ${PREVIOUS_CHART_VERSION}"
    mkdir -p "${GIT_REPO_DIR}" "${GIT_WORK_TREE}/${CHART_PATH_IN_REPO}"
    git init -q --bare --initial-branch=master "${GIT_REPO_DIR}/${GIT_REPO_NAME}"
    git -C "${GIT_WORK_TREE}" init -q --initial-branch=master
    git -C "${GIT_WORK_TREE}" config user.email "test@kaniop.rs"
    git -C "${GIT_WORK_TREE}" config user.name "kaniop-test"
    git -C "${GIT_WORK_TREE}" remote add origin "${GIT_REPO_DIR}/${GIT_REPO_NAME}"

    helm pull "${CHART_REF}" --version "${PREVIOUS_CHART_VERSION}" --destination "${WORK_DIR}"
    tar xzf "${WORK_DIR}/kaniop-${PREVIOUS_CHART_VERSION}.tgz" \
        -C "${GIT_WORK_TREE}/${CHART_PATH_IN_REPO}" --strip-components=1
    commit_and_push_chart "previous chart ${PREVIOUS_CHART_VERSION}"
}

commit_and_push_chart() {
    git -C "${GIT_WORK_TREE}" add -A
    git -C "${GIT_WORK_TREE}" commit -q --allow-empty -m "$1"
    git -C "${GIT_WORK_TREE}" push -q origin HEAD:master
}

push_current_chart() {
    log "Pushing the current chart to the local git repo"
    rm -rf "${GIT_WORK_TREE:?}/${CHART_PATH_IN_REPO}"
    mkdir -p "${GIT_WORK_TREE}/${CHART_PATH_IN_REPO}"
    cp -r "${REPO_ROOT}/charts/kaniop/." "${GIT_WORK_TREE}/${CHART_PATH_IN_REPO}/"
    commit_and_push_chart "current chart $(current_version)"
}

start_git_daemon() {
    log "Starting git daemon on the kind network"
    docker rm -f "${GIT_DAEMON_CONTAINER}" >/dev/null 2>&1 || true
    docker run -d --name "${GIT_DAEMON_CONTAINER}" --network kind \
        -v "${GIT_REPO_DIR}:/git:ro" \
        "${GIT_DAEMON_IMAGE}" sh -c \
        'timeout 120s apk add --no-cache git-daemon >/dev/null && git config --global --add safe.directory "*" && exec git daemon --reuseaddr --base-path=/git --export-all --verbose' \
        >/dev/null

    local _
    for _ in {1..150}; do
        if docker logs "${GIT_DAEMON_CONTAINER}" 2>&1 | grep -q 'Ready to rumble'; then
            break
        fi
        if [[ "$(docker inspect -f '{{.State.Running}}' "${GIT_DAEMON_CONTAINER}" 2>/dev/null)" != "true" ]]; then
            docker logs "${GIT_DAEMON_CONTAINER}" 2>&1 || true
            fatal "git daemon exited during startup"
        fi
        sleep 1
    done
    if ! docker logs "${GIT_DAEMON_CONTAINER}" 2>&1 | grep -q 'Ready to rumble'; then
        docker logs "${GIT_DAEMON_CONTAINER}" 2>&1 || true
        fatal "git daemon did not become ready"
    fi
    local daemon_ip
    daemon_ip=$(docker inspect -f '{{(index .NetworkSettings.Networks "kind").IPAddress}}' \
        "${GIT_DAEMON_CONTAINER}")
    [[ -n "${daemon_ip}" ]] || fatal "could not resolve the git daemon IP on the kind network"
    GIT_REPO_URL="git://${daemon_ip}:9418/${GIT_REPO_NAME}"
    log "git daemon serving ${GIT_REPO_URL}"
}

install_argocd() {
    log "Installing Argo CD ${ARGOCD_VERSION}"
    kubectl create namespace "${ARGOCD_NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f -
    # kubectl fetches a remote manifest without retrying, so one transient CDN error while
    # reading the manifest from raw.githubusercontent.com aborts the whole run. Download it with
    # retries first and apply the local file.
    local manifest="${WORK_DIR}/argocd-install.yaml"
    local attempt
    for attempt in 1 2 3; do
        [[ "${attempt}" -eq 1 ]] || sleep $((attempt * 20))
        log "Attempt ${attempt} to download the Argo CD install manifest"
        if curl -fsSL --connect-timeout 10 --max-time 300 \
            --retry 3 --retry-delay 10 --retry-all-errors --retry-max-time 90 \
            -o "${manifest}" "https://raw.githubusercontent.com/argoproj/argo-cd/${ARGOCD_VERSION}/manifests/install.yaml"; then
            break
        fi
        if [[ "${attempt}" -eq 3 ]]; then
            fatal "could not download the Argo CD ${ARGOCD_VERSION} install manifest"
        fi
    done
    [[ -s "${manifest}" ]] || fatal "the Argo CD ${ARGOCD_VERSION} install manifest is empty"
    kubectl apply -n "${ARGOCD_NAMESPACE}" --server-side --force-conflicts -f "${manifest}"
    kubectl -n "${ARGOCD_NAMESPACE}" rollout status deploy/argocd-server --timeout=300s
    kubectl -n "${ARGOCD_NAMESPACE}" rollout status deploy/argocd-repo-server --timeout=300s
    kubectl -n "${ARGOCD_NAMESPACE}" rollout status statefulset/argocd-application-controller --timeout=300s
}

# Usage: apply_application [image-version]
# Without an image version, the chart's default image tags are used.
apply_application() {
    local version="${1:-}"
    local image_params=""
    if [[ -n "${version}" ]]; then
        image_params=$(cat <<EOF
        - name: image.tag
          value: "${version}"
          forceString: true
        - name: webhook.enabled
          value: "true"
        - name: webhook.image.tag
          value: "${version}"
          forceString: true
        - name: webhook.logging.level
          value: "${E2E_LOGGING_LEVEL}"
        - name: crdApply.enabled
          value: "true"
EOF
)
    fi
    log "Applying Argo CD Application ${APP_NAME} (image version: ${version:-chart default})"
    kubectl apply -f - <<EOF
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: ${APP_NAME}
  namespace: ${ARGOCD_NAMESPACE}
spec:
  project: default
  source:
    repoURL: "${GIT_REPO_URL}"
    path: ${CHART_PATH_IN_REPO}
    targetRevision: master
    helm:
      parameters:
        - name: env[0].name
          value: KANIDM_DEV_YOLO
        - name: env[0].value
          value: "1"
          forceString: true
        - name: logging.level
          value: "${E2E_LOGGING_LEVEL}"
        - name: webhook.patch.createSecretJob.activeDeadlineSeconds
          value: "120"
        - name: webhook.patch.webhookJob.activeDeadlineSeconds
          value: "120"
${image_params}
  destination:
    server: https://kubernetes.default.svc
    namespace: ${KANIOP_NAMESPACE}
  # No automated sync: an automated sync triggered by the Application change can race the
  # explicit sync and render stale manifests, so the test triggers every sync itself.
  syncPolicy:
    syncOptions:
      - CreateNamespace=true
      - PruneLast=true
      - SkipDryRunOnMissingResource=true
      - ServerSideApply=true
EOF
}

# Usage: wait_for_application_synced <timeout-seconds> [expected-revision] [started-after]
# started-after is an RFC 3339 timestamp; the sync operation must have started at or after it.
wait_for_application_synced() {
    local timeout_secs="$1"
    local expected_revision="${2:-}"
    local started_after="${3:-}"
    local iterations=0
    while [[ $iterations -lt $timeout_secs ]]; do
        local app health status revision phase started
        app=$(kubectl -n "${ARGOCD_NAMESPACE}" get application "${APP_NAME}" -o json 2>/dev/null || echo '{}')
        health=$(jq -r '.status.health.status // "Unknown"' <<<"${app}")
        status=$(jq -r '.status.sync.status // "Unknown"' <<<"${app}")
        revision=$(jq -r '.status.sync.revision // ""' <<<"${app}")
        phase=$(jq -r '.status.operationState.phase // "Unknown"' <<<"${app}")
        started=$(jq -r '.status.operationState.startedAt // ""' <<<"${app}")
        if [[ "${health}" == "Healthy" && "${status}" == "Synced" && "${phase}" == "Succeeded" \
            && ( -z "${expected_revision}" || "${revision}" == "${expected_revision}" ) \
            && ( -z "${started_after}" || ! "${started}" < "${started_after}" ) ]]; then
            log "Application ${APP_NAME} is Healthy and Synced at revision ${revision}"
            return
        fi
        if [[ ( "${phase}" == "Failed" || "${phase}" == "Error" ) \
            && ( -z "${started_after}" || ! "${started}" < "${started_after}" ) ]]; then
            kubectl -n "${ARGOCD_NAMESPACE}" get application "${APP_NAME}" -o yaml || true
            fatal "Argo CD sync operation ${phase}: $(jq -r '.status.operationState.message // ""' <<<"${app}")"
        fi
        sleep 3
        iterations=$((iterations + 3))
    done
    kubectl -n "${ARGOCD_NAMESPACE}" get application "${APP_NAME}" -o yaml || true
    fatal "Application ${APP_NAME} did not become Healthy+Synced within ${timeout_secs}s"
}

sync_previous_chart() {
    log "Syncing the previous chart with Argo CD"
    preload_published_images
    apply_application
    trigger_full_sync "$(git -C "${GIT_WORK_TREE}" rev-parse HEAD)"
    kubectl -n "${KANIOP_NAMESPACE}" rollout status "deploy/${RELEASE_NAME}" --timeout=180s
}

# Usage: trigger_full_sync <revision>
trigger_full_sync() {
    local revision="$1"
    local started_after
    started_after=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
    log "Triggering a full sync (hooks enabled) at revision ${revision}"
    # A manual operation does not inherit spec.syncPolicy.syncOptions, so pass them explicitly.
    local operation
    operation=$(kubectl -n "${ARGOCD_NAMESPACE}" get application "${APP_NAME}" -o json \
        | jq -c --arg rev "${revision}" '{operation: {sync: {
            revision: $rev,
            prune: true,
            syncOptions: (.spec.syncPolicy.syncOptions // []),
            syncStrategy: {hook: {}}}}}')
    kubectl -n "${ARGOCD_NAMESPACE}" patch application "${APP_NAME}" --type merge -p "${operation}"
    wait_for_application_synced 600 "${revision}" "${started_after}"
}

sync_current_chart() {
    push_current_chart
    apply_application "$(current_version)"
    trigger_full_sync "$(git -C "${GIT_WORK_TREE}" rev-parse HEAD)"
}

cleanup() {
    local exit_code="$1"
    if [[ "${exit_code}" -ne 0 ]]; then
        kubectl -n "${ARGOCD_NAMESPACE}" get application "${APP_NAME}" -o yaml 2>/dev/null || true
    fi
    if [[ "${CLEANUP_ON_EXIT}" == "true" ]]; then
        docker rm -f "${GIT_DAEMON_CONTAINER}" >/dev/null 2>&1 || true
    fi
    cleanup_kind "${exit_code}"
}

main() {
    check_prereqs kind helm kubectl docker git jq openssl curl
    trap 'cleanup $?' EXIT

    resolve_previous_chart_version
    setup_kind
    build_and_load_current_images
    generate_crds
    setup_local_git_repo
    start_git_daemon
    install_argocd
    sync_previous_chart
    setup_kanidm_and_persons

    make_crds_stale
    sync_current_chart
    # Argo CD also server-side applies the chart's crds/ directory itself. On the first sync
    # after the hook manifests change (as they do on every upgrade), Argo CD v2.14 can report
    # the crdApply PreSync Job as succeeded while the new Job is still failing, so only require
    # current CRDs here; the next sync must run the hook.
    verify_crds_current false
    verify_resources_after_upgrade "${RELEASE_NAME}" sync

    log "Triggering a second full sync (the PreSync hook must apply CRDs)"
    make_crds_stale
    trigger_full_sync "$(git -C "${GIT_WORK_TREE}" rev-parse HEAD)"
    verify_crds_current
    verify_resources_after_upgrade "${RELEASE_NAME}" resync

    log "SUCCESS: Argo CD CRD upgrade e2e test passed (from ${PREVIOUS_CHART_VERSION})"
}

main "$@"
