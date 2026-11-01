# Kaniop Chart

A Helm chart for Kaniop, a Kubernetes operator for managing [Kanidm](https://kanidm.com).

Source code can be found here:

- https://github.com/pando85/kaniop/tree/master/charts/kaniop
- https://github.com/pando85/kaniop

## Installation

```bash
# Install Kaniop using OCI registry
helm install --create-namespace --namespace kaniop --wait kaniop oci://ghcr.io/pando85/helm-charts/kaniop
```

This command will:

- Create the `kaniop` namespace (if it doesn't exist)
- Deploy the Kaniop operator into the specified namespace
- Wait for the deployment to complete

## Upgrading

> **Minimum upgrade source**: upgrades are supported only from Kaniop v0.11.0 or later. Releases
> after v0.20.x no longer ship the v0.11 `KanidmPersonAccount` CRD plural migration or the
> `crdMigration` values. If you run v0.10.x or older, upgrade to a v0.11.0 to v0.19.3 release
> first, following the
> [v0.11 CRD Plural Migration guide](https://pando85.github.io/docs/kaniop/latest/upgrade/v0.11-migration.html),
> and then upgrade to this release.

To upgrade Kaniop to the latest version, run the following command:

```bash
helm upgrade --namespace kaniop --wait kaniop oci://ghcr.io/pando85/helm-charts/kaniop
helm show crds oci://ghcr.io/pando85/helm-charts/kaniop | kubectl apply --server-side \
  --force-conflicts -f -
```

A `crdApply` pre-install/pre-upgrade hook (enabled by default) also server-side applies the
current CRDs on every Helm install and upgrade, because Helm never updates CRDs itself. Argo CD
runs it as a PreSync hook and also applies the chart's CRDs itself.

## Uninstallation

```bash
helm uninstall kaniop
```

This removes all the Kubernetes components associated with the chart and deletes the release.

_See [helm uninstall](https://helm.sh/docs/helm/helm_uninstall/) for command documentation._

CRDs created by this chart are not removed by default and should be manually cleaned up:

```bash
helm show crds oci://ghcr.io/pando85/helm-charts/kaniop | kubectl delete -f -
```
