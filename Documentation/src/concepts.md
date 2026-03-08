---
title: Concepts
weight: 011000
indent: true
---

# Concepts

Kaniop introduces a set of Custom Resources (CRs) to manage Kanidm clusters and related identity
objects in a declarative, GitOps-friendly manner. Behind the scenes, the operator watches your CRs
and reconciles the cluster state to match your declarations.

## Custom Resources

- **Kanidm** (`kaniop.rs/v1beta1`): Defines the topology and configuration of a Kanidm cluster,
  including replica groups, storage, and high availability.
- **KanidmPersonAccount** (`kaniop.rs/v1beta1`): Creates and manages a person account in the target
  Kanidm cluster.
- **KanidmGroup** (`kaniop.rs/v1beta1`): Defines a group of accounts for authorization and policy
  mapping in Kanidm.
- **KanidmOAuth2Client** (`kaniop.rs/v1beta1`): Configures OAuth2/OpenID Connect clients and their
  scopes, claims, and redirect URLs.

## KanidmRef

Most identity objects (person, group, OAuth2 client) reference the Kanidm cluster they belong to
using a `kanidmRef` block:

```yaml
kanidmRef:
  name: my-idm # The name of the Kanidm resource in the same namespace
  # namespace: default  # Optional if cross-namespace references are allowed
```

This ensures that your identity objects are always reconciled against the correct cluster instance.

## GitOps and Reconciliation

Kaniop follows Kubernetes controller conventions:

1. **Declare** your desired state in YAML files and commit to Git.
2. **Apply** CRs to your cluster via `kubectl apply` or Helm.
3. **Watch** the cluster: The Kaniop operator monitors CRs continuously.
4. **Reconcile**: Any drift between desired and actual state is automatically corrected.

This model allows you to treat identity management just like any other Kubernetes
workload—versioned, reviewed, and rolled out with your existing CI/CD pipelines.
