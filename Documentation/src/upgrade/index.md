---
title: "Upgrade Guides"
weight: 39000
---

# Upgrade Guides

This section contains upgrade procedures for Kaniop releases that require special migration
steps beyond a standard `helm upgrade`.

## Current migration guides

- [v0.11 CRD Plural Migration](v0.11-migration.md) — Mandatory upgrade procedure for
  moving from v0.10.x to v0.11.0+. The `KanidmPersonAccount` CRD plural name was corrected
  from `kanidmpersonsaccounts` to `kanidmpersonaccounts`, requiring a migration Job that
  preserves Kanidm identities and person specifications.

## Standard upgrades

For releases not listed above, follow the standard upgrade procedure documented in the
[Installation guide](../installation.md#upgrading-kaniop):

```bash
helm upgrade --namespace kaniop --wait kaniop \
  oci://ghcr.io/pando85/helm-charts/kaniop \
  --version <target-version>
```
