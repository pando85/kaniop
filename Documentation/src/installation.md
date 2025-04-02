---
title: Installation
weight: 2000
---

# Installation

This guide will walk you through the installation of Kaniop, a Kubernetes operator for managing Kanidm clusters. By following these steps, you'll have Kaniop up and running in your Kubernetes environment in no time.

## Prerequisites

Before installing Kaniop, ensure the following prerequisites are met:

1. **Kubernetes Cluster**: A running Kubernetes cluster (v1.20 or later is recommended).
2. **Helm**: Helm CLI installed (v3.0 or later).
3. **Namespace**: A dedicated namespace for Kaniop (optional but recommended).
4. **Access**: Sufficient permissions to install Helm charts and create Kubernetes resources.

## Installation Steps

### Step 1: Install Kaniop

Install the Kaniop Helm chart into your Kubernetes cluster:

```bash
helm install --create-namespace --namespace kaniop --wait kaniop oci://ghcr.io/pando85/helm-charts/kaniop
```

This command will:

- Create the `kaniop` namespace (if it doesn't already exist).
- Deploy the Kaniop operator into the specified namespace.
- Wait for the deployment to complete before exiting.

### Step 2: Verify the Installation

Check that the Kaniop operator is running successfully:

```bash
kubectl get pods -n kaniop
```

You should see a pod with the name `kaniop-<release-name>` in a `Running` state.

### Step 3: Configure Kaniop

Once installed, you can start managing Kanidm clusters by applying Kubernetes manifests for your identity management resources. Refer to the [Usage Guide](usage.md) for detailed instructions on configuring and managing Kanidm resources.

## Upgrading Kaniop

To upgrade Kaniop to the latest version, run the following command:

```bash
helm upgrade --namespace kaniop --wait kaniop oci://ghcr.io/pando85/helm-charts/kaniop
```

## Uninstalling Kaniop

To remove Kaniop from your cluster, use the following command:

```bash
helm uninstall --namespace kaniop kaniop
```

This will delete all resources created by the Kaniop Helm chart, but it will not remove any Kanidm resources you have created.
