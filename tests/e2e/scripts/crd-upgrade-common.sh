#!/usr/bin/env bash
#
# Shared helpers for the CRD upgrade end-to-end tests:
#   - tests/e2e/scripts/crd-upgrade-helm.sh   (helm upgrade path)
#   - tests/e2e/scripts/crd-upgrade-argocd.sh (Argo CD sync path)
#
# Both tests install the previous published chart, make every Kaniop CRD stale using the
# field manager of the `crdApply` hook, upgrade to the current local chart, and verify that the
# hook server-side applied the current generated CRDs.
#
# Environment variables:
#   PREVIOUS_CHART_VERSION - chart version to upgrade from (default: latest published chart)
#   KANIOP_NAMESPACE       - namespace for the operator (default: kaniop)
#   KIND_CLUSTER_NAME      - Kind cluster name (default set by each script)
#   KIND_IMAGE_TAG         - Kind node image tag (default: v1.34.3)
#   SKIP_KIND_CREATE       - set to "true" to reuse an existing Kind cluster
#   SKIP_IMAGE_BUILD       - set to "true" to skip building images (use pre-built)
#   SKIP_CRDGEN            - set to "true" to skip running make crdgen
#   KANIOP_IMAGE_VERSION   - override image version tag (default: g$(git rev-parse --short HEAD))
#   CLEANUP_ON_EXIT        - set to "false" to keep the Kind cluster on exit (default: true)
#   E2E_LOGGING_LEVEL      - log filter for operator (default: info,kaniop=debug,kaniop_webhook=debug)
#   HELM_TIMEOUT           - helm operation timeout (default: 10m)
#
# Use KUBECONFIG to keep the test cluster out of your default kubeconfig.

COMMON_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${COMMON_DIR}/../../.." && pwd)"

PREVIOUS_CHART_VERSION="${PREVIOUS_CHART_VERSION:-}"
KANIOP_NAMESPACE="${KANIOP_NAMESPACE:-kaniop}"
KIND_IMAGE_TAG="${KIND_IMAGE_TAG:-v1.34.3}"
SKIP_KIND_CREATE="${SKIP_KIND_CREATE:-false}"
SKIP_IMAGE_BUILD="${SKIP_IMAGE_BUILD:-false}"
SKIP_CRDGEN="${SKIP_CRDGEN:-false}"
KANIOP_IMAGE_VERSION="${KANIOP_IMAGE_VERSION:-}"
CLEANUP_ON_EXIT="${CLEANUP_ON_EXIT:-true}"
E2E_LOGGING_LEVEL="${E2E_LOGGING_LEVEL:-info,kaniop=debug,kaniop_webhook=debug}"
HELM_TIMEOUT="${HELM_TIMEOUT:-10m}"

CHART_REF="oci://ghcr.io/pando85/helm-charts/kaniop"
RELEASE_NAME="kaniop"
CRDS_FILE="${REPO_ROOT}/charts/kaniop/crds/crds.yaml"
# Field manager used by `kaniop-crd-migrator apply-crds` (cmd/crd-migrator/src/apply_crds.rs).
CRD_FIELD_MANAGER="kaniop-helm-crds"
STALE_ANNOTATION="kaniop.rs/crd-upgrade-e2e"
STALE_DESCRIPTION="stale schema written by the CRD upgrade e2e test"
PERSON_PLURAL="kanidmpersonaccounts.kaniop.rs"
TEST_PREFIX="test-crd-upgrade"
KANIDM_NAME="${TEST_PREFIX}"
WEBHOOK_CERTGEN_IMAGE="registry.k8s.io/ingress-nginx/kube-webhook-certgen:v1.6.9"

WORK_DIR="$(mktemp -d)"
KIND_CREATED="false"

export KANIDM_DEV_YOLO=1

log() { echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*"; }
fatal() { log "FATAL: $*" >&2; exit 1; }

check_prereqs() {
    local cmd
    for cmd in "$@"; do
        command -v "$cmd" >/dev/null 2>&1 || fatal "$cmd not found in PATH"
    done
}

current_version() {
    echo "${KANIOP_IMAGE_VERSION:-g$(git -C "${REPO_ROOT}" rev-parse --short HEAD)}"
}

resolve_previous_chart_version() {
    if [[ -z "${PREVIOUS_CHART_VERSION}" ]]; then
        PREVIOUS_CHART_VERSION=$( (helm show chart "${CHART_REF}" 2>/dev/null || true) \
            | sed -n 's/^version: *"\{0,1\}\([^"]*\)"\{0,1\}$/\1/p')
        [[ -n "${PREVIOUS_CHART_VERSION}" ]] || fatal "could not resolve the latest published chart version"
    fi
    log "Upgrading from published chart version ${PREVIOUS_CHART_VERSION}"
}

setup_kind() {
    if [[ "${SKIP_KIND_CREATE}" == "true" ]]; then
        log "SKIP_KIND_CREATE=true, reusing existing cluster"
        kubectl config use-context "kind-${KIND_CLUSTER_NAME}" || fatal "context kind-${KIND_CLUSTER_NAME} not found"
        return
    fi
    if kind get clusters 2>/dev/null | grep -qx "${KIND_CLUSTER_NAME}"; then
        fatal "Kind cluster ${KIND_CLUSTER_NAME} already exists; delete it, choose another KIND_CLUSTER_NAME, or set SKIP_KIND_CREATE=true"
    fi
    log "Creating Kind cluster ${KIND_CLUSTER_NAME}"
    KIND_CREATED="true"
    kind create cluster --name "${KIND_CLUSTER_NAME}" --image "kindest/node:${KIND_IMAGE_TAG}"
    kubectl cluster-info --context "kind-${KIND_CLUSTER_NAME}" >/dev/null
}

# Load a Docker image into the Kind cluster with retry logic.
# Kind v0.33.0 uses `ctr images import --all-platforms` internally, which can fail with
# "content digest not found" for multi-arch images when not all platform blobs are present.
# Retry with a fresh pull to handle transient multi-arch image loading issues.
load_image() {
    local image="$1"
    local attempt
    for attempt in 1 2 3; do
        if kind load --name "${KIND_CLUSTER_NAME}" docker-image "${image}"; then
            return 0
        fi
        if [[ ${attempt} -lt 3 ]]; then
            log "kind load failed for ${image}, re-pulling with platform=linux/amd64 and retrying (${attempt}/3)"
            # Pull with explicit platform to avoid multi-arch digest issues with kind load
            docker pull --platform linux/amd64 "${image}" >/dev/null
        fi
    done
    fatal "failed to load image ${image} into Kind after 3 attempts"
}

preload_published_images() {
    local image
    for image in ghcr.io/pando85/kaniop ghcr.io/pando85/kaniop-webhook; do
        log "Pulling and loading ${image}:${PREVIOUS_CHART_VERSION}"
        docker pull --platform linux/amd64 "${image}:${PREVIOUS_CHART_VERSION}"
        load_image "${image}:${PREVIOUS_CHART_VERSION}"
    done
}

build_and_load_current_images() {
    local version
    version="$(current_version)"
    if [[ "${SKIP_IMAGE_BUILD}" == "true" ]]; then
        log "SKIP_IMAGE_BUILD=true: loading pre-built images for version=${version}"
    else
        log "Building current operator images (version=${version})"
        (cd "${REPO_ROOT}" && VERSION="${version}" make images)
    fi
    load_image "ghcr.io/pando85/kaniop:${version}"
    load_image "ghcr.io/pando85/kaniop-webhook:${version}"
    # Pull with explicit platform to avoid multi-arch digest issues with kind load
    docker pull --platform linux/amd64 "${WEBHOOK_CERTGEN_IMAGE}"
    load_image "${WEBHOOK_CERTGEN_IMAGE}"
}

generate_crds() {
    if [[ "${SKIP_CRDGEN}" != "true" ]]; then
        (cd "${REPO_ROOT}" && make crdgen)
    else
        log "SKIP_CRDGEN=true: using ${CRDS_FILE}"
    fi
    kubectl create --dry-run=client -o json -f "${CRDS_FILE}" \
        | jq -s 'map(if .kind == "List" then .items[] else . end)' >"${WORK_DIR}/crds.json"
    [[ "$(jq length "${WORK_DIR}/crds.json")" -gt 0 ]] || fatal "no CRDs found in ${CRDS_FILE}"
}

crd_names() {
    jq -r '.[].metadata.name' "${WORK_DIR}/crds.json"
}

# Normalize a CRD spec for comparison: drop the server-defaulted conversion strategy and
# listKind, and sort keys.
normalize_crd_spec() {
    jq -S '.spec | del(.conversion) | del(.names.listKind)'
}

wait_for_person_ready() {
    local name="$1"
    local iterations=0
    while [[ $iterations -lt 90 ]]; do
        if [[ "$(kubectl -n default get "${PERSON_PLURAL}" "${name}" \
            -o jsonpath='{.status.ready}' 2>/dev/null)" == "true" ]]; then
            log "Person ${name} is Ready"
            return
        fi
        sleep 2
        iterations=$((iterations + 1))
    done
    kubectl -n default get "${PERSON_PLURAL}" "${name}" -o yaml || true
    fatal "Person ${name} did not become Ready within 180s"
}

create_person() {
    local name="$1"
    kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: ${name}
  namespace: default
spec:
  kanidmRef:
    name: ${KANIDM_NAME}
  personAttributes:
    displayname: "CRD upgrade ${name}"
    mail:
      - ${name}@example.com
EOF
}

setup_kanidm_and_persons() {
    log "Creating Kanidm cluster and person resources"
    local tls_dir
    tls_dir=$(mktemp -d)
    openssl req -x509 -nodes -newkey rsa:2048 \
        -keyout "${tls_dir}/ca.key" -out "${tls_dir}/ca.crt" \
        -days 1 -subj "/CN=Kaniop CRD upgrade test CA" \
        -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign" >/dev/null 2>&1
    openssl req -nodes -newkey rsa:2048 \
        -keyout "${tls_dir}/tls.key" -out "${tls_dir}/tls.csr" \
        -subj "/CN=${KANIDM_NAME}.localhost" >/dev/null 2>&1
    cat >"${tls_dir}/tls.ext" <<EOF
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:${KANIDM_NAME}.localhost
EOF
    openssl x509 -req -in "${tls_dir}/tls.csr" \
        -CA "${tls_dir}/ca.crt" -CAkey "${tls_dir}/ca.key" -CAcreateserial \
        -out "${tls_dir}/tls-leaf.crt" -days 1 \
        -extfile "${tls_dir}/tls.ext" >/dev/null 2>&1
    cat "${tls_dir}/tls-leaf.crt" "${tls_dir}/ca.crt" >"${tls_dir}/tls.crt"
    kubectl -n default create secret tls "${KANIDM_NAME}-tls" \
        --cert="${tls_dir}/tls.crt" --key="${tls_dir}/tls.key"
    rm -rf "${tls_dir}"

    kubectl apply -f - <<EOF
apiVersion: kaniop.rs/v1beta1
kind: Kanidm
metadata:
  name: ${KANIDM_NAME}
  namespace: default
spec:
  domain: ${KANIDM_NAME}.localhost
  tlsSecretName: ${KANIDM_NAME}-tls
  replicaGroups:
    - name: default
      replicas: 1
EOF
    kubectl -n default wait --for=condition=Available "kanidm/${KANIDM_NAME}" --timeout=300s
    kubectl -n default wait --for=condition=Initialized "kanidm/${KANIDM_NAME}" --timeout=180s

    create_person "${TEST_PREFIX}-alpha"
    create_person "${TEST_PREFIX}-beta"
    wait_for_person_ready "${TEST_PREFIX}-alpha"
    wait_for_person_ready "${TEST_PREFIX}-beta"
}

# Re-apply every generated CRD with the crdApply field manager, a stale schema description and
# a marker annotation. Only an apply by the same field manager that omits the annotation removes
# it, so its absence after the upgrade proves that the crdApply hook ran. kubectl's client-side
# schema validation rejects the null entries of nullable enums, so leave validation to the
# API server.
make_crds_stale() {
    log "Making every Kaniop CRD stale with field manager ${CRD_FIELD_MANAGER}"
    jq --arg ann "${STALE_ANNOTATION}" --arg desc "${STALE_DESCRIPTION}" \
        '{apiVersion: "v1", kind: "List", items: map(
            .metadata.annotations[$ann] = "stale"
            | .spec.versions |= map(.schema.openAPIV3Schema.description = $desc))}' \
        "${WORK_DIR}/crds.json" \
        | kubectl apply --server-side --force-conflicts --validate=false \
            --field-manager="${CRD_FIELD_MANAGER}" -f -

    local name
    for name in $(crd_names); do
        if [[ "$(kubectl get crd "${name}" -o json \
            | jq -r --arg ann "${STALE_ANNOTATION}" '.metadata.annotations[$ann] // ""')" != "stale" ]]; then
            fatal "CRD ${name} was not marked stale"
        fi
    done
}

# Usage: verify_crds_current [require-hook]
# With require-hook=false only the live CRD specs are compared, without requiring that the
# crdApply hook (rather than another field manager such as Argo CD) applied them.
verify_crds_current() {
    local require_hook="${1:-true}"
    if [[ "${require_hook}" == "true" ]]; then
        log "Verifying that the crdApply hook applied the current CRDs"
    else
        log "Verifying that the live CRDs match the current generated CRDs"
    fi
    local name expected actual
    for name in $(crd_names); do
        kubectl wait --for=condition=established "crd/${name}" --timeout=120s >/dev/null \
            || fatal "CRD ${name} is not Established"

        kubectl get crd "${name}" --show-managed-fields -o json >"${WORK_DIR}/live.json"
        if [[ "${require_hook}" == "true" && "$(jq -r --arg ann "${STALE_ANNOTATION}" '.metadata.annotations[$ann] // ""' \
            "${WORK_DIR}/live.json")" != "" ]]; then
            fatal "CRD ${name} still has the stale marker: the crdApply hook did not apply it"
        fi
        if [[ "${require_hook}" == "true" ]] && ! jq -e --arg fm "${CRD_FIELD_MANAGER}" \
            'any(.metadata.managedFields[]; .manager == $fm and .operation == "Apply")' \
            "${WORK_DIR}/live.json" >/dev/null; then
            fatal "CRD ${name} has no server-side apply entry for ${CRD_FIELD_MANAGER}"
        fi

        expected="${WORK_DIR}/expected-${name}.json"
        actual="${WORK_DIR}/actual-${name}.json"
        jq --arg name "${name}" '.[] | select(.metadata.name == $name)' "${WORK_DIR}/crds.json" \
            | normalize_crd_spec >"${expected}"
        normalize_crd_spec <"${WORK_DIR}/live.json" >"${actual}"
        if ! diff -u "${expected}" "${actual}"; then
            fatal "CRD ${name} spec does not match ${CRDS_FILE}"
        fi
        log "CRD ${name} matches the generated CRD"
    done
}

verify_resources_after_upgrade() {
    local deployment="$1"
    local stage="${2:-upgrade}"
    log "Verifying the upgraded operator and existing resources"
    kubectl -n "${KANIOP_NAMESPACE}" rollout status "deploy/${deployment}" --timeout=180s
    kubectl -n "${KANIOP_NAMESPACE}" get deploy "${deployment}" \
        -o jsonpath='{.spec.template.spec.containers[0].image}{"\n"}'
    kubectl -n default wait --for=condition=Available "kanidm/${KANIDM_NAME}" --timeout=300s
    wait_for_person_ready "${TEST_PREFIX}-alpha"
    wait_for_person_ready "${TEST_PREFIX}-beta"

    local new_person="${TEST_PREFIX}-after-${stage}"
    log "Creating ${new_person} to prove the upgraded operator reconciles new objects"
    create_person "${new_person}"
    wait_for_person_ready "${new_person}"
}

dump_diagnostics() {
    log "Collecting diagnostics"
    kubectl get pods -A -o wide || true
    kubectl -n "${KANIOP_NAMESPACE}" get jobs || true
    kubectl -n "${KANIOP_NAMESPACE}" get events --sort-by=.lastTimestamp || true
    kubectl -n "${KANIOP_NAMESPACE}" logs -l app.kubernetes.io/component=crd-apply --tail=200 || true
    kubectl -n "${KANIOP_NAMESPACE}" logs "deploy/${RELEASE_NAME}" --tail=200 || true
    kubectl -n default get kanidm,"${PERSON_PLURAL}" -o wide || true
}

cleanup_kind() {
    local exit_code="$1"
    if [[ "${exit_code}" -ne 0 ]]; then
        dump_diagnostics
    fi
    if [[ "${CLEANUP_ON_EXIT}" != "true" ]]; then
        log "CLEANUP_ON_EXIT=false, keeping Kind cluster ${KIND_CLUSTER_NAME} and ${WORK_DIR}"
        return
    fi
    rm -rf "${WORK_DIR}"
    if [[ "${KIND_CREATED}" == "true" ]]; then
        log "Deleting Kind cluster ${KIND_CLUSTER_NAME}"
        kind delete cluster --name "${KIND_CLUSTER_NAME}" || true
    fi
}
