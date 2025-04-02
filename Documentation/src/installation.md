---
title: Installation
weight: 020000
---

This guide covers installing Kaniop in your Kubernetes cluster using various methods.

## Prerequisites

Before installing Kaniop, ensure you have:

- Kubernetes cluster 1.21+ (v1.20+ is minimum supported)
- Helm 3.8+ (for Helm installation)
- `kubectl` configured to access your cluster
- Sufficient RBAC permissions to install operators

## Installing with Helm (Recommended)

The recommended way to install Kaniop is using the official Helm chart:

```bash
# Install Kaniop using OCI registry
helm install --create-namespace --namespace kaniop --wait kaniop oci://ghcr.io/pando85/helm-charts/kaniop
```

This command will:

- Create the `kaniop` namespace (if it doesn't exist)
- Deploy the Kaniop operator into the specified namespace
- Wait for the deployment to complete

### Custom Configuration

You can customize the installation by creating a values file:

```bash
# Create custom values file
cat > kaniop-values.yaml << EOF
operator:
  replicas: 2
  resources:
    requests:
      cpu: 100m
      memory: 128Mi
    limits:
      cpu: 500m
      memory: 512Mi

webhook:
  enabled: true
  failurePolicy: Fail
EOF

# Install with custom values
helm install --create-namespace --namespace kaniop --wait
  --values kaniop-values.yaml
  kaniop oci://ghcr.io/pando85/helm-charts/kaniop
```

Check that the Kaniop operator is running successfully:

```bash
kubectl get pods -n kaniop
```

You should see a pod with the name `kaniop-<release-name>` in a `Running` state.

### Step 3: Configure Kaniop

Once installed, you can start managing Kanidm clusters by applying Kubernetes manifests for your
identity management resources. Refer to the [Usage Guide](usage.md) for detailed instructions on
configuring and managing Kanidm resources.

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

This will delete all resources created by the Kaniop Helm chart, but it will not remove any Kanidm
resources you have created.
