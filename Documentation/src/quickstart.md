---
title: Quickstart
weight: 012000
indent: true
---

# Quickstart

This quickstart will guide you through deploying a Kanidm cluster with Kaniop and creating your
first identity resources. In about 10 minutes, you'll have a fully functional identity management
system running on Kubernetes.

## Prerequisites

- A Kubernetes cluster (v1.20+)
- `kubectl` configured to access your cluster
- Helm v3 installed

## Step 1: Install the Operator

Install the Kaniop operator using Helm:

```bash
helm install --create-namespace --namespace kaniop --wait kaniop oci://ghcr.io/pando85/helm-charts/kaniop
```

Verify the operator is running:

```bash
kubectl get pods -n kaniop
# Expected output: kaniop-<release-name> pod in Running state
```

## Step 2: Deploy a Kanidm Cluster

Create a simple Kanidm cluster configuration. You can use the basic example from the repository:

```bash
# Use the basic Kanidm example
curl -O https://raw.githubusercontent.com/pando85/kaniop/main/examples/kanidm.yaml
```

Or create your own `my-idm.yaml` with:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: Kanidm
metadata:
  name: my-idm
  namespace: default
spec:
  domain: idm.example.com # Change this to your domain
  replicaGroups:
    - name: default
      replicas: 1
```

Apply the manifest:

```bash
kubectl apply -f kanidm.yaml  # or my-idm.yaml if you created your own
```

Wait for the StatefulSet to be ready:

```bash
kubectl get statefulsets -l kanidm.kaniop.rs/cluster=my-idm
kubectl wait --for=condition=ready pod -l kanidm.kaniop.rs/cluster=my-idm --timeout=300s
```

## Step 3: Configure OAuth2 Client

Set up an OAuth2 client using the repository example:

```bash
# Use the OAuth2 example
curl -O https://raw.githubusercontent.com/pando85/kaniop/main/examples/oauth2.yaml
```

The example contains:

```yaml
# See examples/oauth2.yaml for the complete configuration
apiVersion: kaniop.rs/v1beta1
kind: KanidmOAuth2Client
metadata:
  name: my-service
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  displayname: My Service
  origin: https://my-service.localhost
  redirectUrl:
    - https://my-service.localhost/oauth2/callback
  # Advanced options like scopeMap, claimMap available - see examples/oauth2.yaml
```

Apply and verify:

```bash
kubectl apply -f oauth2.yaml
kubectl get kanidmoauth2clients
```

## Step 4: Create a Group

Create a group using the repository example:

```bash
# Use the group example
curl -O https://raw.githubusercontent.com/pando85/kaniop/main/examples/group.yaml
```

The example contains:

```yaml
# See examples/group.yaml for the complete configuration
apiVersion: kaniop.rs/v1beta1
kind: KanidmGroup
metadata:
  name: my-group
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  # members:
  # - me
  # Additional options available - see examples/group.yaml
```

Apply and verify:

```bash
kubectl apply -f group.yaml
kubectl get kanidmgroups
```

## Step 5: Create a Person Account

Create a user account using the example from the repository:

```bash
# Use the person example
curl -O https://raw.githubusercontent.com/pando85/kaniop/main/examples/person.yaml
```

The example contains:

```yaml
# See examples/person.yaml for the complete configuration
apiVersion: kaniop.rs/v1beta1
kind: KanidmPersonAccount
metadata:
  name: me
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  personAttributes:
    displayname: Me
    # mail:
    # - me@my-idm.localhost
    # - alias-me@my-idm.localhost
    # Additional attributes available - see examples/person.yaml
```

Apply the person account:

```bash
kubectl apply -f person.yaml
```

Verify the account was created:

```bash
kubectl get kanidmpersonaccounts
kubectl describe kanidmpersonaccount me
```

## Next Steps

🎉 **Congratulations!** You now have:

- A running Kanidm cluster managed by Kaniop
- A user account (`me`) ready for configuration
- A group (`my-group`) for organizing users
- An OAuth2 client (`my-service`) for application integration

### Explore More Examples

The `examples/` directory contains additional configurations:

- [`examples/kanidm-ingress.yaml`](https://github.com/pando85/kaniop/blob/master/examples/kanidm-ingress.yaml) -
  Kanidm with Ingress configuration
- [`examples/kanidm-replication.yaml`](https://github.com/pando85/kaniop/blob/master/examples/kanidm-replication.yaml) -
  Multi-replica setup with storage
- [`examples/kanidm-tls.yaml`](https://github.com/pando85/kaniop/blob/master/examples/kanidm-tls.yaml) -
  TLS configuration

### What's Next?

1. **[Installation Guide](installation.md)**: Learn about production-ready configurations
2. **[Usage Guide](usage.md)**: Dive deeper into managing persons, groups, and OAuth2 clients
3. **[Configuration](helm-charts.md)**: Customize your Kaniop deployment with Helm values

### Accessing Your Kanidm Instance

To access the Kanidm web interface:

```bash
kubectl port-forward svc/my-idm 8443:8443 -n default
```

Then open [https://localhost:8443](https://localhost:8443) in your browser.
