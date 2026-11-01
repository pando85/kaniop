---
title: Managing Service Accounts
weight: 33000
indent: true
---

# Managing Service Accounts

Kaniop manages individual service accounts in your Kanidm cluster using the `KanidmServiceAccount`
Custom Resource. This enables declarative service account management with full Git-based workflows.

## Basic Service Account

Here's a simple service account configuration. You can reference the complete example at
[`examples/service-account.yaml`](https://github.com/pando85/kaniop/blob/{{KANIOP_VERSION}}/examples/service-account.yaml):

```yaml
# Basic configuration - see examples/service-account.yaml for all options
apiVersion: kaniop.rs/v1beta1
kind: KanidmServiceAccount
metadata:
  name: alice
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  serviceAccountAttributes:
    displayname: Alice Smith
    entryManagedBy: admin
```

## Why service-account descriptions are not managed

Kaniop intentionally does **not** expose
`spec.serviceAccountAttributes.description`. During
[PR #1076](https://github.com/pando85/kaniop/pull/1076), Kanidm v1.11.2
rejected service-account `description` writes from the operator's
built-in administrative-group permissions with `403 AccessDenied` via the
attribute-write endpoint. A PATCH-based approach was also attempted, but
the feature was ultimately
[reverted](https://github.com/pando85/kaniop/commit/a04c85515eaea5c61b940740922cc6626a5f07e2)
rather than add a CRD field that the operator could not reliably reconcile.

This is an authorization limitation in the tested Kanidm setup, not a
restriction on storing service-account descriptions generally. An
appropriately authorized Kanidm client can manage them outside Kaniop;
Kaniop does not set, clear, or reconcile that attribute. Support should
be reconsidered only after confirming the required permissions and E2E
coverage for setting, updating, clearing, and drift repair.
