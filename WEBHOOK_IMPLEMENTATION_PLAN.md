# Webhook Implementation Plan: Duplicate Object Validation

## Executive Summary

Implement validating admission webhook to prevent creation of Kaniop custom resources (KanidmGroup,
KanidmPersonAccount, KanidmOAuth2Client, KanidmServiceAccount) with duplicate `name` and `kanidmRef`
combinations.

**Key Constraints:**

- Only validate CREATE operations (kanidmRef is immutable via CRD validation)
- Use in-memory stores (kube reflectors) to avoid API server load during validation
- Reuse existing operator libs architecture and patterns
- No Prometheus metrics required in webhook
- Normalize namespaces: if `kanidmRef.namespace` is omitted, use object's namespace

---

## Architecture Overview

### Current State Analysis

**Existing Webhook Code (`cmd/webhook/src/main.rs`):**

- Basic structure present: single handler `validate_kanidm_group`
- Uses direct API calls (`Api::list()`) — **inefficient**, causes API load
- Lacks stores/watchers
- Only handles KanidmGroup
- Hardcoded admission response logic

**Operator Pattern (Reference: `libs/operator/src/controller/`):**

- Uses `Store<T>` (reflector-backed in-memory cache) for resources
- `create_subscriber()` creates store+writer+subscriber
- Watchers stream events into stores
- `KanidmResource` trait provides `kanidm_ref_spec()`, namespace normalization

---

## Implementation Plan

### Phase 1: Core Infrastructure (Foundation)

#### 1.1 Create Shared Validation Module

**File:** `cmd/webhook/src/validator.rs`

**Responsibilities:**

- Generic validation logic for all resource types
- KanidmRef normalization (namespace defaulting)
- Duplicate detection algorithm

**Implementation:**

```rust
use kaniop_operator::crd::KanidmRef;
use kube::{Resource, ResourceExt};
use kube::runtime::reflector::Store;

/// Normalize KanidmRef by filling in missing namespace with object's namespace
pub fn normalize_kanidm_ref(kanidm_ref: &KanidmRef, object_namespace: &str) -> (String, String) {
    let ref_name = kanidm_ref.name.clone();
    let ref_namespace = kanidm_ref
        .namespace
        .clone()
        .unwrap_or_else(|| object_namespace.to_string());
    (ref_name, ref_namespace)
}

/// Generic duplicate checker for any resource type
pub fn check_duplicate<T>(
    object: &T,
    object_name: &str,
    store: &Store<T>,
) -> Result<(), String>
where
    T: Resource + ResourceExt + Clone,
    T: HasKanidmRef,  // New trait to abstract kanidm_ref access
{
    let obj_namespace = object.namespace().unwrap_or_else(|| "default".to_string());
    let (ref_name, ref_namespace) = normalize_kanidm_ref(
        object.kanidm_ref_spec(),
        &obj_namespace
    );

    // Search store for conflicts
    for resource in store.state() {
        // Skip self (same UID)
        if resource.meta().uid == object.meta().uid {
            continue;
        }

        let res_namespace = resource.namespace().unwrap_or_else(|| "default".to_string());
        let (res_ref_name, res_ref_namespace) = normalize_kanidm_ref(
            resource.kanidm_ref_spec(),
            &res_namespace
        );

        // Check for duplicate
        if ref_name == res_ref_name && ref_namespace == res_ref_namespace {
            return Err(format!(
                "{} with same kanidmRef already exists: {}/{}",
                object_name,
                resource.namespace().unwrap_or_else(|| "default".to_string()),
                resource.name_any()
            ));
        }
    }

    Ok(())
}

/// Trait to abstract access to kanidm_ref across different resource types
pub trait HasKanidmRef {
    fn kanidm_ref_spec(&self) -> &KanidmRef;
}
```

**Why this approach:**

- Reusable across all 4 resource types
- Mirrors operator's `KanidmResource` trait pattern
- Namespace normalization logic in one place
- Easy to test

---

#### 1.2 Implement `HasKanidmRef` Trait for All Resources

**File:** `cmd/webhook/src/resources.rs`

**Implementation:**

```rust
use kaniop_operator::crd::KanidmRef;
use kaniop_group::crd::KanidmGroup;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_service_account::crd::KanidmServiceAccount;
use crate::validator::HasKanidmRef;

impl HasKanidmRef for KanidmGroup {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }
}

impl HasKanidmRef for KanidmPersonAccount {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }
}

impl HasKanidmRef for KanidmOAuth2Client {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }
}

impl HasKanidmRef for KanidmServiceAccount {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }
}
```

---

### Phase 2: State Management (Stores & Watchers)

#### 2.1 Create Webhook State with Stores

**File:** `cmd/webhook/src/state.rs`

**Implementation:**

```rust
use kube::runtime::reflector::Store;
use kaniop_group::crd::KanidmGroup;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_service_account::crd::KanidmServiceAccount;

#[derive(Clone)]
pub struct WebhookState {
    pub group_store: Store<KanidmGroup>,
    pub person_store: Store<KanidmPersonAccount>,
    pub oauth2_store: Store<KanidmOAuth2Client>,
    pub service_account_store: Store<KanidmServiceAccount>,
}

impl WebhookState {
    pub fn new(
        group_store: Store<KanidmGroup>,
        person_store: Store<KanidmPersonAccount>,
        oauth2_store: Store<KanidmOAuth2Client>,
        service_account_store: Store<KanidmServiceAccount>,
    ) -> Self {
        Self {
            group_store,
            person_store,
            oauth2_store,
            service_account_store,
        }
    }
}
```

**Why this approach:**

- One store per resource type (standard kube pattern)
- Stores are cloneable (cheap Arc clones)
- No locks needed (Store is thread-safe)

---

#### 2.2 Initialize Watchers in `main.rs`

**Update:** `cmd/webhook/src/main.rs`

**Pattern:** Mirror operator's setup in `cmd/operator/src/main.rs`

**Key Changes:**

```rust
use kube::runtime::reflector;
use kube::runtime::watcher::{self, Config as WatcherConfig};
use kube::runtime::WatchStreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ... existing setup ...

    let client = Client::try_default().await?;

    // Create stores and writers for each resource type
    let (group_store, group_writer) = reflector::store_shared::<KanidmGroup>(256);
    let (person_store, person_writer) = reflector::store_shared::<KanidmPersonAccount>(256);
    let (oauth2_store, oauth2_writer) = reflector::store_shared::<KanidmOAuth2Client>(256);
    let (sa_store, sa_writer) = reflector::store_shared::<KanidmServiceAccount>(256);

    // Create APIs
    let group_api = Api::<KanidmGroup>::all(client.clone());
    let person_api = Api::<KanidmPersonAccount>::all(client.clone());
    let oauth2_api = Api::<KanidmOAuth2Client>::all(client.clone());
    let sa_api = Api::<KanidmServiceAccount>::all(client.clone());

    // Create watchers (background tasks)
    let group_watcher = watcher(group_api, WatcherConfig::default())
        .default_backoff()
        .reflect_shared(group_writer)
        .for_each(|_| futures::future::ready(()));

    let person_watcher = watcher(person_api, WatcherConfig::default())
        .default_backoff()
        .reflect_shared(person_writer)
        .for_each(|_| futures::future::ready(()));

    let oauth2_watcher = watcher(oauth2_api, WatcherConfig::default())
        .default_backoff()
        .reflect_shared(oauth2_writer)
        .for_each(|_| futures::future::ready(()));

    let sa_watcher = watcher(sa_api, WatcherConfig::default())
        .default_backoff()
        .reflect_shared(sa_writer)
        .for_each(|_| futures::future::ready(()));

    // Create state
    let state = WebhookState::new(
        group_store,
        person_store,
        oauth2_store,
        sa_store,
    );

    // Create router
    let app = Router::new()
        .route("/health", get(health))
        .route("/validate-kanidm-group", post(validate_kanidm_group))
        .route("/validate-kanidm-person", post(validate_kanidm_person))
        .route("/validate-kanidm-oauth2", post(validate_kanidm_oauth2))
        .route("/validate-kanidm-service-account", post(validate_kanidm_service_account))
        .with_state(state);

    // Run server and watchers concurrently
    let listener = TcpListener::bind(format!("0.0.0.0:{}", args.port)).await?;
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    tokio::select! {
        _ = server => {},
        _ = group_watcher => {},
        _ = person_watcher => {},
        _ = oauth2_watcher => {},
        _ = sa_watcher => {},
    }

    Ok(())
}
```

**Why this approach:**

- Watchers run in background, keep stores updated
- No API calls during validation (all cached)
- Standard kube-rs reflector pattern
- Watchers auto-reconnect on errors (default_backoff)

---

### Phase 3: Admission Handlers

#### 3.1 Create Generic Admission Response Helpers

**File:** `cmd/webhook/src/admission.rs`

**Implementation:**

```rust
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct AdmissionReview<T> {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub request: Option<AdmissionRequest<T>>,
    pub response: Option<AdmissionResponse>,
}

#[derive(Deserialize, Serialize)]
pub struct AdmissionRequest<T> {
    pub uid: String,
    pub operation: String,
    pub object: Option<T>,
}

#[derive(Deserialize, Serialize)]
pub struct AdmissionResponse {
    pub uid: String,
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

#[derive(Deserialize, Serialize)]
pub struct Status {
    pub message: String,
}

impl AdmissionResponse {
    pub fn allow(uid: String) -> Self {
        Self {
            uid,
            allowed: true,
            status: None,
        }
    }

    pub fn deny(uid: String, message: impl Into<String>) -> Self {
        Self {
            uid,
            allowed: false,
            status: Some(Status {
                message: message.into(),
            }),
        }
    }
}

impl<T> AdmissionReview<T> {
    pub fn response(self, response: AdmissionResponse) -> AdmissionReview<()> {
        AdmissionReview {
            api_version: "admission.k8s.io/v1".to_string(),
            kind: "AdmissionReview".to_string(),
            request: None,
            response: Some(response),
        }
    }
}
```

---

#### 3.2 Implement Handler Macro for DRY

**File:** `cmd/webhook/src/handlers.rs`

**Implementation:**

```rust
use axum::extract::State;
use axum::response::Json;
use crate::admission::{AdmissionReview, AdmissionResponse};
use crate::state::WebhookState;
use crate::validator::{check_duplicate, HasKanidmRef};
use kube::{Resource, ResourceExt};
use tracing::{debug, error};

/// Generic validation handler
pub async fn validate_resource<T>(
    state: &WebhookState,
    store: &kube::runtime::reflector::Store<T>,
    review: AdmissionReview<T>,
    resource_name: &str,
) -> Json<AdmissionReview<()>>
where
    T: Resource + ResourceExt + Clone + HasKanidmRef,
{
    let request = match review.request.as_ref() {
        Some(req) => req,
        None => {
            error!("Missing request in admission review");
            return Json(review.response(AdmissionResponse::deny(
                "unknown".to_string(),
                "Invalid admission review: missing request",
            )));
        }
    };

    let uid = request.uid.clone();
    let operation = &request.operation;

    // Only validate CREATE operations (kanidmRef is immutable)
    if operation != "CREATE" {
        debug!(
            "Skipping validation for {} operation on {}",
            operation, resource_name
        );
        return Json(review.response(AdmissionResponse::allow(uid)));
    }

    let object = match request.object.as_ref() {
        Some(obj) => obj,
        None => {
            error!("Missing object in CREATE request");
            return Json(review.response(AdmissionResponse::deny(
                uid,
                "Invalid admission review: missing object",
            )));
        }
    };

    // Check for duplicates
    match check_duplicate(object, resource_name, store) {
        Ok(_) => {
            debug!(
                "Validation passed for {} {}/{}",
                resource_name,
                object.namespace().unwrap_or_else(|| "default".to_string()),
                object.name_any()
            );
            Json(review.response(AdmissionResponse::allow(uid)))
        }
        Err(err) => {
            debug!(
                "Validation failed for {} {}/{}: {}",
                resource_name,
                object.namespace().unwrap_or_else(|| "default".to_string()),
                object.name_any(),
                err
            );
            Json(review.response(AdmissionResponse::deny(uid, err)))
        }
    }
}

// Concrete handlers for each resource type
pub async fn validate_kanidm_group(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_group::crd::KanidmGroup>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(&state, &state.group_store, review, "KanidmGroup").await
}

pub async fn validate_kanidm_person(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_person::crd::KanidmPersonAccount>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(&state, &state.person_store, review, "KanidmPersonAccount").await
}

pub async fn validate_kanidm_oauth2(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_oauth2::crd::KanidmOAuth2Client>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(&state, &state.oauth2_store, review, "KanidmOAuth2Client").await
}

pub async fn validate_kanidm_service_account(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_service_account::crd::KanidmServiceAccount>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(&state, &state.service_account_store, review, "KanidmServiceAccount").await
}
```

**Why this approach:**

- Generic `validate_resource` eliminates duplication
- Each handler is one-liner calling generic function
- Clear error messages with resource type context
- Proper logging at debug level

---

### Phase 4: Kubernetes Configuration

#### 4.1 Update ValidatingWebhookConfiguration

**File:** `charts/kaniop/templates/validating-webhook-configuration.yaml`

**Changes:**

- Add webhooks for all 4 resource types
- All target CREATE operations only (UPDATE blocked by CRD immutability)
- Match existing pattern for KanidmGroup

```yaml
{{- if .Values.webhook.enabled }}
apiVersion: admissionregistration.k8s.io/v1
kind: ValidatingWebhookConfiguration
metadata:
  name: {{ include "kaniop.fullname" . }}-webhook
  labels:
    {{- include "kaniop.labels" . | nindent 4 }}
webhooks:
  - name: validate-kanidm-group.kaniop.rs
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: {{ include "kaniop.fullname" . }}-webhook
        namespace: {{ .Release.Namespace }}
        path: /validate-kanidm-group
      caBundle: {{ .Values.webhook.caBundle | default "" | b64enc }}
    failurePolicy: Fail
    matchConditions:
      - name: 'exclude-leases'
        expression: '!(request.resource.group == "coordination.k8s.io" && request.resource.resource == "leases")'
    rules:
      - operations: [CREATE]
        apiGroups: [kaniop.rs]
        apiVersions: [v1beta1]
        resources: [kanidmgroups]
    sideEffects: None
    timeoutSeconds: 10

  - name: validate-kanidm-person.kaniop.rs
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: {{ include "kaniop.fullname" . }}-webhook
        namespace: {{ .Release.Namespace }}
        path: /validate-kanidm-person
      caBundle: {{ .Values.webhook.caBundle | default "" | b64enc }}
    failurePolicy: Fail
    matchConditions:
      - name: 'exclude-leases'
        expression: '!(request.resource.group == "coordination.k8s.io" && request.resource.resource == "leases")'
    rules:
      - operations: [CREATE]
        apiGroups: [kaniop.rs]
        apiVersions: [v1beta1]
        resources: [kanidmpersonsaccounts]
    sideEffects: None
    timeoutSeconds: 10

  - name: validate-kanidm-oauth2.kaniop.rs
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: {{ include "kaniop.fullname" . }}-webhook
        namespace: {{ .Release.Namespace }}
        path: /validate-kanidm-oauth2
      caBundle: {{ .Values.webhook.caBundle | default "" | b64enc }}
    failurePolicy: Fail
    matchConditions:
      - name: 'exclude-leases'
        expression: '!(request.resource.group == "coordination.k8s.io" && request.resource.resource == "leases")'
    rules:
      - operations: [CREATE]
        apiGroups: [kaniop.rs]
        apiVersions: [v1beta1]
        resources: [kanidmoauth2clients]
    sideEffects: None
    timeoutSeconds: 10

  - name: validate-kanidm-service-account.kaniop.rs
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: {{ include "kaniop.fullname" . }}-webhook
        namespace: {{ .Release.Namespace }}
        path: /validate-kanidm-service-account
      caBundle: {{ .Values.webhook.caBundle | default "" | b64enc }}
    failurePolicy: Fail
    matchConditions:
      - name: 'exclude-leases'
        expression: '!(request.resource.group == "coordination.k8s.io" && request.resource.resource == "leases")'
    rules:
      - operations: [CREATE]
        apiGroups: [kaniop.rs]
        apiVersions: [v1beta1]
        resources: [kanidmserviceaccounts]
    sideEffects: None
    timeoutSeconds: 10
{{- end }}
```

---

#### 4.2 Update Dependencies in `Cargo.toml`

**File:** `cmd/webhook/Cargo.toml`

**Add dependencies:**

```toml
[dependencies]
# Existing...
kaniop-group = { workspace = true }
kaniop-person = { workspace = true }
kaniop-oauth2 = { workspace = true }
kaniop-service-account = { workspace = true }
kaniop-operator = { workspace = true }
# ... rest existing
futures = { workspace = true }
```

---

### Phase 5: Testing Strategy

#### 5.1 Unit Tests

**File:** `cmd/webhook/src/validator.rs` (add test module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kaniop_operator::crd::KanidmRef;

    #[test]
    fn test_normalize_kanidm_ref_with_namespace() {
        let ref_obj = KanidmRef {
            name: "my-kanidm".to_string(),
            namespace: Some("other-ns".to_string()),
        };
        let (name, ns) = normalize_kanidm_ref(&ref_obj, "current-ns");
        assert_eq!(name, "my-kanidm");
        assert_eq!(ns, "other-ns");
    }

    #[test]
    fn test_normalize_kanidm_ref_without_namespace() {
        let ref_obj = KanidmRef {
            name: "my-kanidm".to_string(),
            namespace: None,
        };
        let (name, ns) = normalize_kanidm_ref(&ref_obj, "current-ns");
        assert_eq!(name, "my-kanidm");
        assert_eq!(ns, "current-ns");
    }

    // Add tests for check_duplicate with mock stores
}
```

#### 5.2 Integration Tests

**Strategy:**

1. Deploy webhook in e2e environment (`make e2e`)
2. Create test manifests attempting duplicates
3. Verify rejection via kubectl apply
4. Verify acceptance for non-duplicates

**Test Cases:**

- Same name, same namespace, same kanidmRef → REJECT
- Same name, different namespace, same kanidmRef (if cross-namespace) → REJECT
- Same name, same namespace, different kanidmRef.name → ACCEPT
- Same name, same namespace, different kanidmRef.namespace → ACCEPT
- Omitted kanidmRef.namespace defaults to object namespace → REJECT duplicate

---

## Implementation Order

1. **Phase 1.1-1.2:** Core validation logic (can be tested in isolation)
2. **Phase 2.1-2.2:** State and watchers (verify stores populate)
3. **Phase 3.1-3.2:** Handlers (test with mock AdmissionReview)
4. **Phase 4.1:** Helm configuration
5. **Phase 4.2:** Dependencies
6. **Phase 5:** Testing

---

## Success Criteria

- [ ] `make lint` passes (zero clippy warnings)
- [ ] `make test` passes (unit tests)
- [ ] `make e2e` successfully deploys webhook
- [ ] Manual e2e validation:
  - Create resource A with kanidmRef X → SUCCESS
  - Create resource B with same kanidmRef X → REJECT
  - Create resource C with kanidmRef Y → SUCCESS
- [ ] All 4 resource types validated
- [ ] Webhook logs show store updates (watcher events)
- [ ] No API calls during validation (check logs/metrics)
- [ ] Proper error messages returned to kubectl
- [ ] Handles edge cases:
  - Omitted namespace in kanidmRef
  - Cross-namespace kanidmRef
  - Empty stores on startup (allow creation)

---

## Rollback Plan

If issues arise:

1. Set `webhook.enabled: false` in Helm values
2. Redeploy: `helm upgrade kaniop charts/kaniop`
3. Webhook bypassed, operator continues normally

---

## Performance Considerations

**Store Memory:**

- ~1KB per resource (estimate)
- 1000 resources = ~1MB per store
- 4 stores × 1MB = ~4MB total (negligible)

**Validation Latency:**

- Store lookup: O(n) where n = resources in cluster
- Worst case: 10k resources = ~10ms scan
- Well within 10s timeout

**Store Sync:**

- Initial sync on startup: list all resources
- Incremental updates via watch stream
- Eventual consistency: brief window where new resource not in store (acceptable—duplicate will be
  caught by operator reconcile or next webhook call)

---

## Edge Cases & Mitigations

| Edge Case                        | Mitigation                                                               |
| -------------------------------- | ------------------------------------------------------------------------ |
| Store not yet synced on startup  | Accept first CREATE (optimistic), subsequent CREATEs validated           |
| Watcher disconnects              | `default_backoff()` auto-reconnects, temporary validation gap acceptable |
| API server slow/unavailable      | Watchers buffer/retry; webhook fails open if store empty? (discuss)      |
| Race: two CREATEs simultaneously | Webhook serializes via Kubernetes admission controller queue             |
| UPDATE operation sneaks in       | CRD-level immutability validation rejects (defense in depth)             |

**Recommendation:** Fail **closed** (deny) if stores not populated to avoid admitting duplicates.

---

## Code Reuse from Operator

- `kaniop_operator::crd::{KanidmRef, is_default}` — existing
- `kube::runtime::reflector::{Store, store_shared, watcher}` — pattern
- Error handling: `anyhow`, `tracing` — consistent
- No controller-specific code needed (metrics, reconcile, etc.)

---

## Files to Create/Modify

### Create:

- `cmd/webhook/src/state.rs`
- `cmd/webhook/src/validator.rs`
- `cmd/webhook/src/resources.rs`
- `cmd/webhook/src/admission.rs`
- `cmd/webhook/src/handlers.rs`

### Modify:

- `cmd/webhook/src/main.rs` (major refactor: add watchers, state, routes)
- `cmd/webhook/Cargo.toml` (add dependencies)
- `charts/kaniop/templates/validating-webhook-configuration.yaml` (add 3 new webhooks)

### Delete (optional):

- Old `validate_kanidm_group` implementation in `main.rs` (replace with new handlers)

---

## Validation Checklist (Pre-PR)

- [ ] All imports at top-level (no inline `use`)
- [ ] YAML formatting: compact arrays, quoted `on`
- [ ] No hand-edited CRDs
- [ ] Clippy clean
- [ ] Tests cover happy path + 1 edge case minimum
- [ ] Error messages clear for end users
- [ ] Logging at appropriate levels (debug for validation flow, error for failures)
- [ ] No blocking calls in async handlers
- [ ] Dependencies justified (all reused from workspace or essential)

---

## Open Questions for Review

1. **Fail open vs closed:** If stores unpopulated, allow or deny CREATE?
   - **Recommendation:** Deny with clear error "webhook not ready, retry"
2. **Readiness probe:** Wait for stores to sync before marking ready?
   - **Recommendation:** Yes, block readiness until initial list complete
3. **RBAC:** Webhook needs `list`/`watch` on all 4 resource types
   - **Action:** Update `charts/kaniop/templates/clusterrole.yaml`

---

## Timeline Estimate

- Phase 1: 2-3 hours
- Phase 2: 2-3 hours
- Phase 3: 1-2 hours
- Phase 4: 1 hour
- Phase 5: 2-3 hours
- **Total: 8-12 hours** (single engineer, assuming familiarity with codebase)

---

## References

- Operator watcher setup: `cmd/operator/src/main.rs:110-140`
- Reflector pattern: `libs/operator/src/controller/mod.rs:create_subscriber()`
- KanidmResource trait: `libs/operator/src/controller/kanidm.rs:28-50`
- CRD immutability: `libs/operator/src/crd.rs:30` (x-kubernetes-validations)
- Existing webhook: `cmd/webhook/src/main.rs:58-143`

---

**End of Plan**
