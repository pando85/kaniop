---
title: Managing Kanidm Clusters
weight: 030100
---

# Managing Kanidm Clusters

Deploy and operate Kanidm identity management clusters on Kubernetes using the `Kanidm` Custom
Resource Definition (CRD). Kaniop provides a cloud-native abstraction for managing the complete
Kanidm lifecycle—from initial deployment through scaling and certificate rotation.

## Key Capabilities

- **Declarative deployment**: Define cluster topology in YAML; Kaniop handles StatefulSets,
  Services, and certificates.
- **High availability**: Multi-replica configurations with different groups of replica roles and
  topologies.
- **Zero-touch certificate management**: Automated generation, distribution, and renewal of
  replication TLS certificates.
- **External node federation**: Seamlessly integrate external Kanidm instances into your Kaniop
  cluster.

## Basic Kanidm Cluster

Here's a simple Kanidm cluster configuration. You can reference the complete example at
[`examples/kanidm.yaml`](https://github.com/pando85/kaniop/blob/master/examples/kanidm.yaml):

```yaml
apiVersion: kaniop.rs/v1beta1
kind: Kanidm
metadata:
  name: my-idm
spec:
  domain: my-idm.localhost
  replicaGroups:
    - name: default
      replicas: 1
```

Notice that this configuration requires a TLS certificate with the name `my-idm-tls`.
