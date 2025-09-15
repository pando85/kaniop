---
title: Managing Persons
weight: 033000
indent: true
---

# Managing Persons

Kaniop manages individual person accounts in your Kanidm cluster using the `KanidmPersonAccount`
Custom Resource. This enables declarative user management with full Git-based workflows.

## Basic Person Account

Here's a simple person account configuration. You can reference the complete example at
[`examples/person.yaml`](https://github.com/pando85/kaniop/blob/master/examples/person.yaml):

```yaml
# Basic configuration - see examples/person.yaml for all options
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: alice
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  personAttributes:
    displayname: Alice Smith
    mail:
      - alice@example.com
```

## Advanced Configuration Options

The `examples/person.yaml` file demonstrates many additional features:

- **Email aliases**: Multiple email addresses with primary/secondary setup
- **Legal names**: Separate display and legal names
- **Account lifecycle**: `accountValidFrom` and `accountExpire` settings
- **POSIX attributes**: For SSH and filesystem access

### Complete Person with All Attributes

```yaml
# Extended example based on examples/person.yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: john-doe
  namespace: default
  labels:
    department: engineering
    role: senior-developer
spec:
  kanidmRef:
    name: my-idm
  personAttributes:
    displayname: John Doe
    legalname: John Michael Doe
    mail:
      - john.doe@example.com
      - j.doe@example.com # Alias
    accountValidFrom: "2024-01-01T00:00:00Z"
    accountExpire: "2025-12-31T23:59:59Z"
  posixAttributes:
    gidnumber: 1000
    loginshell: /bin/bash
```

## POSIX Account Configuration

For systems requiring POSIX compatibility (SSH, file systems), see the POSIX section in
[`examples/person.yaml`](https://github.com/pando85/kaniop/blob/master/examples/person.yaml):

```yaml
# POSIX configuration from examples/person.yaml
posixAttributes:
  gidnumber: 1000 # Auto-generated if omitted
  loginshell: /bin/bash
```

## Account Lifecycle Management

### Temporary Account

Create an account with an expiration date:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: contractor
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  personAttributes:
    displayname: Jane Contractor
    mail:
      - jane.contractor@example.com
    accountValidFrom: "2024-06-01T00:00:00Z"
    accountExpire: "2024-12-31T23:59:59Z"
```

### Future-Dated Account

Create an account that becomes active on a specific date:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: new-hire
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  personAttributes:
    displayname: New Employee
    mail:
      - new.employee@example.com
    accountValidFrom: "2024-09-15T09:00:00Z"
```

## Management Operations

### Apply Person Changes

```bash
kubectl apply -f person.yaml
```

### Check Person Status

```bash
kubectl get kanidmpersonaccounts
kubectl describe kanidmpersonaccount alice
```

### Monitor Person Events

```bash
kubectl get events --field-selector involvedObject.name=alice
```

### Update Person Attributes

Edit the YAML file and reapply:

```bash
kubectl apply -f person.yaml
```

Kaniop will detect changes and update the person account in Kanidm.

## Best Practices

- **Use email as identifier**: Consider using email addresses as the metadata name for clarity
- **Set expiration dates**: Use `accountExpire` for temporary accounts (contractors, interns)
- **Manage emails carefully**: The first email in the list becomes the primary address
- **Use labels**: Add Kubernetes labels for organization and filtering
- **Version control**: Store all person manifests in Git for audit trails

## Common Patterns

### Department-Based Organization

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: engineering-alice
  namespace: default
  labels:
    department: engineering
    team: backend
    level: senior
spec:
  kanidmRef:
    name: my-idm
  personAttributes:
    displayname: Alice Smith
    mail:
      - alice.smith@example.com
```

### Service Accounts

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: ci-service
  namespace: default
  labels:
    type: service-account
    purpose: ci-cd
spec:
  kanidmRef:
    name: my-idm
  personAttributes:
    displayname: CI/CD Service Account
    mail:
      - ci-cd@example.com
```

### Multi-Environment Users

```yaml
# Development environment
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: alice
  namespace: identity-dev
spec:
  kanidmRef:
    name: dev-idm
  personAttributes:
    displayname: Alice Smith (Dev)
    mail:
      - alice.dev@example.com
---
# Production environment
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: alice
  namespace: identity-prod
spec:
  kanidmRef:
    name: prod-idm
  personAttributes:
    displayname: Alice Smith
    mail:
      - alice@example.com
```

## Troubleshooting

### Account Not Created

Check the person resource status:

```bash
kubectl describe kanidmpersonaccount alice
```

Look for error messages in the `Conditions` section.

### Email Conflicts

If you see conflicts with email addresses, ensure each email is unique across all person accounts in
the same Kanidm cluster.

### POSIX Issues

For POSIX-related problems, verify that `gidnumber` values don't conflict with existing system or
user IDs.
