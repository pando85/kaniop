---
title: "Upgrade Guides"
weight: 39000
---

# Upgrade Guides

This section contains upgrade procedures for Kaniop releases that require special migration
steps beyond a standard `helm upgrade`.

## Minimum supported upgrade source

> **Breaking change**: upgrade to Kaniop v0.11.0 or later before upgrading to any release after
> v0.20.x.

Releases after v0.20.x no longer ship the one-time `KanidmPersonAccount` CRD plural migration
that v0.11.0 to v0.20.x ran from Helm hooks or Argo CD sync hooks. The `crdMigration` chart
values were removed with it, and the chart values schema rejects them: delete any
`crdMigration` keys from your values files before upgrading.

| Running version | Upgrade path |
|---|---|
| v0.11.0 or later | Upgrade directly with a standard `helm upgrade` or Argo CD sync. |
| v0.10.3 | Upgrade to a v0.11.0 to v0.19.3 release first (the migration is a no-op there), then upgrade. |
| v0.10.2 or older | Follow the [v0.11 CRD Plural Migration](v0.11-migration.md) guide with a v0.11.0 to v0.19.3 release (for example `--version 0.19.3`), then upgrade. |

Use v0.19.3 or earlier as the intermediate release. v0.20.0 also contains the migration, but
its verification step fails when person resources exist, because v0.20.0 added a defaulted
field to the person CRD that the migration's checksum comparison does not account for.

If the legacy `kanidmpersonsaccounts.kaniop.rs` CRD is still installed and the `crdApply` hook
is enabled (the default), the pre-upgrade hook fails before it changes any CRD, and the
upgrade stops with a message that points to this page. Nothing is deleted.

### Clean up migration leftovers

The v0.11.0 to v0.20.x migration hooks created ServiceAccounts, RBAC, Jobs, a marker
ConfigMap, and backup Secrets that Helm and Argo CD do not track, so upgrading does not remove
them. The migration ClusterRole can delete CRDs. After upgrading from one of those releases,
delete them (replace `kaniop` with your namespace; the marker is named
`<release-fullname>-person-crd-migration`):

```bash
kubectl delete clusterrole,clusterrolebinding -l app.kubernetes.io/component=crd-migration
kubectl -n kaniop delete serviceaccount,role,rolebinding,job \
  -l app.kubernetes.io/component=crd-migration
kubectl -n kaniop delete configmap kaniop-person-crd-migration --ignore-not-found
```

The migration also stored person backups in Secrets labeled
`kaniop.rs/migration=person-plural-v1`. Delete them once you no longer need them:

```bash
kubectl -n kaniop delete secret -l kaniop.rs/migration=person-plural-v1
```

## Migration guides

- [v0.11 CRD Plural Migration](v0.11-migration.md) — Historical guide for moving from v0.10.x
  to v0.11.0+. The `KanidmPersonAccount` CRD plural name was corrected from
  `kanidmpersonsaccounts` to `kanidmpersonaccounts`. Only v0.11.0 to v0.20.x releases contain
  the migration Job that preserves Kanidm identities and person specifications; use v0.19.3 or
  earlier for it.

## Standard upgrades

For releases not listed above, follow the standard upgrade procedure documented in the
[Installation guide](../installation.md#upgrading-kaniop):

```bash
helm upgrade --namespace kaniop --wait kaniop \
  oci://ghcr.io/pando85/helm-charts/kaniop \
  --version <target-version>
```
