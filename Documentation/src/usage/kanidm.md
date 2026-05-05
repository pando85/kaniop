---
title: Managing Kanidm Clusters
weight: 030000
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
- **Safe upgrades**: Automated upgrade checks and rolling upgrades with minimal downtime.

## Basic Kanidm Cluster

Here's a simple Kanidm cluster configuration. You can reference the complete example at
[`examples/kanidm.yaml`](https://github.com/pando85/kaniop/blob/{{KANIOP_VERSION}}/examples/kanidm.yaml):

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

## Security Context Configuration

For production deployments, it's recommended to configure security contexts to comply with
Kubernetes [Pod Security Standards](https://kubernetes.io/docs/concepts/security/pod-security-standards/)
at the **restricted** level. This ensures your Kanidm deployment follows security best practices
and can run in clusters enforcing strict security policies.

### Recommended Configuration

To meet PSA:restricted requirements, configure both pod-level and container-level security contexts:

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
  # Pod-level security context
  securityContext:
    runAsNonRoot: true
    runAsUser: 389
    runAsGroup: 389
    fsGroup: 389
    seccompProfile:
      type: RuntimeDefault
  # Container-level security context
  containers:
    - name: kanidm
      securityContext:
        allowPrivilegeEscalation: false
        capabilities:
          drop:
            - ALL
  # Init container security context
  initContainers:
    - name: kanidm-generate-replication-config
      securityContext:
        allowPrivilegeEscalation: false
        capabilities:
          drop:
            - ALL
```

### Key Security Settings

The configuration above applies these critical security controls:

- **runAsNonRoot: true**: Ensures containers run as non-root users
- **runAsUser/runAsGroup**: Defines the user and group IDs for all containers (UID/GID 389 recommended, matching LDAP port)
- **fsGroup**: Sets the group ID for mounted volumes
- **seccompProfile.type: RuntimeDefault**: Applies the runtime's default seccomp profile
- **allowPrivilegeEscalation: false**: Prevents processes from gaining additional privileges
- **capabilities.drop: ["ALL"]**: Drops all Linux capabilities

### Understanding the Configuration

Kaniop merges user-provided container configurations with operator-managed containers. This means:

- The `kanidm` container in the `containers` list will be merged with the operator's kanidm container
- Init containers like `kanidm-generate-replication-config` can be customized similarly
- User-defined values override operator defaults when conflicts occur

For more information on container security best practices, see the
[Kubernetes documentation](https://kubernetes.io/docs/concepts/security/pod-security-standards/#restricted).

## Customizing Appearance

You can customize Kanidm's appearance by mounting a custom CSS file. Kanidm loads CSS from
`/hpkg/override.css`, allowing you to override default styles.

### Custom CSS Example

Create a ConfigMap with your custom CSS and mount it to the Kanidm pods:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: kanidm-custom-css
  namespace: default
data:
  override.css: |
    /* Custom styling */
    body {
      background-color: #f5f5f5;
    }
    .kanidm_header {
      background-color: #2c3e50;
      color: #ecf0f1;
    }
```

Then reference it in your Kanidm CR:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: Kanidm
metadata:
  name: my-idm
spec:
  domain: idm.example.com
  replicaGroups:
    - name: default
      replicas: 1
  volumes:
    - name: custom-css
      configMap:
        name: kanidm-custom-css
  volumeMounts:
    - name: custom-css
      mountPath: /hpkg/override.css
      subPath: override.css
```

See the complete example at
[`examples/kanidm-custom-css.yaml`](https://github.com/pando85/kaniop/blob/{{KANIOP_VERSION}}/examples/kanidm-custom-css.yaml).

For more information on customizing Kanidm, see the
[Kanidm documentation](https://kanidm.github.io/kanidm/stable/customising.html).
