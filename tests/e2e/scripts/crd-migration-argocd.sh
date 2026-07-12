#!/usr/bin/env bash
#
# End-to-end test for the CRD plural migration using Argo CD.
#
# This script:
#   1. Creates a Kind cluster with full prerequisites (cloud-provider-kind, ingress-nginx, gateway-api)
#   2. Installs Argo CD
#   3. Creates a local bare git repo serving as the chart source
#   4. Pushes the LEGACY chart to the local repo and creates an Argo CD Application
#   5. Creates person resources under the legacy CRD
#   6. Runs Rust pre-migration e2e tests that record Kanidm UUIDs and Kubernetes UIDs
#   7. Pushes the CURRENT chart (with migration hooks) to the same repo
#   8. Triggers a full sync and verifies the migration
#   9. Runs Rust post-migration e2e tests that verify Kanidm UUID preservation
#   10. Records post-migration UIDs, runs second sync, runs idempotent-rerun tests
#
# Environment variables:
#   LEGACY_CHART_VERSION  - legacy chart version (default: 0.10.2)
#   ARGOCD_VERSION        - Argo CD version to install (default: v2.14.6)
#   KIND_CLUSTER_NAME     - Kind cluster name (default: chart-testing-argocd)
#   SKIP_KIND_CREATE      - skip Kind creation (default: false)
#   CLEANUP_ON_EXIT       - cleanup on exit (default: true)
#   E2E_LOGGING_LEVEL     - log filter for operator (default: info,kaniop=debug)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

LEGACY_CHART_VERSION="${LEGACY_CHART_VERSION:-0.10.2}"
ARGOCD_VERSION="${ARGOCD_VERSION:-v2.14.6}"
KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME:-chart-testing-argocd}"
KIND_IMAGE_TAG="${KIND_IMAGE_TAG:-v1.34.3}"
KANIOP_NAMESPACE="${KANIOP_NAMESPACE:-kaniop}"
SKIP_KIND_CREATE="${SKIP_KIND_CREATE:-false}"
CLEANUP_ON_EXIT="${CLEANUP_ON_EXIT:-true}"
E2E_LOGGING_LEVEL="${E2E_LOGGING_LEVEL:-info,kaniop=debug,kaniop_webhook=debug}"
HELM_TIMEOUT="${HELM_TIMEOUT:-10m}"
KUBE_CONTEXT="kind-${KIND_CLUSTER_NAME}"
ARGOCD_NAMESPACE="${ARGOCD_NAMESPACE:-argocd}"
LEGACY_PLURAL="kanidmpersonsaccounts.kaniop.rs"
CORRECTED_PLURAL="kanidmpersonaccounts.kaniop.rs"
LOCAL_GIT_REPO="/tmp/kaniop-chart-repo.git"
GIT_DAEMON_CONTAINER="kaniop-git-daemon"
GIT_DAEMON_IMAGE="alpine:3.23"
GIT_REPO_URL=""
CHART_PATH_IN_REPO="charts/kaniop"
CLOUD_PROVIDER_KIND_IMAGE="registry.k8s.io/cloud-provider-kind/cloud-controller-manager:v0.8.0"
WEBHOOK_CERTGEN_IMAGE="registry.k8s.io/ingress-nginx/kube-webhook-certgen:v1.6.9"
INGRESS_NGINX_MANIFEST="https://kind.sigs.k8s.io/examples/ingress/deploy-ingress-nginx.yaml"
GATEWAY_API_MANIFEST="https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/standard-install.yaml"

export KANIDM_DEV_YOLO=1
export RUST_MIN_STACK=8388608
export MIGRATION_SOURCE_COUNT="${MIGRATION_SOURCE_COUNT:-3}"

log() { echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*"; }
fatal() { log "FATAL: $*" >&2; exit 1; }

check_prereqs() {
    for cmd in kind helm kubectl docker git jq openssl; do
        command -v "$cmd" >/dev/null 2>&1 || fatal "$cmd not found in PATH"
    done
}

setup_kind() {
    if [[ "${SKIP_KIND_CREATE}" == "true" ]]; then
        log "SKIP_KIND_CREATE=true, reusing existing cluster"
        kubectl config use-context "${KUBE_CONTEXT}" || fatal "context ${KUBE_CONTEXT} not found"
        return
    fi
    if kind get clusters 2>/dev/null | grep -q "^${KIND_CLUSTER_NAME}$"; then
        log "Deleting existing Kind cluster ${KIND_CLUSTER_NAME} for a clean migration test"
        kind delete cluster --name "${KIND_CLUSTER_NAME}"
    fi
    log "Creating Kind cluster ${KIND_CLUSTER_NAME}"
    kind create cluster \
        --name "${KIND_CLUSTER_NAME}" \
        --image "kindest/node:${KIND_IMAGE_TAG}" \
        --config "${REPO_ROOT}/.github/kind-cluster.yaml"
    kubectl cluster-info --context "${KUBE_CONTEXT}" >/dev/null
}

setup_kind_prerequisites() {
    log "Setting up Kind prerequisites (cloud-provider-kind, ingress-nginx, gateway-api)"

    if ! docker ps --format '{{.Names}}' | grep -q '^cloud-provider-kind$'; then
        log "Starting cloud-provider-kind"
        docker run -d --name cloud-provider-kind --network kind \
            -v /var/run/docker.sock:/var/run/docker.sock \
            --restart=always \
            "${CLOUD_PROVIDER_KIND_IMAGE}"
    else
        log "cloud-provider-kind already running"
    fi

    if ! kubectl get deploy -n ingress-nginx ingress-nginx-controller >/dev/null 2>&1; then
        log "Installing ingress-nginx"
        kubectl apply -f "${INGRESS_NGINX_MANIFEST}"
        kubectl -n ingress-nginx rollout status deployment/ingress-nginx-controller \
            --timeout=120s
    else
        log "ingress-nginx already installed"
    fi

    if ! kubectl get crd httproutes.gateway.networking.k8s.io >/dev/null 2>&1; then
        log "Installing gateway-api"
        kubectl apply -f "${GATEWAY_API_MANIFEST}"
        kubectl wait --for=condition=established crd/httproutes.gateway.networking.k8s.io --timeout=60s
    else
        log "gateway-api already installed"
    fi
}

setup_local_git_repo() {
    log "Setting up local bare git repo for Argo CD chart source"
    rm -rf "${LOCAL_GIT_REPO}" /tmp/kaniop-chart-work
    git init --bare --initial-branch=master "${LOCAL_GIT_REPO}" >/dev/null

    mkdir -p /tmp/kaniop-chart-work
    (cd /tmp/kaniop-chart-work && git init --initial-branch=master >/dev/null && \
        git config user.email "test@kaniop.rs" && \
        git config user.name "kaniop-test")

    helm pull oci://ghcr.io/pando85/helm-charts/kaniop \
        --version "${LEGACY_CHART_VERSION}" \
        --destination /tmp/kaniop-chart-work/

    mkdir -p /tmp/kaniop-chart-work/${CHART_PATH_IN_REPO}
    (cd /tmp/kaniop-chart-work && \
        tar xzf "kaniop-${LEGACY_CHART_VERSION}.tgz" -C ${CHART_PATH_IN_REPO} --strip-components=1 && \
        rm "kaniop-${LEGACY_CHART_VERSION}.tgz" && \
        git add -A && \
        git commit -m "legacy chart v${LEGACY_CHART_VERSION}" >/dev/null && \
        git remote add origin "${LOCAL_GIT_REPO}" && \
        git push origin master >/dev/null 2>&1)

    log "Legacy chart pushed to local git repo"
}

push_current_chart_to_repo() {
    local version
    version="$(cd "${REPO_ROOT}" && git rev-parse --short HEAD)"

    log "Pushing current chart to local git repo (version=${version})"

    (cd /tmp/kaniop-chart-work && \
        git fetch origin && \
        git reset --hard origin/master)

    rm -rf /tmp/kaniop-chart-work/${CHART_PATH_IN_REPO}/*
    cp -r "${REPO_ROOT}/charts/kaniop/"* /tmp/kaniop-chart-work/${CHART_PATH_IN_REPO}/

    (cd /tmp/kaniop-chart-work && \
        git add -A && \
        git commit -m "current chart v${version}" >/dev/null && \
        git push origin HEAD:master >/dev/null 2>&1)
}

install_argocd() {
    log "Installing Argo CD ${ARGOCD_VERSION}"
    kubectl create namespace "${ARGOCD_NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f -
    kubectl apply -n "${ARGOCD_NAMESPACE}" \
        -f "https://raw.githubusercontent.com/argoproj/argo-cd/${ARGOCD_VERSION}/manifests/install.yaml"

    log "Waiting for Argo CD deployments"
    kubectl -n "${ARGOCD_NAMESPACE}" rollout status deploy/argocd-server --timeout=300s
    kubectl -n "${ARGOCD_NAMESPACE}" rollout status deploy/argocd-repo-server --timeout=300s
    kubectl -n "${ARGOCD_NAMESPACE}" rollout status statefulset/argocd-application-controller --timeout=300s
}

start_git_daemon() {
    log "Starting git daemon for local repo access from Kind"
    docker rm -f "${GIT_DAEMON_CONTAINER}" >/dev/null 2>&1 || true
    docker run -d --name "${GIT_DAEMON_CONTAINER}" --network kind \
        -p 9418:9418 \
        -v "$(dirname "${LOCAL_GIT_REPO}"):/git:ro" \
        "${GIT_DAEMON_IMAGE}" sh -c \
        'timeout 120s apk add --no-cache git-daemon >/dev/null && git config --global --add safe.directory "*" && exec git daemon --reuseaddr --base-path=/git --export-all --verbose'

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
    local gateway_ip
    gateway_ip=$(docker network inspect kind \
        | jq -r 'first(.[0].IPAM.Config[] | select(.Gateway != null and (.Gateway | contains("."))) | .Gateway)')
    GIT_REPO_URL="git://${gateway_ip}:9418/kaniop-chart-repo.git"
    log "git daemon started at ${GIT_REPO_URL}"
}

create_kaniop_application_legacy() {
    local git_url="$1"
    local app_name="${2:-kaniop}"

    log "Creating Argo CD Application ${app_name} -> ${git_url}@legacy"
    kubectl apply -f - <<EOF
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: ${app_name}
  namespace: ${ARGOCD_NAMESPACE}
spec:
  project: default
  source:
    repoURL: "${git_url}"
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
  destination:
    server: https://kubernetes.default.svc
    namespace: ${KANIOP_NAMESPACE}
  syncPolicy:
    automated:
      prune: true
      selfHeal: false
    syncOptions:
      - CreateNamespace=true
      - PruneLast=true
      - SkipDryRunOnMissingResource=true
      - ServerSideApply=true
EOF
}

create_kaniop_application_current() {
    local git_url="$1"
    local version="$2"
    local app_name="${3:-kaniop}"

    log "Updating Argo CD Application ${app_name} -> ${git_url}@current"
    kubectl apply -f - <<EOF
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: ${app_name}
  namespace: ${ARGOCD_NAMESPACE}
spec:
  project: default
  source:
    repoURL: "${git_url}"
    path: ${CHART_PATH_IN_REPO}
    targetRevision: master
    helm:
      parameters:
        - name: image.tag
          value: "${version}"
        - name: env[0].name
          value: KANIDM_DEV_YOLO
        - name: env[0].value
          value: "1"
          forceString: true
        - name: logging.level
          value: "${E2E_LOGGING_LEVEL}"
        - name: webhook.enabled
          value: "true"
        - name: webhook.image.tag
          value: "${version}"
        - name: webhook.logging.level
          value: "${E2E_LOGGING_LEVEL}"
        - name: webhook.patch.createSecretJob.activeDeadlineSeconds
          value: "120"
        - name: webhook.patch.webhookJob.activeDeadlineSeconds
          value: "120"
  destination:
    server: https://kubernetes.default.svc
    namespace: ${KANIOP_NAMESPACE}
  syncPolicy:
    automated:
      prune: true
      selfHeal: false
    syncOptions:
      - CreateNamespace=true
      - PruneLast=true
      - SkipDryRunOnMissingResource=true
      - ServerSideApply=true
EOF
}

wait_for_application_synced() {
    local app_name="$1"
    local timeout_secs="${2:-600}"
    local expected_revision="${3:-}"
    local iterations=0
    while [[ $iterations -lt $timeout_secs ]]; do
        local health status revision operation_phase
        health=$(kubectl -n "${ARGOCD_NAMESPACE}" get application "${app_name}" \
            -o jsonpath='{.status.health.status}' 2>/dev/null || echo "Unknown")
        status=$(kubectl -n "${ARGOCD_NAMESPACE}" get application "${app_name}" \
            -o jsonpath='{.status.sync.status}' 2>/dev/null || echo "Unknown")
        revision=$(kubectl -n "${ARGOCD_NAMESPACE}" get application "${app_name}" \
            -o jsonpath='{.status.sync.revision}' 2>/dev/null || echo "")
        operation_phase=$(kubectl -n "${ARGOCD_NAMESPACE}" get application "${app_name}" \
            -o jsonpath='{.status.operationState.phase}' 2>/dev/null || echo "Unknown")
        if [[ "${health}" == "Healthy" && "${status}" == "Synced" \
            && ( -z "${expected_revision}" \
                || ( "${revision}" == "${expected_revision}" && "${operation_phase}" == "Succeeded" ) ) ]]; then
            log "Application ${app_name} is Healthy and Synced at revision ${revision}"
            return
        fi
        sleep 3
        iterations=$((iterations + 3))
    done
    log "Application status:"
    kubectl -n "${ARGOCD_NAMESPACE}" get application "${app_name}" -o yaml || true
    fatal "Application ${app_name} did not become Healthy+Synced within ${timeout_secs}s"
}

setup_legacy_sync() {
    log "Syncing legacy chart via Argo CD"
    for image in ghcr.io/pando85/kaniop ghcr.io/pando85/kaniop-webhook; do
        docker pull "${image}:${LEGACY_CHART_VERSION}"
        kind load --name "${KIND_CLUSTER_NAME}" docker-image "${image}:${LEGACY_CHART_VERSION}"
    done
    kubectl create namespace "${KANIOP_NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f -

    create_kaniop_application_legacy "${GIT_REPO_URL}"
    wait_for_application_synced "kaniop" 600
}

verify_legacy_crd() {
    log "Verifying legacy CRD is present"
    kubectl wait --for=condition=established \
        "crd/${LEGACY_PLURAL}" --timeout=120s \
        || fatal "Legacy CRD did not become established"
}

setup_kanidm_and_persons() {
    log "Creating Kanidm cluster and person resources"
    local tls_dir
    tls_dir=$(mktemp -d)
    openssl req -x509 -nodes -newkey rsa:2048 \
        -keyout "${tls_dir}/tls.key" -out "${tls_dir}/tls.crt" \
        -days 1 -subj "/CN=test-migration.localhost" \
        -addext "subjectAltName=DNS:test-migration.localhost" >/dev/null 2>&1
    kubectl -n default create secret tls test-migration-tls \
        --cert="${tls_dir}/tls.crt" --key="${tls_dir}/tls.key"
    rm -rf "${tls_dir}"
    kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: Kanidm
metadata:
  name: test-migration
  namespace: default
spec:
  domain: test-migration.localhost
  replicaGroups:
    - name: default
      replicas: 1
  ingress:
    annotations:
      nginx.ingress.kubernetes.io/backend-protocol: "HTTPS"
EOF
    kubectl -n default wait --for=condition=Available kanidm/test-migration --timeout=300s
    kubectl -n default wait --for=condition=Initialized kanidm/test-migration --timeout=180s

    kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: test-migration-person-alpha
  namespace: default
  annotations:
    argocd.argoproj.io/tracking-id: "default:KanidmPersonAccount:test-migration-person-alpha"
spec:
  kanidmRef:
    name: test-migration
  personAttributes:
    displayname: "ArgoCD Migration Alpha"
    mail:
      - argocd-alpha@example.com
---
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: test-migration-person-labeled
  namespace: default
  labels:
    migration-test: "true"
    team: platform
    argocd-instance: kaniop
  annotations:
    migration-test-annotation: "preserved"
    argocd.argoproj.io/tracking-id: "default:KanidmPersonAccount:test-migration-person-labeled"
spec:
  kanidmRef:
    name: test-migration
  personAttributes:
    displayname: "ArgoCD Migration Labeled"
    mail:
      - argocd-labeled@example.com
---
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: test-migration-person-argocd
  namespace: default
  annotations:
    argocd.argoproj.io/tracking-id: "default:KanidmPersonAccount:test-migration-person-argocd"
spec:
  kanidmRef:
    name: test-migration
  personAttributes:
    displayname: "ArgoCD Migration Tracking"
    mail:
      - argocd-tracking@example.com
EOF

    for name in test-migration-person-alpha test-migration-person-labeled test-migration-person-argocd; do
        local iterations=0
        while [[ $iterations -lt 90 ]]; do
            local ready
            ready=$(kubectl -n default get "${LEGACY_PLURAL}" "${name}" \
                -o jsonpath='{.status.ready}' 2>/dev/null || echo "false")
            if [[ "${ready}" == "true" ]]; then
                log "Person ${name} is Ready"
                break
            fi
            sleep 2
            iterations=$((iterations + 1))
        done
    done
}

run_pre_migration_e2e() {
    log "Running pre-migration Rust e2e tests (record Kanidm UUIDs and Kubernetes UIDs)"
    export MIGRATION_STAGE="pre-migration"
    export MIGRATION_EXPECTED_PHASE="N/A"
    (cd "${REPO_ROOT}" && \
        RUST_TEST_THREADS=1 cargo test \
        --target=x86_64-unknown-linux-gnu \
        -p kaniop-e2e-tests --features e2e-test --no-fail-fast \
        crd_migration_record_pre_migration_identity)
}

trigger_migration_sync() {
    log "Triggering migration sync via Argo CD"

    local version
    version="$(cd "${REPO_ROOT}" && git rev-parse --short HEAD)"

    log "Building and loading current images"
    (cd "${REPO_ROOT}" && make images)
    kind load --name "${KIND_CLUSTER_NAME}" docker-image "ghcr.io/pando85/kaniop:${version}"
    kind load --name "${KIND_CLUSTER_NAME}" docker-image "ghcr.io/pando85/kaniop-webhook:${version}"
    docker pull "${WEBHOOK_CERTGEN_IMAGE}"
    kind load --name "${KIND_CLUSTER_NAME}" docker-image "${WEBHOOK_CERTGEN_IMAGE}"

    (cd "${REPO_ROOT}" && make crdgen)

    push_current_chart_to_repo
    local expected_revision
    expected_revision=$(git -C /tmp/kaniop-chart-work rev-parse HEAD)

    create_kaniop_application_current "${GIT_REPO_URL}" "${version}"

    log "Triggering full sync"
    kubectl -n "${ARGOCD_NAMESPACE}" patch application kaniop --type merge \
        -p "{\"operation\":{\"sync\":{\"revision\":\"${expected_revision}\",\"syncStrategy\":{\"hook\":{}}}}}"

    log "Waiting for migration sync to complete (up to 600s)"
    wait_for_application_synced "kaniop" 600 "${expected_revision}"
}

verify_post_migration() {
    log "Verifying post-migration state"

    if kubectl get "crd/${LEGACY_PLURAL}" >/dev/null 2>&1; then
        fatal "Legacy CRD still exists after Argo CD migration sync"
    fi

    kubectl wait --for=condition=established \
        "crd/${CORRECTED_PLURAL}" --timeout=120s

    kubectl -n "${KANIOP_NAMESPACE}" rollout status deploy/kaniop --timeout=180s

    local person_count
    person_count=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o json 2>/dev/null \
        | jq '[.items[] | select(.metadata.name | startswith("test-migration-person-"))] | length')

    if [[ "${person_count}" -lt 2 ]]; then
        fatal "Expected at least 2 migrated persons, found ${person_count}"
    fi
    log "Found ${person_count} migrated person(s) after Argo CD sync"
}

verify_argocd_tracking_survived() {
    log "Verifying Argo CD tracking annotations survived migration"

    for name in test-migration-person-alpha test-migration-person-labeled test-migration-person-argocd; do
        local tracking
        tracking=$(kubectl -n default get "${CORRECTED_PLURAL}" "${name}" \
            -o jsonpath='{.metadata.annotations.argocd\.argoproj\.io/tracking-id}' 2>/dev/null || echo "")
        if [[ -z "${tracking}" ]]; then
            fatal "Argo CD tracking annotation missing from ${name} after migration"
        fi
        log "Argo CD tracking preserved for ${name}: ${tracking}"
    done
}

run_post_migration_e2e() {
    log "Running post-migration Rust e2e tests after Argo CD sync"
    export MIGRATION_STAGE="post-migration"
    export MIGRATION_EXPECTED_PHASE="Completed"
    (cd "${REPO_ROOT}" && \
        RUST_TEST_THREADS=1 cargo test \
        --target=x86_64-unknown-linux-gnu \
        -p kaniop-e2e-tests --features e2e-test --no-fail-fast \
        crd_migration -- \
        --skip crd_migration_record_pre_migration_identity \
        --skip crd_migration_idempotent_rerun)
}

record_post_migration_uids_and_run_idempotency() {
    log "Recording post-migration UIDs before second sync"
    local names
    names=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
        | grep '^test-migration-person-' || true)

    for name in ${names}; do
        local uid
        uid=$(kubectl -n default get "${CORRECTED_PLURAL}" "${name}" \
            -o jsonpath='{.metadata.uid}')
        if [[ -n "${uid}" ]]; then
            kubectl -n default annotate "${CORRECTED_PLURAL}" "${name}" \
                "kaniop.rs/post-migration-uid=${uid}" --overwrite
            log "Recorded post-migration UID for ${name}: ${uid}"
        fi
    done

    log "Triggering second sync (idempotency)"
    kubectl -n "${ARGOCD_NAMESPACE}" patch application kaniop --type merge \
        -p '{"operation": {"sync": {"syncStrategy": {"hook": {}}}}}'
    wait_for_application_synced "kaniop" 600

    log "Running idempotent-rerun Rust e2e tests"
    export MIGRATION_STAGE="idempotent-rerun"
    export MIGRATION_EXPECTED_PHASE="Completed"
    (cd "${REPO_ROOT}" && \
        RUST_TEST_THREADS=1 cargo test \
        --target=x86_64-unknown-linux-gnu \
        -p kaniop-e2e-tests --features e2e-test --no-fail-fast \
        crd_migration_idempotent_rerun)
}

test_selective_sync_unsupported() {
    log "Documenting that selective sync does NOT run migration hooks (unsupported)"
    log "Argo CD hooks do not execute during resource-filtered sync operations."
    log "This is a documented limitation, not a test failure."
}

cleanup() {
    if [[ "${CLEANUP_ON_EXIT}" != "true" ]]; then
        log "CLEANUP_ON_EXIT=false, keeping cluster"
        return
    fi
    log "Cleaning up Argo CD migration test resources"

    docker rm -f "${GIT_DAEMON_CONTAINER}" >/dev/null 2>&1 || true
    rm -rf "${LOCAL_GIT_REPO}" /tmp/kaniop-chart-work

    local names
    names=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
        | grep '^test-migration-person-' || true)

    for name in ${names}; do
        kubectl -n default delete "${CORRECTED_PLURAL}" "${name}" \
            --ignore-not-found=true --timeout=30s 2>/dev/null || true
    done

    kubectl -n default delete kanidm/test-migration \
        --ignore-not-found=true --timeout=30s 2>/dev/null || true
    kubectl -n default delete secret/test-migration-tls \
        --ignore-not-found=true --timeout=30s 2>/dev/null || true
}

main() {
    check_prereqs
    trap cleanup EXIT

    setup_kind
    setup_kind_prerequisites
    setup_local_git_repo
    start_git_daemon
    install_argocd
    setup_legacy_sync
    verify_legacy_crd
    setup_kanidm_and_persons
    run_pre_migration_e2e
    trigger_migration_sync
    verify_post_migration
    verify_argocd_tracking_survived
    run_post_migration_e2e
    record_post_migration_uids_and_run_idempotency
    test_selective_sync_unsupported

    log "SUCCESS: CRD migration Argo CD e2e test passed"
}

main "$@"
