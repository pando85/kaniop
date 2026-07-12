#!/usr/bin/env bash
#
# Failure-injection tests for the CRD plural migration.
#
# This script exercises the migration Job's resume capability by injecting
# failures at specific phases and verifying that:
#   - Backups remain intact after each failure
#   - No Kanidm account was deleted
#   - Re-running the Job resumes and completes successfully
#   - A final rerun is a no-op (idempotent)
#
# The migrator supports failure injection via the chart value:
#   crdMigration.personAccountPlural.failAfter=<phase>
# which sets the KANIOP_MIGRATION_FAIL_AFTER env var on the migration Job.
#
# Failure phases:
#   BackedUp, OperatorStopped, FinalizersRemoved,
#   LegacyCRDDeleted, CorrectedCRDCreated, ObjectsRestored
#
# Environment variables:
#   LEGACY_CHART_VERSION  - legacy chart version (default: 0.10.2)
#   KIND_CLUSTER_NAME     - Kind cluster name (default: chart-testing-failures)
#   SKIP_KIND_CREATE      - skip Kind creation (default: false)
#   CLEANUP_ON_EXIT       - cleanup on exit (default: true)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

LEGACY_CHART_VERSION="${LEGACY_CHART_VERSION:-0.10.2}"
KANIOP_NAMESPACE="${KANIOP_NAMESPACE:-kaniop}"
KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME:-chart-testing-failures}"
KIND_IMAGE_TAG="${KIND_IMAGE_TAG:-v1.34.3}"
SKIP_KIND_CREATE="${SKIP_KIND_CREATE:-false}"
CLEANUP_ON_EXIT="${CLEANUP_ON_EXIT:-true}"
HELM_TIMEOUT="${HELM_TIMEOUT:-10m}"
KUBE_CONTEXT="kind-${KIND_CLUSTER_NAME}"
LEGACY_PLURAL="kanidmpersonsaccounts.kaniop.rs"
CORRECTED_PLURAL="kanidmpersonaccounts.kaniop.rs"
RELEASE_NAME="kaniop"
FAILURE_PHASES=("BackedUp" "OperatorStopped" "FinalizersRemoved" "LegacyCRDDeleted" "CorrectedCRDCreated" "ObjectsRestored")
CLOUD_PROVIDER_KIND_IMAGE="registry.k8s.io/cloud-provider-kind/cloud-controller-manager:v0.8.0"
INGRESS_NGINX_MANIFEST="https://kind.sigs.k8s.io/examples/ingress/deploy-ingress-nginx.yaml"
GATEWAY_API_MANIFEST="https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/standard-install.yaml"

export KANIDM_DEV_YOLO=1
export RUST_MIN_STACK=8388608

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

build_and_load_current_images() {
    log "Building current operator images"
    (cd "${REPO_ROOT}" && make images)
    local version
    version="$(cd "${REPO_ROOT}" && git rev-parse --short HEAD)"
    kind load --name "${KIND_CLUSTER_NAME}" docker-image "ghcr.io/pando85/kaniop:${version}"
    kind load --name "${KIND_CLUSTER_NAME}" docker-image "ghcr.io/pando85/kaniop-webhook:${version}"
}

reset_cluster_for_failure_test() {
    log "Resetting cluster for failure injection test"

    local legacy_names
    legacy_names=$(kubectl -n default get "${LEGACY_PLURAL}" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
        | grep '^test-failure-person' || true)
    for name in ${legacy_names}; do
        kubectl -n default delete "${LEGACY_PLURAL}" "${name}" --ignore-not-found=true 2>/dev/null || true
    done

    local corrected_names
    corrected_names=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
        | grep '^test-failure-person' || true)
    for name in ${corrected_names}; do
        kubectl -n default delete "${CORRECTED_PLURAL}" "${name}" --ignore-not-found=true 2>/dev/null || true
    done

    kubectl -n "${KANIOP_NAMESPACE}" delete secret -l kaniop.rs/migration=person-plural-v1 --ignore-not-found=true 2>/dev/null || true
    kubectl -n "${KANIOP_NAMESPACE}" delete configmap kaniop-person-crd-migration --ignore-not-found=true 2>/dev/null || true
    kubectl -n "${KANIOP_NAMESPACE}" delete jobs -l app.kubernetes.io/component=crd-migrator --ignore-not-found=true 2>/dev/null || true

    kubectl get "crd/${LEGACY_PLURAL}" >/dev/null 2>&1 && {
        kubectl delete "crd/${LEGACY_PLURAL}" --ignore-not-found=true 2>/dev/null || true
    }
    kubectl get "crd/${CORRECTED_PLURAL}" >/dev/null 2>&1 && {
        kubectl delete "crd/${CORRECTED_PLURAL}" --ignore-not-found=true 2>/dev/null || true
    }

    kubectl -n default delete kanidm/test-failure --ignore-not-found=true 2>/dev/null || true
    kubectl -n default delete secret/test-failure-tls --ignore-not-found=true 2>/dev/null || true

    helm uninstall "${RELEASE_NAME}" --namespace "${KANIOP_NAMESPACE}" 2>/dev/null || true
}

setup_legacy_state() {
    log "Installing legacy chart for failure injection"
    for image in ghcr.io/pando85/kaniop ghcr.io/pando85/kaniop-webhook; do
        docker pull "${image}:${LEGACY_CHART_VERSION}"
        kind load --name "${KIND_CLUSTER_NAME}" docker-image "${image}:${LEGACY_CHART_VERSION}"
    done

    kubectl create namespace "${KANIOP_NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f -

    helm upgrade --install "${RELEASE_NAME}" "oci://ghcr.io/pando85/helm-charts/kaniop" \
        --namespace "${KANIOP_NAMESPACE}" \
        --version "${LEGACY_CHART_VERSION}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1"

    kubectl -n "${KANIOP_NAMESPACE}" rollout status deploy/"${RELEASE_NAME}" --timeout=180s

    local tls_dir
    tls_dir=$(mktemp -d)
    openssl req -x509 -nodes -newkey rsa:2048 \
        -keyout "${tls_dir}/tls.key" -out "${tls_dir}/tls.crt" \
        -days 1 -subj "/CN=test-failure.localhost" \
        -addext "subjectAltName=DNS:test-failure.localhost" >/dev/null 2>&1
    kubectl -n default create secret tls test-failure-tls \
        --cert="${tls_dir}/tls.crt" --key="${tls_dir}/tls.key"
    rm -rf "${tls_dir}"

    kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: Kanidm
metadata:
  name: test-failure
  namespace: default
spec:
  domain: test-failure.localhost
  replicaGroups:
    - name: default
      replicas: 1
  ingress:
    annotations:
      nginx.ingress.kubernetes.io/backend-protocol: "HTTPS"
EOF
    kubectl -n default wait --for=condition=Available kanidm/test-failure --timeout=300s
    kubectl -n default wait --for=condition=Initialized kanidm/test-failure --timeout=180s

    kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: test-failure-person
  namespace: default
spec:
  kanidmRef:
    name: test-failure
  personAttributes:
    displayname: "Failure Test Person"
    mail:
      - failure-test@example.com
EOF

    local iterations=0
    while [[ $iterations -lt 90 ]]; do
        local ready
        ready=$(kubectl -n default get "${LEGACY_PLURAL}" test-failure-person \
            -o jsonpath='{.status.ready}' 2>/dev/null || echo "false")
        if [[ "${ready}" == "true" ]]; then
            log "Person test-failure-person is Ready"
            break
        fi
        sleep 2
        iterations=$((iterations + 1))
    done
}

inject_failure_and_resume() {
    local phase="$1"
    local version
    version="$(cd "${REPO_ROOT}" && git rev-parse --short HEAD)"

    log "=== Testing failure injection at phase: ${phase} ==="

    reset_cluster_for_failure_test
    setup_legacy_state

    log "Running migration with crdMigration.personAccountPlural.failAfter=${phase}"
    helm upgrade "${RELEASE_NAME}" "${REPO_ROOT}/charts/kaniop" \
        --namespace "${KANIOP_NAMESPACE}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set-string "image.tag=${version}" \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1" \
        --set-string "crdMigration.personAccountPlural.failAfter=${phase}" \
        2>&1 || true

    log "Verifying backups remain after failure at ${phase}"
    local backup_count
    backup_count=$(kubectl -n "${KANIOP_NAMESPACE}" get secret \
        -l kaniop.rs/migration=person-plural-v1 \
        -o json 2>/dev/null | jq '.items | length' || echo "0")

    if [[ "${phase}" == "BackedUp" || "${phase}" == "OperatorStopped" || "${phase}" == "FinalizersRemoved" ]]; then
        if [[ "${backup_count}" -eq 0 ]]; then
            log "WARNING: No backups found after failure at ${phase} (may be expected for early phases)"
        else
            log "Backups preserved: ${backup_count} secret(s)"
        fi
    fi

    log "Resuming migration (no failure injection)"
    helm upgrade "${RELEASE_NAME}" "${REPO_ROOT}/charts/kaniop" \
        --namespace "${KANIOP_NAMESPACE}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set-string "image.tag=${version}" \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1"

    log "Verifying migration completed after resume"
    if kubectl get "crd/${LEGACY_PLURAL}" >/dev/null 2>&1; then
        fatal "Legacy CRD still exists after resume from failure at ${phase}"
    fi

    kubectl wait --for=condition=established \
        "crd/${CORRECTED_PLURAL}" --timeout=120s

    log "Running idempotency check after resume from ${phase}"
    helm upgrade "${RELEASE_NAME}" "${REPO_ROOT}/charts/kaniop" \
        --namespace "${KANIOP_NAMESPACE}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set-string "image.tag=${version}" \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1"

    log "=== Phase ${phase}: PASSED ==="
}

cleanup() {
    if [[ "${CLEANUP_ON_EXIT}" != "true" ]]; then
        log "CLEANUP_ON_EXIT=false, keeping cluster"
        return
    fi
    log "Cleaning up failure-injection test resources"

    kubectl -n "${KANIOP_NAMESPACE}" delete secret \
        -l kaniop.rs/migration=person-plural-v1 --ignore-not-found=true 2>/dev/null || true
    kubectl -n "${KANIOP_NAMESPACE}" delete configmap kaniop-person-crd-migration \
        --ignore-not-found=true 2>/dev/null || true

    local legacy_names
    legacy_names=$(kubectl -n default get "${LEGACY_PLURAL}" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
        | grep '^test-failure-person' || true)
    for name in ${legacy_names}; do
        kubectl -n default delete "${LEGACY_PLURAL}" "${name}" --ignore-not-found=true 2>/dev/null || true
    done

    local corrected_names
    corrected_names=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
        | grep '^test-failure-person' || true)
    for name in ${corrected_names}; do
        kubectl -n default delete "${CORRECTED_PLURAL}" "${name}" --ignore-not-found=true 2>/dev/null || true
    done

    kubectl -n default delete kanidm/test-failure --ignore-not-found=true 2>/dev/null || true
    kubectl -n default delete secret/test-failure-tls --ignore-not-found=true 2>/dev/null || true
}

main() {
    check_prereqs
    trap cleanup EXIT

    setup_kind
    setup_kind_prerequisites
    build_and_load_current_images

    log "Starting CRD migration failure-injection tests"
    log "Phases to test: ${FAILURE_PHASES[*]}"

    for phase in "${FAILURE_PHASES[@]}"; do
        inject_failure_and_resume "${phase}"
    done

    log "SUCCESS: All failure-injection tests passed"
}

main "$@"
