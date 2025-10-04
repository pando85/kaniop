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
kubectl apply -f https://raw.githubusercontent.com/pando85/kaniop/master/examples/kanidm-tls.yaml
kubectl apply -f https://raw.githubusercontent.com/pando85/kaniop/master/examples/kanidm.yaml
```

Wait for the StatefulSet to be ready:

```bash
kubectl get statefulsets -l kanidm.kaniop.rs/cluster=my-idm
kubectl wait --for=condition=ready pod -l kanidm.kaniop.rs/cluster=my-idm --timeout=300s
```

## Step 3: Configure OAuth2 Client

Set up an OAuth2 client using the repository example:

```bash
# Use the OAuth2 client example
kubectl apply -f https://raw.githubusercontent.com/pando85/kaniop/master/examples/oauth2.yaml
```

You can verify it with:

```bash
kubectl get kanidmoauth2clients
```

## Step 4: Create a Group

Create a group using the repository example:

```bash
# Use the group example
kubectl apply -f https://raw.githubusercontent.com/pando85/kaniop/master/examples/group.yaml
```

Verify the group was created:

```bash
kubectl get kanidmgroups
```

## Step 5: Create a Person Account

Create a user account using the example from the repository:

```bash
# Use the person example
kubectl apply -f https://raw.githubusercontent.com/pando85/kaniop/master/examples/person.yaml
```

Verify the account was created and get the link to set the credentials:

```bash
kubectl get kanidmpersonaccounts
kubectl describe kanidmpersonaccount me
```

## Next Steps

🎉 **Congratulations!** You now have:

- A running Kanidm cluster managed by Kaniop
- An OAuth2 client (`my-service`) for application integration
- A group (`my-group`) for organizing users
- A user account (`me`) ready for configuration

### Explore More Examples

The `examples/` directory contains additional configurations:

- [`examples/kanidm-ingress.yaml`](https://github.com/pando85/kaniop/blob/master/examples/kanidm-ingress.yaml) -
  Kanidm with Ingress configuration
- [`examples/kanidm-replication.yaml`](https://github.com/pando85/kaniop/blob/master/examples/kanidm-replication.yaml) -
  Multi-replica setup with storage

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
