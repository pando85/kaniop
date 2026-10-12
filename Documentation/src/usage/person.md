---
title: Managing Persons
weight: 32000
indent: true
---

# Managing Persons

Kaniop manages individual person accounts in your Kanidm cluster using the `KanidmPersonAccount`
Custom Resource. This enables declarative user management with full Git-based workflows.

## Basic Person Account

Here's a simple person account configuration. You can reference the complete example at
[`examples/person.yaml`](https://github.com/pando85/kaniop/blob/{{KANIOP_VERSION}}/examples/person.yaml):

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
[`examples/person.yaml`](https://github.com/pando85/kaniop/blob/{{KANIOP_VERSION}}/examples/person.yaml):

```yaml
# POSIX configuration from examples/person.yaml
posixAttributes:
  gidnumber: 1000 # Auto-generated if omitted
  loginshell: /bin/bash
```

## SSH Public Keys

SSH keys can be managed by tag. Kanidm validates the key format and supported key types; Kaniop does not parse or validate SSH key material.

```yaml
spec:
  kanidmRef:
    name: my-idm
  personAttributes:
    displayname: Alice Smith
  sshPublicKeys:
    laptop: "ssh-ed25519 AAAA... alice@laptop"
    yubikey: "sk-ssh-ed25519@openssh.com AAAA... alice@yubikey"
```

The map is **partially managed**: Kaniop owns only the tags it has synchronized, so manually added SSH keys with other tags remain intact. Keys with a declared tag are adopted even when they already exist in Kanidm. The entire map is managed as a single server-side apply ownership unit, so applying an empty map transfers full ownership and revokes all previously managed keys.

| `sshPublicKeys` | Behavior |
| --- | --- |
| Omitted | Leave every existing key untouched and stop managing previously owned tags |
| Map with keys | Add missing keys, update changed keys, and remove previously managed tags omitted from the map |
| `{}` | Remove previously managed keys, preserving keys added outside Kaniop |

This is an **opt-in** feature. The `SshPublicKeysUpdated` condition reports whether the declared keys and recorded tag ownership have converged. A rejected SSH key leaves the resource not ready and the Kanidm error is surfaced by reconciliation.

**Key rotation warning:** Kanidm does not replace an existing SSH key tag via insertion. Kaniop therefore deletes the previous key before adding the new key under the same tag. This operation is not atomic: if Kanidm rejects a replacement, the original key may already have been removed. Verify replacement public keys before rotating access-critical keys.

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
