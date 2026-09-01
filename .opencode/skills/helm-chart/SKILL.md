---
name: helm-chart
description: Guidelines for modifying the Helm chart. Use when changing values.yaml, templates, or adding new configuration options.
---

## Helm Chart Modification Checklist

When modifying the Helm chart at `charts/kaniop/`, follow this checklist to ensure completeness.

### 1. values.yaml

Add or modify the value with proper YAML doc comments:

```yaml
## Kubernetes cluster domain used for DNS resolution.
## Change this if your cluster uses a non-default DNS domain.
clusterDomain: "cluster.local"
```

### 2. values.schema.json

**Always update** when adding or modifying a value. Add a matching JSON schema entry:

```json
"clusterDomain": {
  "type": "string",
  "description": "Kubernetes cluster domain used for DNS resolution.",
  "default": "cluster.local"
}
```

Find the right location by searching for neighboring values (e.g., add after `idmReconcileIntervalSeconds`).

### 3. Templates

Update the relevant template(s) to use the new value. For env vars:

```yaml
- name: CLUSTER_DOMAIN
  value: {{ .Values.clusterDomain }}
```

For conditional rendering:

```yaml
{{- if .Values.tracing.enabled }}
```

### 4. Unit Tests (Required)

**All Helm chart changes must include unit tests.** Tests live in `charts/kaniop/tests/`.

#### Test File Naming

- `{resource}_test.yaml` (e.g., `deployment_test.yaml`, `webhook-certificate_test.yaml`)
- One test file per template or logical group

#### Test Structure

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/helm-unittest/helm-unittest/main/schema/helm-testsuite.json
suite: test deployment
templates:
  - templates/deployment.yaml
tests:
  - it: Should render with default values
    release:
      name: kaniop
      namespace: kaniop
    asserts:
      - contains:
          path: spec.template.spec.containers[0].env
          content:
            name: CLUSTER_DOMAIN
            value: cluster.local
          any: true

  - it: Should render with custom value
    release:
      name: kaniop
      namespace: kaniop
    set:
      clusterDomain: mycluster.example.com
    asserts:
      - contains:
          path: spec.template.spec.containers[0].env
          content:
            name: CLUSTER_DOMAIN
            value: mycluster.example.com
          any: true
```

#### Testing Multi-Document Templates

When a template renders multiple documents (e.g., cert-manager.yaml renders Issuer + Certificate), use `documentIndex` or provide an `issuerRef` to limit rendering:

```yaml
# With issuerRef, only the Certificate is rendered (1 document)
set:
  webhook.certManager.enabled: true
  webhook.certManager.issuerRef.name: selfsigned-issuer
  webhook.certManager.issuerRef.kind: ClusterIssuer
asserts:
  - hasDocuments:
      count: 1
```

#### Running Tests

```bash
helm unittest charts/kaniop
```

All tests must pass before committing.

## Operator Environment Variable Pattern

When adding a new operator configuration via env var, follow this pattern:

### 1. Add clap arg in `cmd/operator/src/main.rs`:

```rust
#[arg(long, default_value = "cluster.local", env)]
cluster_domain: String,
```

The `env` attribute auto-maps to uppercase field name (`CLUSTER_DOMAIN`). Use `env = "CUSTOM_NAME"` for different names.

### 2. Add global getter/setter in `libs/operator/src/controller/mod.rs`:

```rust
const DEFAULT_CLUSTER_DOMAIN: &str = "cluster.local";
static CLUSTER_DOMAIN: OnceLock<String> = OnceLock::new();

pub fn set_cluster_domain(domain: String) {
    let _ = CLUSTER_DOMAIN.set(domain);
}

pub fn cluster_domain() -> &'static str {
    CLUSTER_DOMAIN.get_or_init(|| DEFAULT_CLUSTER_DOMAIN.to_string())
}
```

### 3. Call setter in `cmd/operator/src/main.rs`:

```rust
set_cluster_domain(args.cluster_domain);
```

### 4. Import and use in reconcilers:

```rust
use crate::controller::cluster_domain;

let host = format!("{}.{}.svc.{}", service, namespace, cluster_domain());
```

## Common Pitfalls

| Pitfall | Fix |
|---------|-----|
| Forgetting values.schema.json | Always update schema when changing values |
| No unit test for new feature | Add test with default AND custom value cases |
| Hardcoded `cluster.local` | Use `.Values.clusterDomain` in templates |
| Multi-document template test failures | Use `issuerRef` or `documentIndex` to scope assertions |
| Env var not passed to deployment | Add to `env` list in `templates/deployment.yaml` |
