---
title: Managing Service Accounts
weight: 033000
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
