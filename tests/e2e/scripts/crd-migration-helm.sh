#!/usr/bin/env bash
#
# End-to-end test for the CRD plural migration (kanidmpersonsaccounts -> kanidmpersonaccounts).
#
# This script:
#   1. Creates a Kind cluster with full prerequisites (cloud-provider-kind, ingress-nginx, gateway-api)
#   2. Installs the LEGACY chart (v0.10.2) from the OCI registry
#   3. Deploys a Kanidm cluster and creates person resources
#   4. Runs Rust pre-migration e2e tests that record Kanidm UUIDs and Kubernetes UIDs
#   5. Runs `helm upgrade` to the CURRENT local chart (with migration hooks)
#   6. Runs Rust post-migration e2e tests that verify Kanidm UUID preservation
#   7. Records post-migration UIDs for idempotency check
#   8. Runs second `helm upgrade` (idempotency)
#   9. Runs Rust idempotent-rerun e2e tests
#
# Environment variables:
#   LEGACY_CHART_VERSION  - legacy chart version to install (default: 0.10.2)
#   KANIOP_NAMESPACE      - namespace for the operator (default: kaniop)
#   KIND_CLUSTER_NAME     - Kind cluster name (default: chart-testing)
#   KIND_IMAGE_TAG        - Kind node image tag (default: from Makefile)
#   MIGRATION_SOURCE_COUNT - expected number of migrated persons (default: 3)
#   SKIP_KIND_CREATE      - set to "true" to skip Kind cluster creation
#   CLEANUP_ON_EXIT       - set to "false" to keep cluster on exit (default: true)
#   E2E_LOGGING_LEVEL     - log filter for operator (default: info,kaniop=debug)
#   HELM_TIMEOUT          - helm operation timeout (default: 10m)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

LEGACY_CHART_VERSION="${LEGACY_CHART_VERSION:-0.10.2}"
KANIOP_NAMESPACE="${KANIOP_NAMESPACE:-kaniop}"
KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME:-chart-testing}"
KIND_IMAGE_TAG="${KIND_IMAGE_TAG:-v1.34.3}"
MIGRATION_SOURCE_COUNT="${MIGRATION_SOURCE_COUNT:-3}"
SKIP_KIND_CREATE="${SKIP_KIND_CREATE:-false}"
CLEANUP_ON_EXIT="${CLEANUP_ON_EXIT:-true}"
E2E_LOGGING_LEVEL="${E2E_LOGGING_LEVEL:-info\\,kaniop=debug\\,kaniop_webhook=debug}"
HELM_TIMEOUT="${HELM_TIMEOUT:-10m}"
KUBE_CONTEXT="kind-${KIND_CLUSTER_NAME}"
LEGACY_CHART_REF="oci://ghcr.io/pando85/helm-charts/kaniop"
RELEASE_NAME="kaniop"
LEGACY_PLURAL="kanidmpersonsaccounts.kaniop.rs"
CORRECTED_PLURAL="kanidmpersonaccounts.kaniop.rs"
CLOUD_PROVIDER_KIND_IMAGE="registry.k8s.io/cloud-provider-kind/cloud-controller-manager:v0.8.0"
INGRESS_NGINX_MANIFEST="https://kind.sigs.k8s.io/examples/ingress/deploy-ingress-nginx.yaml"
GATEWAY_API_MANIFEST="https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/standard-install.yaml"

export KANIDM_DEV_YOLO=1
export RUST_MIN_STACK=8388608
export MIGRATION_SOURCE_COUNT

log() { echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*"; }
fatal() { log "FATAL: $*" >&2; exit 1; }

check_prereqs() {
    for cmd in kind helm kubectl docker cargo git jq openssl; do
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

preload_published_images() {
    local version="$1"
    for image in ghcr.io/pando85/kaniop ghcr.io/pando85/kaniop-webhook; do
        log "Pulling and loading ${image}:${version}"
        docker pull "${image}:${version}"
        kind load --name "${KIND_CLUSTER_NAME}" docker-image "${image}:${version}"
    done
}

install_legacy_chart() {
    log "Installing legacy chart v${LEGACY_CHART_VERSION}"
    preload_published_images "${LEGACY_CHART_VERSION}"
    kubectl create namespace "${KANIOP_NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f -

    helm upgrade --install "${RELEASE_NAME}" "${LEGACY_CHART_REF}" \
        --namespace "${KANIOP_NAMESPACE}" \
        --version "${LEGACY_CHART_VERSION}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1" \
        --set "logging.level=${E2E_LOGGING_LEVEL}"

    log "Waiting for legacy operator deployment"
    kubectl -n "${KANIOP_NAMESPACE}" rollout status deploy/"${RELEASE_NAME}" --timeout=180s
}

verify_legacy_crd() {
    log "Verifying legacy CRD is present"
    kubectl wait --for=condition=established \
        "crd/${LEGACY_PLURAL}" --timeout=60s \
        || fatal "Legacy CRD did not become established"
}

setup_kanidm() {
    log "Creating Kanidm cluster for migration test"
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
}

create_person_resources() {
    log "Creating person resources for migration test"

    kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: test-migration-person-alpha
  namespace: default
spec:
  kanidmRef:
    name: test-migration
  personAttributes:
    displayname: "Migration Alpha"
    mail:
      - alpha-migration@example.com
---
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: test-migration-person-labeled
  namespace: default
  labels:
    migration-test: "true"
    team: platform
  annotations:
    migration-test-annotation: "preserved"
spec:
  kanidmRef:
    name: test-migration
  personAttributes:
    displayname: "Migration Labeled"
    mail:
      - labeled-migration@example.com
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
    displayname: "Migration ArgoCD"
    mail:
      - argocd-migration@example.com
EOF

    if [[ "${MIGRATION_SOURCE_COUNT}" -ge 4 ]]; then
        kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: test-migration-person-posix
  namespace: default
spec:
  kanidmRef:
    name: test-migration
  personAttributes:
    displayname: "Migration Posix"
    mail:
      - posix-migration@example.com
  posixAttributes: {}
EOF
    fi

    log "Waiting for person resources to become Ready"
    for name in test-migration-person-alpha test-migration-person-labeled test-migration-person-argocd; do
        poll_person_ready "$name"
    done
    if [[ "${MIGRATION_SOURCE_COUNT}" -ge 4 ]]; then
        poll_person_ready "test-migration-person-posix"
    fi
}

poll_person_ready() {
    local name="$1"
    local iterations=0
    while [[ $iterations -lt 90 ]]; do
        local ready
        ready=$(kubectl -n default get "${LEGACY_PLURAL}" "${name}" \
            -o jsonpath='{.status.ready}' 2>/dev/null || echo "false")
        if [[ "${ready}" == "true" ]]; then
            log "Person ${name} is Ready"
            return
        fi
        sleep 2
        iterations=$((iterations + 1))
    done
    fatal "Person ${name} did not become Ready within timeout"
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

run_helm_upgrade() {
    local version
    version="$(cd "${REPO_ROOT}" && git rev-parse --short HEAD)"

    log "Running helm upgrade to current chart (version=${version})"

    (cd "${REPO_ROOT}" && make crdgen)

    helm upgrade "${RELEASE_NAME}" "${REPO_ROOT}/charts/kaniop" \
        --namespace "${KANIOP_NAMESPACE}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set-string "image.tag=${version}" \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1" \
        --set "logging.level=${E2E_LOGGING_LEVEL}" \
        --set "webhook.enabled=true" \
        --set-string "webhook.image.tag=${version}" \
        --set "webhook.logging.level=${E2E_LOGGING_LEVEL}"

    log "helm upgrade completed"
}

verify_post_migration_state() {
    log "Verifying post-migration state"

    log "Checking legacy CRD is gone"
    if kubectl get "crd/${LEGACY_PLURAL}" >/dev/null 2>&1; then
        fatal "Legacy CRD still exists after migration"
    fi

    log "Checking corrected CRD is established"
    kubectl wait --for=condition=established \
        "crd/${CORRECTED_PLURAL}" --timeout=120s

    log "Checking operator deployment is ready"
    kubectl -n "${KANIOP_NAMESPACE}" rollout status deploy/"${RELEASE_NAME}" --timeout=180s

    local person_count
    person_count=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o json 2>/dev/null \
        | jq '[.items[] | select(.metadata.name | startswith("test-migration-person-"))] | length')

    if [[ "${person_count}" -lt "${MIGRATION_SOURCE_COUNT}" ]]; then
        fatal "Expected at least ${MIGRATION_SOURCE_COUNT} persons, found ${person_count}"
    fi
    log "Found ${person_count} migrated person(s)"
}

run_post_migration_e2e() {
    log "Running post-migration Rust e2e tests (verify Kanidm UUID preservation)"
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
    log "Recording post-migration UIDs before the idempotency upgrade"
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

    log "Re-running helm upgrade (idempotency)"
    run_helm_upgrade
    verify_post_migration_state

    log "Running idempotent-rerun Rust e2e tests"
    export MIGRATION_STAGE="idempotent-rerun"
    export MIGRATION_EXPECTED_PHASE="Completed"
    (cd "${REPO_ROOT}" && \
        RUST_TEST_THREADS=1 cargo test \
        --target=x86_64-unknown-linux-gnu \
        -p kaniop-e2e-tests --features e2e-test --no-fail-fast \
        crd_migration_idempotent_rerun)
}

run_v0103_noop_upgrade() {
    local noop_version="0.10.3"
    local noop_release="kaniop-noop"
    local noop_marker="${noop_release}-person-crd-migration"

    log "=== Testing v${noop_version} no-op upgrade (corrected CRD already present) ==="

    log "Removing the completed migration fixture before starting the zero-person case"
    local names
    names=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
        | grep '^test-migration-person-' || true)
    for name in ${names}; do
        kubectl -n default delete "${CORRECTED_PLURAL}" "${name}" --wait --timeout=120s
    done
    kubectl -n default delete kanidm/test-migration --ignore-not-found=true --wait --timeout=180s
    kubectl -n "${KANIOP_NAMESPACE}" delete configmap kaniop-person-crd-migration \
        --ignore-not-found=true

    log "Uninstalling main migration release"
    helm uninstall "${RELEASE_NAME}" --namespace "${KANIOP_NAMESPACE}" --wait --timeout "${HELM_TIMEOUT}" 2>/dev/null || true

    log "Waiting for operator pods to terminate"
    kubectl -n "${KANIOP_NAMESPACE}" wait --for=delete pod -l app.kubernetes.io/instance="${RELEASE_NAME}" --timeout=60s 2>/dev/null || true

    log "Installing v${noop_version} chart (corrected CRD, no migration hooks)"
    preload_published_images "${noop_version}"
    helm install "${noop_release}" "${LEGACY_CHART_REF}" \
        --namespace "${KANIOP_NAMESPACE}" \
        --version "${noop_version}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1" \
        --set "logging.level=${E2E_LOGGING_LEVEL}"

    log "Waiting for v${noop_version} operator deployment"
    kubectl -n "${KANIOP_NAMESPACE}" rollout status deploy/"${noop_release}" --timeout=180s

    log "Verifying corrected CRD is established (from v${noop_version})"
    kubectl wait --for=condition=established \
        "crd/${CORRECTED_PLURAL}" --timeout=60s

    log "Verifying legacy CRD is absent"
    if kubectl get "crd/${LEGACY_PLURAL}" >/dev/null 2>&1; then
        fatal "Legacy CRD should not exist with v${noop_version}"
    fi

    log "Verifying zero person resources"
    local person_count
    person_count=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o json 2>/dev/null \
        | jq '[.items[] | select(.metadata.name | startswith("test-migration-person-"))] | length')
    if [[ "${person_count}" -ne 0 ]]; then
        fatal "Expected zero persons before no-op upgrade, found ${person_count}"
    fi

    log "Verifying no migration marker or backups exist"
    local marker_exists
    marker_exists=$(kubectl -n "${KANIOP_NAMESPACE}" get configmap "${noop_marker}" 2>/dev/null && echo "yes" || echo "no")
    if [[ "${marker_exists}" == "yes" ]]; then
        fatal "Migration marker should not exist before no-op upgrade"
    fi

    local backup_count
    backup_count=$(kubectl -n "${KANIOP_NAMESPACE}" get secret \
        -l kaniop.rs/migration=person-plural-v1 \
        -o json 2>/dev/null | jq '.items | length' || echo "0")
    if [[ "${backup_count}" -ne 0 ]]; then
        fatal "No backup secrets should exist before no-op upgrade, found ${backup_count}"
    fi

    local version
    version="$(cd "${REPO_ROOT}" && git rev-parse --short HEAD)"

    log "Upgrading v${noop_version} release to local chart (version=${version})"
    helm upgrade "${noop_release}" "${REPO_ROOT}/charts/kaniop" \
        --namespace "${KANIOP_NAMESPACE}" \
        --timeout "${HELM_TIMEOUT}" \
        --wait \
        --set-string "image.tag=${version}" \
        --set "env[0].name=KANIDM_DEV_YOLO" \
        --set-string "env[0].value=1" \
        --set "logging.level=${E2E_LOGGING_LEVEL}" \
        --set "webhook.enabled=true" \
        --set-string "webhook.image.tag=${version}" \
        --set "webhook.logging.level=${E2E_LOGGING_LEVEL}"

    log "Verifying operator deployment is ready after no-op upgrade"
    kubectl -n "${KANIOP_NAMESPACE}" rollout status deploy/"${noop_release}" --timeout=180s

    log "Verifying corrected CRD still established after no-op upgrade"
    kubectl wait --for=condition=established \
        "crd/${CORRECTED_PLURAL}" --timeout=60s

    log "Verifying no migration marker created by no-op upgrade"
    marker_exists=$(kubectl -n "${KANIOP_NAMESPACE}" get configmap "${noop_marker}" 2>/dev/null && echo "yes" || echo "no")
    if [[ "${marker_exists}" == "yes" ]]; then
        fatal "Migration marker should not be created by no-op upgrade (corrected CRD already present)"
    fi

    log "Verifying no backup secrets created by no-op upgrade"
    backup_count=$(kubectl -n "${KANIOP_NAMESPACE}" get secret \
        -l kaniop.rs/migration=person-plural-v1 \
        -o json 2>/dev/null | jq '.items | length' || echo "0")
    if [[ "${backup_count}" -ne 0 ]]; then
        fatal "No backup secrets should be created by no-op upgrade, found ${backup_count}"
    fi

    log "Verifying zero persons still (no resources created or modified)"
    person_count=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o json 2>/dev/null \
        | jq '[.items[] | select(.metadata.name | startswith("test-migration-person-"))] | length')
    if [[ "${person_count}" -ne 0 ]]; then
        fatal "Expected zero persons after no-op upgrade, found ${person_count}"
    fi

    log "Cleaning up no-op release"
    helm uninstall "${noop_release}" --namespace "${KANIOP_NAMESPACE}" --wait --timeout "${HELM_TIMEOUT}" 2>/dev/null || true

    log "=== v${noop_version} no-op upgrade: PASSED ==="
}

cleanup() {
    if [[ "${CLEANUP_ON_EXIT}" != "true" ]]; then
        log "CLEANUP_ON_EXIT=false, keeping cluster"
        return
    fi
    log "Cleaning up migration test resources (scoped to test-migration-* only)"

    kubectl -n "${KANIOP_NAMESPACE}" delete secret \
        -l kaniop.rs/migration=person-plural-v1 --ignore-not-found=true 2>/dev/null || true
    kubectl -n "${KANIOP_NAMESPACE}" delete configmap kaniop-person-crd-migration \
        --ignore-not-found=true 2>/dev/null || true

    local names
    names=$(kubectl -n default get "${CORRECTED_PLURAL}" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
        | grep '^test-migration-person-' || true)

    for name in ${names}; do
        kubectl -n default delete "${CORRECTED_PLURAL}" "${name}" --ignore-not-found=true 2>/dev/null || true
    done

    kubectl -n default delete kanidm/test-migration --ignore-not-found=true 2>/dev/null || true
    kubectl -n default delete secret/test-migration-tls --ignore-not-found=true 2>/dev/null || true
}

main() {
    check_prereqs
    trap cleanup EXIT

    setup_kind
    setup_kind_prerequisites
    build_and_load_current_images
    install_legacy_chart
    verify_legacy_crd
    setup_kanidm
    create_person_resources
    run_pre_migration_e2e
    run_helm_upgrade
    verify_post_migration_state
    run_post_migration_e2e
    record_post_migration_uids_and_run_idempotency
    run_v0103_noop_upgrade

    log "SUCCESS: CRD migration Helm e2e test passed"
}

main "$@"
