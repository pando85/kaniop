---
title: Managing Groups
weight: 032000
indent: true
---

# Managing Groups

Groups in Kanidm represent collections of user accounts for authorization and policy management.
Kaniop uses the `KanidmGroup` Custom Resource to manage these entities declaratively.

## Basic Group Configuration

Here's a simple group configuration. You can reference the complete example at
[`examples/group.yaml`](https://github.com/pando85/kaniop/blob/master/examples/group.yaml):

```yaml
# Basic configuration - see examples/group.yaml for complete setup
apiVersion: kaniop.rs/v1beta1
kind: KanidmGroup
metadata:
  name: developers
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  members:
    - alice
    - bob
    - charlie
```

## Advanced Group Features

The `examples/group.yaml` file demonstrates advanced features:

- **Email aliases**: Groups with mailing list functionality
- **POSIX groups**: For file system permissions
- **Member management**: Adding users and nested groups

### Group with Email Configuration

Based on the patterns in
[`examples/group.yaml`](https://github.com/pando85/kaniop/blob/master/examples/group.yaml):

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmGroup
metadata:
  name: engineering-team
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  members:
    - alice
    - bob
  mail:
    - engineering@example.com
    - dev-team@example.com
```

The first email address in the list becomes the primary address.

## POSIX Group Configuration

For systems requiring POSIX compatibility, you can configure group IDs:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmGroup
metadata:
  name: sysadmins
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  members:
    - alice
  posixAttributes:
    gidnumber: 1001
```

If `gidnumber` is omitted, Kanidm will auto-generate one.

## Management Operations

### Apply Group Changes

```bash
kubectl apply -f group.yaml
```

### Check Group Status

```bash
kubectl get kanidmgroups
kubectl describe kanidmgroup developers
```

## Best Practices

- **Use descriptive names**: Choose names that clearly indicate the group's purpose
- **Manage membership centrally**: Use the `members` field for consistent membership management
- **Organize by function**: Create groups based on roles and responsibilities
