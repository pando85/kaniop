---
title: Managing Kanidm Clusters
weight: 30100
indent: true
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

The operator tracks the content of this TLS secret: a hash of it is injected into the pod
template, so a certificate renewal (e.g. performed by cert-manager) automatically triggers a
rolling restart of the Kanidm pods. This is required because kanidmd only reads its TLS
material at startup. For the renewal to be detected immediately, the secret must be of type
`kubernetes.io/tls` (the default for cert-manager and `kubectl create secret tls`); other
secret types are only picked up on the next periodic reconciliation.

## Outgoing Email

Set `spec.mailSender` on a `Kanidm` resource to deploy a single
`kanidm-mail-sender` instance. Create a Kubernetes Secret in the same namespace with
`username` and `password` keys for your SMTP relay, then configure:

```yaml
spec:
  mailSender:
    relay: smtps://smtp.example.com:465
    credentialsSecret:
      name: smtp-credentials
    fromAddress: kanidm@example.com
    queuePollIntervalSeconds: 5
```

The poll interval is in **seconds** (5 by default). Kaniop configures a Kanidm service
account, its API token, the mail-sender configuration Secret, and the Deployment automatically.

### Trusting a private CA for the Kanidm endpoint

If the public Kanidm HTTPS endpoint uses a private PKI, put the PEM-encoded CA
certificate (or bundle) in a ConfigMap in the same namespace as the `Kanidm` resource.
For example:

```bash
kubectl -n identity create configmap kanidm-ca --from-file=ca.crt=./ca.crt
```

Then configure the mail sender to trust that CA:

```yaml
spec:
  mailSender:
    relay: smtps://smtp.example.com:465
    credentialsSecret:
      name: smtp-credentials
    fromAddress: kanidm@example.com
    caBundleRef:
      name: kanidm-ca
      key: ca.crt
```

Kaniop mounts this file read-only and sets `ca_path` in the mail sender's
Kanidm client configuration. Certificate and hostname verification remain enabled.
Without `caBundleRef`, the mail sender keeps its normal TLS trust configuration.

The CA reference applies to **mail sender → Kanidm HTTPS** connections, not the
separate SMTP relay's certificate. The ConfigMap is not managed by Kaniop; when
rotating its CA contents, restart the mail-sender Deployment to reload trust.

By default, Kaniop follows Kanidm's **Docker Hub release convention**:
`kanidm/server:<tag>` uses `kanidm/tools:<tag>`. The tag is taken from
`spec.image` (for example, `1.11.2`), but the server image's `@sha256:...`
digest is **never inherited** because the tools image has a different digest.

The default does not implement any GHCR-specific or registry-specific image
translation. Set `spec.mailSender.image` to **any desired image reference**
(including a private mirror or an independently pinned tools digest) to
override the derived image completely:

```yaml
spec:
  image: kanidm/server:1.11.2@sha256:<server-digest>
  mailSender:
    relay: smtps://smtp.example.com:465
    credentialsSecret:
      name: smtp-credentials
    fromAddress: kanidm@example.com
    image: kanidm/tools:1.11.2@sha256:<tools-digest>
```

When `spec.image` has only a digest without a version tag, Kaniop cannot
infer the tools release version; in that case `spec.mailSender.image` is
required.

A ready mail-sender Deployment can deliver messages already queued by Kanidm.
Kaniop's person controller currently creates credential reset links in Kubernetes
Events; it does **not** automatically send those links by email (see
[issue #1109](https://github.com/pando85/kaniop/issues/1109)).

### Image references across Kanidm components

Kanidm container images are not interchangeable, even when they share a version
tag. Kaniop handles them as follows:

| Component | Image reference | Digest and override behavior |
| --- | --- | --- |
| Kanidm server StatefulSet | `spec.image` (default `kanidm/server:latest`) | Used verbatim, including a pinned digest; an explicit `spec.containers` override for container `kanidm` takes precedence (advanced/unsupported customization). |
| Kanidm mail sender | `spec.mailSender.image`, otherwise `kanidm/tools:<server-tag>` | The derived image uses the server **tag only**, never its digest. The explicit mail sender image is used verbatim. |
| `KanidmRestore` restore/verify and safety-backup Jobs | `KanidmRestore.spec.restoreImage` | Used verbatim; for safety, the reference must match the target `Kanidm.spec.image` exactly and must be immutable (versioned or digest-pinned). |
| Backup transfer and discovery Jobs | Kaniop data-mover image | Independent of the Kanidm server and tools images. |
| Replication-config init container | Rash image | Independent of the Kanidm server and tools images; not derived from either. |

The Kanidm upgrade/version checks parse an image tag without its
`@sha256:...` digest or registry port. An image reference containing only
a digest has **no tag** to infer; this does not change the image supplied
directly to Kubernetes.

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
