---
title: RADIUS Servers
weight: 34000
indent: true
---

# RADIUS Servers

Kaniop deploys and operates Kanidm's RADIUS component (FreeRADIUS plus the Kanidm authentication
module) using the `KanidmRadiusServer` Custom Resource. One resource gives you one independently
managed RADIUS endpoint: a dedicated Kanidm service account, a read-only API token, a Deployment and
a UDP Service. You can run several resources against one Kanidm cluster, for example one per site
with different NAS devices, VLAN policy and Service exposure.

Kaniop manages the **server**. It does not provision NAS devices and does not create or reset users'
RADIUS passwords (those stay in Kanidm, see [Users and RADIUS passwords](#users-and-radius-passwords)).

## Prerequisites

- A `Kanidm` cluster managed by Kaniop, running a release whose `kanidm/radius` image Kaniop can
  pair with it (see [Version pairing](#version-pairing-with-the-radius-image)).
- The Kaniop release that includes the `KanidmRadiusServer` CRD. When upgrading, apply the chart's
  CRDs as described in [Installation](../installation.md#upgrading-kaniop).
- The `KanidmRadiusServer` namespace must be authorized by the `Kanidm` resource through
  `spec.radiusServerNamespaceSelector`. Without it only the Kanidm's own namespace is permitted:

  ```yaml
  apiVersion: kaniop.rs/v1beta1
  kind: Kanidm
  metadata:
    name: my-idm
    namespace: identity
  spec:
    domain: idm.example.com
    # Absent: only the namespace of this Kanidm. {}: every namespace.
    radiusServerNamespaceSelector:
      matchLabels:
        kubernetes.io/metadata.name: network
  ```

  A `KanidmRadiusServer` in a namespace that is not selected gets a `ResourceNotWatched` Warning
  event, `IdentityReady=False` with reason `NamespaceNotWatched`, and no finalizer. The selector is
  independent of `serviceAccountNamespaceSelector`.
- A Secret with one shared secret per NAS, a Secret with the EAP server certificate and key, and
  (for private PKI) a CA bundle for the Kanidm endpoint. All must be in the **same namespace** as
  the `KanidmRadiusServer`.
- Operator RBAC. The Helm chart's ClusterRole already grants what the controller needs: read access
  to Pods and ReplicaSets (to find which pods still use a token or config Secret), and management
  of Deployments, Services, Secrets and Events. If you maintain the operator's RBAC yourself, add
  `get`, `list` and `watch` on `apps/replicasets` when upgrading from a release without RADIUS support.
- RADIUS pods must be able to reach the Kanidm URL over HTTPS (see
  [How the Kanidm URL is derived](#how-the-kanidm-url-is-derived)).

## Example

The generated reference, with every option documented, is
[`examples/radius-server.yaml`](https://github.com/pando85/kaniop/blob/{{KANIOP_VERSION}}/examples/radius-server.yaml).
A complete working set, in the `network` namespace:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: nas-radius-secrets
  namespace: network
type: Opaque
stringData:
  # 16-255 characters of [A-Za-z0-9_-] only, for example:
  #   openssl rand -base64 24 | tr '+/' '-_' | tr -d '='
  mikrotik: REPLACE-WITH-A-GENERATED-SECRET
---
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: radius-eap-tls
  namespace: network
spec:
  secretName: radius-eap-tls
  dnsNames:
    - radius.example.com
  usages:
    - server auth
    - digital signature
    - key encipherment
  issuerRef:
    name: internal-ca
    kind: ClusterIssuer
---
apiVersion: kaniop.rs/v1beta1
kind: KanidmRadiusServer
metadata:
  name: campus-radius
  namespace: network
spec:
  kanidmRef:
    name: my-idm
    namespace: identity
  replicas: 2
  authorization:
    requiredGroups:
      - radius_access_allowed@idm.example.com
    defaultVlan: 10
    groupVlans:
      - group: staff@idm.example.com
        vlan: 20
  clients:
    - name: mikrotik
      ipaddr: 192.0.2.10/32
      sharedSecretRef:
        name: nas-radius-secrets
        key: mikrotik
  radiusTls:
    secretRef:
      name: radius-eap-tls
    caKey: ca.crt
  kanidmTls:
    caBundleRef:
      kind: ConfigMap
      name: kanidm-ca
      key: ca.crt
  service:
    type: LoadBalancer
    port: 1812
    externalTrafficPolicy: Local
  tokenRotation:
    enabled: true
    periodDays: 90
```

Check progress with:

```bash
kubectl get kanidmradiusservers -n network   # short name: krs
kubectl describe krs campus-radius -n network
```

The `Ready` column is true when `IdentityReady`, `ConfigurationReady`, `Available` and `Exposed` are
all `True`; the `Endpoint` column shows the address NAS devices should use.

Naming and limits:

- The resource name must be at most 52 characters (the generated config Secret is
  `<name>-<10 hex characters>`).
- The Deployment and the Service are both named after the resource.
- `spec.kanidmRef` is immutable. To move a server to another Kanidm, delete and create it.
- `replicas` defaults to 2 (range 1 to 100). Rollouts use `maxSurge: 1` and `maxUnavailable: 0`.

## Users and RADIUS passwords

Kaniop does not manage user credentials. Every person who authenticates over RADIUS needs a RADIUS
secret generated in Kanidm, separate from their normal password, and membership in one of the
`requiredGroups`:

```bash
kanidm person radius generate-secret alice
```

## NAS clients and shared secrets

Each entry in `spec.clients` lets one NAS (or one network) talk to the server.

| Field | Meaning |
|-------|---------|
| `name` | FreeRADIUS client name, `A-Za-z0-9_-`, 1 to 63 characters, unique. |
| `ipaddr` | IPv4 or IPv6 address, or a CIDR with a non-zero prefix. `192.0.2.10` and `192.0.2.10/32` are the same client. Host bits of a CIDR are masked. |
| `sharedSecretRef` | `name` and `key` of a Secret in the same namespace. |

Rules enforced when the inputs are read:

- The shared secret is **never** in the resource. The referenced value must be 16 to 255 characters
  from `A-Za-z0-9_-`. The upstream image writes it unquoted into the FreeRADIUS `clients.conf`, so
  whitespace, quotes, `#`, braces and similar characters are rejected rather than escaped. Configure
  the same value on the NAS device.
- Client names must be unique, and so must the resulting networks (duplicates are rejected, not
  merged).
- A request from an address that matches no client is dropped by FreeRADIUS. When the NAS reports
  authentication timeouts, first check that the **source address the pod sees** matches `ipaddr`
  (see [Service exposure](#service-exposure-and-source-addresses)).

Rotating a NAS secret means updating the Secret. Kaniop watches the referenced Secrets, renders a new
config Secret and rolls the Deployment. No API token is minted or revoked by that change. Update the
NAS device at the same time, since in-flight devices use the old secret until they are changed.

### Message-Authenticator (BlastRADIUS)

Configure every NAS to send the `Message-Authenticator` attribute in all Access-Request packets.
This is the mitigation for the BlastRADIUS attack on RADIUS/UDP (CVE-2024-3596), and the server
enforces it:

- The upstream `kanidm/radius` image writes `require_message_authenticator = yes` into every
  generated FreeRADIUS `client` block. This was checked against the `kanidm/radius:1.11.2` image:
  running its bootstrap on a one-client `radius.toml` produced that line in the client block.
  FreeRADIUS discards an Access-Request from such a client that has no `Message-Authenticator`.
  Nothing is sent back, so the NAS sees a timeout, not a reject.
- Kaniop has no setting to relax this, and the image does not write `limit_proxy_state` per client.
  The per-client `yes` overrides the FreeRADIUS global default (`auto`). Kaniop does not change the
  image's behaviour, so it follows whatever the image version you run does.
- EAP exchanges carry `Message-Authenticator` by specification, so EAP-TLS and PEAP devices normally
  comply. Check older firmware and PAP/CHAP-only devices, which may not send it. Update the firmware
  or the device's RADIUS settings rather than looking for a way to turn the check off.

This protects requests reaching the server. Keep NAS-to-server traffic on a trusted network segment
and use a long, random shared secret, because UDP RADIUS has no transport protection of its own.

An illustrative MikroTik RouterOS client definition (adapt to your device and EAP setup):

```text
/radius add service=wireless address=<service address> secret=<shared secret> authentication-port=1812
```

## Authorization and VLANs

```yaml
spec:
  authorization:
    requiredGroups:
      - radius_access_allowed@idm.example.com
    defaultVlan: 10
    groupVlans:
      - group: staff@idm.example.com
        vlan: 20
```

- `requiredGroups` (1 to 100 entries, required): Kanidm group SPNs (`name@domain`). A user must be a
  member of **at least one** of them to be accepted. The list cannot be empty, which prevents a
  server that accepts every Kanidm user by accident.
- `defaultVlan` (default 1, range 1 to 4094): VLAN returned to permitted users that match no entry
  in `groupVlans`.
- `groupVlans` (up to 100 entries): maps a group to a VLAN (1 to 4094). It is independent of
  `requiredGroups`: a group mapped here does not by itself grant access. Avoid overlapping mappings
  for the same user, since the choice between them is made by the upstream module.

Changing authorization rolls the Deployment (new config Secret, same token).

## EAP TLS with cert-manager

`spec.radiusTls` points at a Secret with the certificate chain and private key FreeRADIUS presents
to supplicants. The upstream image requires them, and Kaniop does not issue certificates.

| Field | Default | Meaning |
|-------|---------|---------|
| `secretRef.name` | required | Same-namespace Secret. |
| `certKey` | `tls.crt` | Key with the PEM certificate chain. |
| `privateKeyKey` | `tls.key` | Key with the matching PEM private key. |
| `caKey` | none | Optional key in the same Secret with the CA that supplicants are expected to trust. |

A cert-manager `Certificate` writes exactly these keys, plus `ca.crt` when the issuer provides one,
so the example above only adds `caKey: ca.crt`. Use a certificate that your supplicants will
validate (typically its DNS name is the RADIUS server name configured in the supplicant profile).

Before any change is applied, Kaniop checks that the chain parses, the key matches the certificate,
and the certificate is currently valid. An invalid or expired certificate reports
`ConfigurationReady=False` with reason `InvalidTlsMaterial`; the running pods and token are left
untouched.

When cert-manager renews the certificate, Kaniop notices the Secret change and rolls the Deployment
(a digest of the public certificate and CA bytes is part of the pod template). No token is minted or
revoked. The private key is read to validate it and is passed to the pods through the Secret
mount; it is never copied into status or events.

## Kanidm CA trust

If Kanidm's HTTPS endpoint uses a private CA, point the RADIUS pods at the CA bundle:

```yaml
spec:
  kanidmTls:
    caBundleRef:
      kind: ConfigMap   # or Secret; default ConfigMap
      name: kanidm-ca
      key: ca.crt
```

The ConfigMap or Secret must be in the **namespace of the `KanidmRadiusServer`**, not the Kanidm
namespace. Certificate and hostname verification are always on; there is no option to disable
them. Without `caBundleRef` the pods use the image's system trust store. A missing object reports
`KanidmCaNotFound`, an unreadable or empty bundle `KanidmCaInvalid`.

### How the Kanidm URL is derived

There is no URL field on `KanidmRadiusServer`. Pods connect to the referenced Kanidm's
`spec.origin` when set, otherwise `https://<spec.domain>`. Consequences:

- The name in that URL must resolve from inside the RADIUS pods and be covered by Kanidm's
  certificate. A placeholder domain such as `my-idm.localhost` (used in the generated example)
  will not work from another pod.
- For split-horizon or non-default-port setups, express them on the `Kanidm` resource: set
  `spec.origin` (for example `https://idm.example.com:8443`) or make DNS inside the cluster resolve
  the Kanidm domain to the Kanidm Service. Note that `origin` is also Kanidm's WebAuthn origin, so
  choose it deliberately.

## Service exposure and source addresses

```yaml
spec:
  service:
    type: ClusterIP        # ClusterIP (default), NodePort or LoadBalancer
    port: 1812             # Service port, default 1812 (the container always listens on 1812)
    externalTrafficPolicy: Local   # NodePort and LoadBalancer only; rejected on ClusterIP
    annotations: {}        # e.g. cloud or MetalLB load balancer annotations
```

The Service is **UDP**. Make sure your load balancer implementation supports UDP, and that any
NetworkPolicy or firewall allows UDP to the pods on 1812 from your NAS devices and egress from the
pods to Kanidm over HTTPS.

| Type | Use it when | `status.endpoint` |
|------|-------------|-------------------|
| `ClusterIP` | The NAS is inside the cluster network. The CRD rejects `externalTrafficPolicy` on a `ClusterIP` Service (as Kubernetes does), and the controller reports `InvalidSpec` if one gets through. | `<clusterIP>:<port>` |
| `NodePort` | You front the cluster with your own load balancer or address any node. | `*:<nodePort>` (any node) |
| `LoadBalancer` | Your platform assigns an external address. | `<address>:<port>` once assigned |

FreeRADIUS authenticates a NAS by the **source IP of the packet** and uses the shared secret of the
client that matches it. This makes `externalTrafficPolicy` important for NAS devices outside the
cluster:

- `Cluster` (the Kubernetes default when the field is omitted): the node that receives the packet may
  forward it to a pod on another node and rewrites the source address (SNAT). The pod then sees a
  node address, not the NAS. Per-NAS `ipaddr` entries no longer match and requests are dropped. It
  only works if your `clients` list the node or SNAT addresses (which collapses every NAS into one
  client and one shared secret) or one CIDR that covers them.
- `Local`: the source address is preserved and traffic is delivered only to pods on the receiving
  node. Nodes without a ready pod must not receive traffic: load balancer health checks do this on
  clouds that support it, but with MetalLB or `NodePort` you must ensure every node that can
  receive traffic runs a pod, using `replicas`, `topologySpreadConstraints` or `affinity`.

Prefer `Local` and exact `ipaddr` entries when you have per-NAS secrets. Check the actual source
address in the pod logs if a NAS is silently ignored.

## Pod lifecycle and rollouts

RADIUS runs over UDP, so a NAS that sends to a pod that is already gone gets no answer and retries
later. The pods are built so that rotations and rollouts do not cause that:

- A `preStop` hook sleeps for **10 seconds** before `radiusd` receives `SIGTERM`. A terminating pod
  keeps answering UDP while the endpoint removal propagates to EndpointSlices and kube-proxy (or
  your CNI), so NAS retries are served instead of dropped.
- `terminationGracePeriodSeconds` is **30**, longer than the hook, so `radiusd` is stopped cleanly
  and not killed during the sleep.
- Rollouts use `maxSurge: 1` and `maxUnavailable: 0`: an old pod is removed only after a new pod is
  ready. Token rotation, NAS secret changes and configuration changes all roll this way.

Neither value is configurable. A pod takes about 10 seconds longer to terminate, so a rollout of N
replicas takes correspondingly longer, and so does deletion (see
[Deleting a server](#deleting-a-server)). Keep `replicas` at 2 or more if you need uninterrupted
service during a rollout or a node drain.

## Security model

- **Dedicated identity per resource.** Each `KanidmRadiusServer` owns one Kanidm service account
  named `kaniop_radius_<uid>` where `<uid>` is the resource's Kubernetes UID without hyphens. The
  display name is the ownership marker `Kaniop RADIUS: <namespace>/<name> [<uid>]`. Re-creating a
  resource with the same name produces a new UID and therefore a **new** account; the old account is
  never adopted.
- **Additive group membership.** The account is added to the built-in `idm_radius_servers` group
  with a single member edge. Kaniop never calls set or purge on that group, so other RADIUS servers
  and unrelated members are untouched. Do not manage `idm_radius_servers` through a
  `KanidmGroup` with `members`, since that is authoritative and would remove the Kaniop accounts.
- **Read-only API token.** The pods authenticate to Kanidm with a read-only token. It is not mounted
  as a Kubernetes service account token (`automountServiceAccountToken` is off).
- **Where the secrets live.** The token and the NAS shared secrets are rendered into one Kubernetes
  Secret per configuration. They are never in the resource, `status`, events or operator logs;
  status carries token **ids** only. Restrict `get` on Secrets in that namespace accordingly.
- **Encryption at rest and RBAC.** The per-resource config Secret holds the Kanidm API token and
  the NAS shared secrets in clear text. Anyone who can read it (through the API, an etcd backup or a
  node) can authenticate to Kanidm as the RADIUS account and impersonate your NAS devices. Enable
  [Kubernetes encryption at rest for Secrets](https://kubernetes.io/docs/tasks/administer-cluster/encrypt-data/)
  (or a KMS provider), protect etcd backups, and grant `get`, `list` and `watch` on Secrets in the
  namespace only to the operator and to people who need them (`list` and `watch` return Secret
  contents too). Kaniop cannot enforce any of this.
- **Immutable, content-addressed config Secrets.** The config Secret is named
  `<name>-<first 10 hex characters of the digest of its content>`, is created once and is never
  updated. A configuration change creates a new Secret and rolls the Deployment; the old Secret is
  garbage-collected only when no pod, ReplicaSet or template references it.
- **Foreign objects are not taken over.** If an object with a name Kaniop needs (Deployment, Service,
  config Secret, Kanidm account) exists and is not controlled by this resource, Kaniop reports
  `ForeignObject`, `SecretConflict` or `OwnershipConflict` and leaves it alone.
- **Pod hardening.** Pods run as UID 497 and GID 496 (the user of the verified image), non-root,
  with all capabilities dropped, privilege escalation disabled and the `RuntimeDefault` seccomp
  profile. The projected configuration volume is mounted read-only with mode 0440.

### Why the root filesystem stays writable

`readOnlyRootFilesystem` is intentionally **not** enabled. The upstream image bootstrap rewrites
`/etc/raddb/clients.conf` and writes `/etc/raddb/certs/{ca,server}.pem` when it starts. A tmpfs
over `/etc/raddb/certs` is not enough because `clients.conf` is a file in `/etc/raddb`. Making
`/etc/raddb` writable from an emptyDir needs an init container to copy it, and `radiusd` then refuses
to start because the directory is globally writable (0777 mount point) unless a root init container
fixes its mode. That trade is worse than a writable container layer. Revisit this only if upstream
stops writing under `/etc/raddb`.

## Token lifecycle

The API token is derived from what Kanidm and Kubernetes actually contain on every reconcile, not
from remembered state, so losing `status` or a Secret never wedges the resource.

1. **Mint.** A new token is created when the Deployment template has none (`Initial`), when the
   token in use cannot be read or no longer exists in Kanidm (`BearerLost`), when it expires within
   one day (`Expired`), when it is older than `periodDays` (`Due`), or when a manual rotation is
   requested (`Requested`).
2. **Verify before rollout.** The new token must pass a `whoami` call against Kanidm (retried briefly
   for replica lag). A token that is rejected is never written to a Secret or pod; the condition
   `Rotating` shows `CandidateRejected` and the existing token keeps serving. This proves the token
   is valid, not that a RADIUS login works: the readiness probe is `radiusd -C`, a configuration
   check. The rolling update with `maxUnavailable: 0` keeps the old pods serving until new pods are
   ready.
3. **Roll out.** A new config Secret holds the new token and the Deployment template points to it.
4. **Revoke only when unused.** The old token is revoked only after no pod (including terminating
   ones), ReplicaSet or the template references it, **and** it is more than 10 minutes old (grace
   for a crashed or in-flight reconcile). If Kaniop cannot work out which token some consumer
   holds, it revokes nothing until that is resolved (`ConsumerUnresolved`). Tokens kept this way are
   listed in `status.retiringTokenIds`.
5. **Bound.** The account never holds more than 4 tokens. If a new token is needed while 4 exist,
   Kaniop reports `TooManyTokens` and keeps revoking unused tokens until it can proceed.

An unreachable Kanidm never causes a revocation.

### Rotation settings

```yaml
spec:
  tokenRotation:
    enabled: true     # default false
    periodDays: 90    # 1 to 365, default 90
```

- With `enabled: false` (the default), the token never expires and is replaced only for the other
  reasons above.
- With `enabled: true`, new tokens expire after **2 x `periodDays`** (180 days with the default of
  90) and are replaced after `periodDays`, which leaves a full period for a failing rotation to be
  noticed and fixed before the token lapses.
- `tokenRotation` uses the same `enabled`/`periodDays` type as the other Kaniop resources. The CRD
  does not bound `periodDays`; Kaniop rejects a value outside 1 to 365 with `InvalidSpec`.

### Manual rotation

Set the annotation `kaniop.rs/radius-rotate-token` to any new non-empty value:

```bash
kubectl annotate krs campus-radius -n network \
  kaniop.rs/radius-rotate-token="$(date +%s)" --overwrite
```

A rotation happens when the value differs from the one recorded with the current token. Setting the
same value again, or removing the annotation afterwards, does not rotate again. Use a new value for
each rotation. If a token may have leaked, rotate and then follow the
[break-glass runbook](#break-glass-runbook) to destroy the old tokens immediately instead of waiting
for the unused-token revocation.

## Status and conditions

All five conditions are always present.

| Condition | `True` when |
|-----------|-------------|
| `IdentityReady` | The Kanidm account exists, is owned by this resource and is in `idm_radius_servers`. |
| `ConfigurationReady` | Every referenced Secret, CA and certificate is valid and the workload could be rendered. |
| `Available` | The rollout is complete and all desired replicas are available. |
| `Exposed` | The Service has an address (cluster IP, node port or load balancer address). |
| `Rotating` | A token change is in progress, or an old token is being kept. `False` (`TokenCurrent`) is the steady state. It does not affect `status.ready`. |

Other status fields: `endpoint`, `tokenId`, `tokenIssuedAt`, `configSecretName`, `deploymentName`,
`serviceName`, `replicas`, `readyReplicas`, `retiringTokenIds`, `observedGeneration`. A condition
that was not reached in the last pass is `Unknown` with reason `NotEvaluated`. Transient API errors
(Kubernetes or Kanidm) appear as `Unknown`/`TransientError` and are retried with backoff.

Reconciliation pauses (without status changes) while a Kanidm restore is in progress.

### Troubleshooting by reason

```bash
kubectl get krs <name> -n <ns> -o jsonpath='{range .status.conditions[*]}{.type}{"\t"}{.status}{"\t"}{.reason}{"\t"}{.message}{"\n"}{end}'
```

| Condition | Reason | Meaning and fix |
|-----------|--------|-----------------|
| `IdentityReady` | `KanidmNotFound` | `spec.kanidmRef` names no Kanidm. Fix the reference (it is immutable: recreate the resource). |
| `IdentityReady` | `NamespaceNotWatched` | The Kanidm's `radiusServerNamespaceSelector` does not select this namespace. Update the Kanidm. Running pods keep their token but it is not rotated while the namespace is not selected. |
| `IdentityReady` | `InvalidIdentity` | The resource lacks a namespace, name or UID. Recreate it. |
| `IdentityReady` | `OwnershipConflict` | A Kanidm account named `kaniop_radius_<uid>` exists without this resource's marker. It is not touched. Investigate who created it. |
| `IdentityReady` | `AccountReady` | Healthy. |
| `ConfigurationReady` | `InvalidSpec` | A spec value is out of range, or `externalTrafficPolicy` is set on a `ClusterIP` Service (the CRD already rejects this at admission). |
| `ConfigurationReady` | `NasSecretNotFound`, `NasSecretKeyInvalid` | A `sharedSecretRef` Secret or key is missing or not UTF-8. |
| `ConfigurationReady` | `TlsSecretNotFound`, `TlsSecretKeyMissing` | The EAP certificate Secret or one of its keys is missing. |
| `ConfigurationReady` | `InvalidTlsMaterial` | Certificate or key unparsable, mismatched, not yet valid or expired. Check the cert-manager `Certificate`. |
| `ConfigurationReady` | `KanidmCaNotFound`, `KanidmCaInvalid` | The CA ConfigMap or Secret is missing, or has no valid certificates. |
| `ConfigurationReady` | `InvalidConfiguration` | A shared secret, client name, address, group or VLAN is invalid (see [NAS clients](#nas-clients-and-shared-secrets)). |
| `ConfigurationReady` | `ImageUnresolved` | The RADIUS image cannot be derived. Set `spec.image`. A server that already runs keeps its current image, so token rotation goes on meanwhile. |
| `ConfigurationReady` | `InvalidWorkload` | The Deployment or Service could not be built from the spec. |
| `ConfigurationReady` | `SecretConflict` | A config Secret with the same name but different content exists. Delete the stray Secret. |
| `ConfigurationReady` | `ConfigurationValid` | Healthy. |
| `Available` | `DeploymentMissing`, `RolloutInProgress`, `ReplicasUnavailable` | Rolling out or pods not ready. Check `kubectl describe pods -l kaniop.rs/radius-uid=<uid>`. |
| `Available` | `ProgressDeadlineExceeded` | The rollout is stuck: image pull, missing Secret mounts, or scheduling. |
| `Available` | `ForeignObject` | A Deployment with this name exists and is not controlled by this resource. |
| `Available` | `RolloutComplete` | Healthy. |
| `Exposed` | `ServiceMissing`, `ClusterIPPending` | The Service has not been created or assigned an IP yet. |
| `Exposed` | `LoadBalancerPending` | The platform has not assigned an address. Check the cloud or MetalLB controller. |
| `Exposed` | `NodePortPending` | No node port assigned yet. |
| `Exposed` | `ForeignObject` | A Service with this name exists and is not controlled by this resource. |
| `Exposed` | `ClusterIPAssigned`, `NodePortAssigned`, `LoadBalancerReady` | Healthy. |
| `Rotating` | `CandidateRejected` | Kanidm rejected the new token (`whoami`). The old token still serves; retried every 30 s. Check Kanidm health and replica lag. |
| `Rotating` | `MintedTokenNotFound` | The new token could not be identified on the account. It is cleaned up as an orphan after 10 minutes. |
| `Rotating`, `ConfigurationReady` | `ConcurrentMutation` | An object changed while it was being modified (a retired config Secret, or the new one). Retried within seconds. |
| `Rotating` | `TooManyTokens` | The account holds 4 tokens and none is unused yet. Retried every minute; clears itself as old tokens are revoked. |
| `Rotating` | `BearerUnreadable` | The planner chose to keep the current token but the config Secret the Deployment mounts has no readable token at that moment (a race with a Secret change). Retried on the next pass; if it persists, inspect the Secret named in `status.configSecretName`. |
| `Rotating` | `ConsumerUnresolved` | Kaniop cannot tell which token a pod or ReplicaSet uses, so it holds all revocation. Look for a pod mounting a deleted config Secret or carrying a bad `kaniop.rs/radius-token-id` annotation. |
| `Rotating` | `Initial`, `BearerLost`, `Expired`, `Due`, `Requested`, `RetiringTokens` | A rotation just happened (reason: why) or old tokens are still in use. Normal. |
| `Rotating` | `TokenCurrent` | Steady state. |
| any | `MissingMetadata` | An object lacks a name, namespace, UID or resourceVersion Kaniop needs. Reported on the condition of the step that failed. |
| any | `TransientError` | A Kubernetes or Kanidm call failed; see the message and operator logs. |
| any | `NotEvaluated` | An earlier step failed first. Fix the first non-healthy condition. |

Other useful checks:

```bash
kubectl get events -n <ns> --field-selector involvedObject.name=<name>
kubectl logs deployment/<name> -n <ns>
kubectl logs deployment/kaniop -n kaniop | grep -i radius
```

A failed input never disturbs a running endpoint: pods and token stay as they were until the input is
valid again.

While a block stops reconciliation before the Deployment is updated (an invalid input,
`NamespaceNotWatched`, `KanidmNotFound`, `TooManyTokens` and the like) the token cannot be rotated. When the token the pods use expires
within two days (twice the one-day safety margin), each blocked pass emits a `TokenNearExpiry`
Warning event naming the token, its expiry and the block; clear the block before the token lapses.
If the last pass did not read the token from Kanidm, the expiry is estimated from
`status.tokenIssuedAt` and the current `tokenRotation` (`issued + 2 x periodDays`).

## Version pairing with the RADIUS image

The RADIUS image is released separately from `kanidm/server` and a server digest is not a RADIUS
digest. Kaniop therefore infers the image only in one case: when the referenced Kanidm runs
`kanidm/server:1.11.2` from Docker Hub (also spelled `docker.io/` or `index.docker.io/`), the RADIUS
image is `kanidm/radius:1.11.2`. That is the only release whose runtime user (UID 497, GID 496) and
bootstrap have been verified. For any other tag, a digest, or another registry, set the image
explicitly or the resource reports `ImageUnresolved` (a server that already runs keeps its current
image until the image is fixed):

```yaml
spec:
  image: registry.example.com/kanidm/radius:1.11.2
```

An explicit image is used as written. Keep the RADIUS image on the same release as Kanidm, and
verify the UID and GID of any other release before using it.

## Migrating from a manual RADIUS deployment

Kaniop never adopts existing accounts, tokens or Deployments. Migrate side by side:

1. Create the `KanidmRadiusServer` with a **different** Service address from the old server (another
   load balancer address or port). It creates its own account and token, so there is nothing to
   clean up in the old deployment.
2. Wait for `Ready`, then point one test NAS at the new endpoint and verify authentication, VLAN
   assignment and denial of a user outside `requiredGroups`.
3. Move the remaining NAS devices (or move the address) one by one.
4. Retire the old deployment, and manually destroy its API token and remove its account from
   `idm_radius_servers` in Kanidm. Kaniop does not know about them.

Both deployments can use `idm_radius_servers` at the same time, because Kaniop only adds and removes
its own account.

## Deleting a server

On deletion the finalizer: deletes the Deployment and waits for its pods to terminate, waits until
no pod or ReplicaSet uses the config Secrets or tokens, removes the account's membership edge,
deletes the account (which destroys all its tokens), and finally lets Kubernetes garbage-collect the
Service and config Secrets. External Secrets (NAS, TLS, CA) are never deleted.

While deletion waits for pods or ReplicaSets, the resource gets an `AwaitingConsumers` Warning event
naming them (for example `pod/campus-7d9f-abcde`), when the set changes and at most every 10 minutes
otherwise. A pod that is not part of the Deployment but mounts a config Secret (a debug pod, a
copy) blocks deletion until it is removed.

Two situations skip the Kanidm part and still remove the finalizer, with a Warning event so the
leftover can be cleaned up by hand:

- `KanidmNotFound`: the referenced `Kanidm` no longer exists, so its RADIUS account could not be
  cleaned up.
- `AccountNotOwned`: the account exists without this resource's marker and is left untouched.

A Kanidm that exists but is unreachable is retried with backoff, not skipped.

## Break-glass runbook

Use this only when the normal finalizer cannot finish and you need the resource gone, or when you
must cut off a RADIUS token immediately. Normal deletion needs none of it.

> **Warning.** Members of `idm_radius_servers` can read users' RADIUS credentials in Kanidm. Clearing
> the finalizer **before** the account is deleted leaves a live account with that access, and any
> tokens it still has, in Kanidm with nothing left to clean it up. Clear the finalizer last.

You need a Kanidm account allowed to manage service accounts and the group, such as `idm_admin`. Its
password is in the `<kanidm>-admin-passwords` Secret created by Kaniop (see
[Quickstart](../quickstart.md#admin-access)). The commands below use the `kanidm` CLI; check
`kanidm <command> --help` for your version.

1. **Identify the account.** The name comes from the resource's UID:

   ```bash
   UID_=$(kubectl get krs campus-radius -n network -o jsonpath='{.metadata.uid}')
   ACCOUNT="kaniop_radius_${UID_//-/}"
   kanidm login -H https://idm.example.com -D idm_admin
   kanidm service-account get "$ACCOUNT"
   ```

   If the resource is already gone, list candidates with
   `kanidm service-account list | grep kaniop_radius_`.

2. **Verify the marker.** The `displayname` must be exactly
   `Kaniop RADIUS: <namespace>/<name> [<uid>]` with the UID with hyphens. If it is not, or the
   account does not exist, the account is not ours: skip steps 4 to 6 and go to step 7 (this is the
   `AccountNotOwned` case, and Kaniop would not have touched it either).

3. **Stop the pods.** Otherwise they keep serving with tokens you are about to destroy, and fail
   once Kanidm rejects them:

   ```bash
   kubectl delete deployment campus-radius -n network --wait
   ```

   If the operator is running and the resource is not being deleted, it recreates the Deployment.
   Delete the resource first (it will stay in `Terminating` on its finalizer) or scale the operator
   down.

4. **List and destroy the tokens.** Destroying the token ids is enough to cut access even if you stop
   here (for example when a token leaked):

   ```bash
   kanidm service-account api-token status "$ACCOUNT"
   kanidm service-account api-token destroy "$ACCOUNT" <token-id>
   ```

5. **Remove the membership edge.** Remove only this account. Never use `set-members` or
   `purge-members` on this group:

   ```bash
   kanidm group remove-members idm_radius_servers "$ACCOUNT"
   ```

6. **Delete the account.** This also removes any token that is left:

   ```bash
   kanidm service-account delete "$ACCOUNT"
   ```

7. **Clear the finalizer.** Only after the account is deleted or confirmed not ours:

   ```bash
   kubectl patch krs campus-radius -n network --type=merge \
     -p '{"metadata":{"finalizers":null}}'
   ```

   This removes every finalizer on the resource. If other controllers add finalizers you want to
   keep, patch by index with a JSON patch (`remove` on `/metadata/finalizers/<index>`) after
   checking `kubectl get krs campus-radius -n network -o jsonpath='{.metadata.finalizers}'`.
   The finalizer Kaniop uses is `kanidmradiusservers.kaniop.rs/finalizer`.

When it is safe to clear the finalizer: the account is deleted, or the marker check showed it is not
this resource's; the Deployment is gone, so no pod depends on the tokens; and you accept that the
Kubernetes children are garbage-collected afterwards (Service and config Secrets). If the Kanidm is
merely unreachable, prefer to wait: the finalizer retries on its own and cleans up properly once it
returns.
