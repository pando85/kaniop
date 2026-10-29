# Specification: Kanidm RADIUS servers (#1119)

Derived from: [intent.md](intent.md). Status: **proposed; not implemented**.

## Evidence and architecture rationale

This design follows:
- Kaniop `libs/operator/src/kanidm/reconcile/mail_sender.rs`: companion Deployment, internally created service account, additive built-in group membership, generated token/config Secret, CA hashing, cleanup.
- Kaniop `libs/group/src/reconcile.rs` and `libs/group/src/crd.rs`: `KanidmGroup.spec.members` is **authoritative** (`idm_group_set_members`), so it must NOT own `idm_radius_servers`.
- Kaniop `libs/service-account/src/crd.rs`: API tokens support read-only purpose, but its current rotation destroys/recreates tokens, unsuitable for an availability-sensitive active RADIUS workload.
- Kaniop `libs/operator/src/crd.rs`: shared immutable `KanidmRef` with namespace support; namespace-selector authorization is part of the controller contract.
- Kanidm `rlm_kanidm/shared/src/config.rs` and `examples/radius.toml`: `uri`, `auth_token`, `ca_path`, `radius_required_groups`, `radius_default_vlan`, `radius_groups`, `radius_clients`, `radius_cert_path`, `radius_key_path` and `radius_ca_path`.
- Kanidm `rlm_kanidm/entrypoint/src/bootstrap.rs`: loads `/data/radius.toml` once, writes FreeRADIUS `clients.conf` and copies/concatenates server certificate and key; no automatic live reload. **Server cert and key files are operationally required** by `prepare_certs`, even when config fields have default paths.
- CloudNativePG `Pooler`: independent companion CR and 0..N deployments; Strimzi embedded `entityOperator` is a more tightly coupled singleton.
- Vault Secrets Operator/cert-manager: Secret update must cause workload rollout when consumer cannot reload.
- Kubernetes: source-IP preservation on external Services, Secret and RBAC hardening.

Sources: [#1119](https://github.com/pando85/kaniop/issues/1119), [#433](https://github.com/pando85/kaniop/issues/433), [mail_sender.rs](https://github.com/pando85/kaniop/blob/master/libs/operator/src/kanidm/reconcile/mail_sender.rs), [group reconciler](https://github.com/pando85/kaniop/blob/master/libs/group/src/reconcile.rs), [upstream config](https://github.com/kanidm/kanidm/blob/master/rlm_kanidm/shared/src/config.rs), [upstream bootstrap](https://github.com/kanidm/kanidm/blob/master/rlm_kanidm/entrypoint/src/bootstrap.rs), [Kanidm RADIUS guide](https://kanidm.github.io/kanidm/master/integrations/radius.html), [CloudNativePG Pooler](https://cloudnative-pg.io/docs/devel/connection_pooling/), [Kubernetes source IP](https://kubernetes.io/docs/tutorials/services/source-ip/), [Secrets guidance](https://kubernetes.io/docs/concepts/security/secrets-good-practices/).

## Design decision

Introduce one namespaced `KanidmRadiusServer` CRD in `kaniop.rs/v1beta1`, managed by its own kube-rs controller. Model a RADIUS edge, not a singleton property on `Kanidm`. One Kanidm may serve multiple RADIUS CRs with different NAS sets, VLAN policies, namespace placement and Service configuration. Reuse common helpers from the mail-sender implementation **only where doing so preserves its behavior**; avoid speculative global refactors.

Alternatives:
1. `Kanidm.spec.radius` is less scaffolding, but couples an external network service to the Kanidm lifecycle and blocks natural multi-instance use. Rejected.
2. `KanidmServiceAccount` plus hand-written Deployment/Helm keeps configuration split across controllers and makes safe overlapping rotations, built-in group membership and finalizers user responsibilities. Rejected as fulfillment of #1119; remains a valid manual alternative.
3. `KanidmRadiusClient`, generic `KanidmGroupMembership` and Kaniop-managed `Certificate` are deferred until a separate use case warrants public API proliferation.

## Required behavior (MUST)

### API surface

- Namespaced `KanidmRadiusServer` with immutable `spec.kanidmRef` (reuse existing `KanidmRef`). Resolve cross-namespace references through the repository's Kanidm namespace-selector authorization model, not a bypass.
- New dedicated `Kanidm.spec.radiusServerNamespaceSelector`, if required by the existing `KanidmResource` trait, with a restrictive default matching existing resource conventions. Changing a selector must de-authorize subsequent RADIUS reconciliation safely.
- Typed, documented fields: `spec.image` (optional inference), `replicas`, `authorization.requiredGroups`, `authorization.defaultVlan`, `authorization.groupVlans[]`, `clients[]` with `name`, `ipaddr`, `sharedSecretRef.name/key`, `kanidmTls.caBundleRef`, `radiusTls.secretRef` with configurable keys, `service.type/port/externalTrafficPolicy/annotations`, bounded pod scheduling/resources, and `tokenRotation`.
- Require nonempty `clients`, nonempty NAS Secret value, an explicit TLS Secret with valid cert+key data, and at least one required authorization group by default (deny unintended unrestricted access); make any intentional allow-all policy opt-in, if ever introduced, with documented security review.
- In `radius_groups`, map only supported typed attributes: required group SPNs, optional VLAN, and no arbitrary FreeRADIUS/`reply_attributes` injection in MVP.
- Use `SecretKeySelector`-shaped, same-namespace input references for shared secrets and certificates; CA input may be ConfigMap or Secret, with explicit kind. Never embed secret bytes in the CR.
- Provide CRD/CEL validation for required fields, immutability, DNS names, IP/CIDR (plus runtime validation), UDP Service restrictions and VLAN range (1..4094 unless upstream use of 0 is explicitly tested). Client names/addresses unique; avoid clients.conf injection from client names or secrets.
- Avoid raw `radius.toml` or arbitrary command/container overrides. Adopt existing Kaniop metadata templates and pod configuration patterns where sensible.

Proposed manifest (not a supported API yet):

~~~yaml
apiVersion: v1
kind: Secret
metadata:
  name: nas-shared-secrets
  namespace: network-auth
type: Opaque
stringData:
  mikrotik: replace-with-strong-random-secret
---
apiVersion: v1
kind: Secret
metadata:
  name: radius-eap-tls
  namespace: network-auth
type: kubernetes.io/tls
stringData:
  tls.crt: "<PEM certificate chain>"
  tls.key: "<PEM private key>"
  ca.crt: "<EAP trust CA, if needed>"
---
apiVersion: kaniop.rs/v1beta1
kind: KanidmRadiusServer
metadata:
  name: campus-radius
  namespace: network-auth
spec:
  kanidmRef:
    name: idm
    namespace: identity
  # For digest-pinned parent image, set image explicitly.
  image: kanidm/radius:1.11.2
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
        name: nas-shared-secrets
        key: mikrotik
  kanidmTls:
    caBundleRef:
      kind: ConfigMap
      name: kanidm-ca
      key: ca.crt
  radiusTls:
    secretRef:
      name: radius-eap-tls
    certKey: tls.crt
    privateKeyKey: tls.key
    caKey: ca.crt
  service:
    type: LoadBalancer
    port: 1812
    externalTrafficPolicy: Local
  tokenRotation:
    enabled: true
    periodDays: 30
~~~

`radiusTls` is mandatory because the current upstream bootstrap reads and bundles the cert/key at startup; a real upstream-image test must verify the cert mount locations and writable FreeRADIUS paths. The exact image/tag shown above is illustrative and must be aligned to a verified supported upstream pair before publishing generated examples.

### Responsibility and ownership

~~~text
KanidmRadiusServer (namespaced CR, with finalizer)
  ├── Kanidm service account: dedicated, UID-bound, one per CR
  │   ├── own read-only API tokens, versioned labels, IDs tracked
  │   └── own ONLY membership edge -> idm_radius_servers
  ├── generated Secret(s): versioned radius.toml with bearer token + resolved NAS secrets
  ├── Deployment: upstream kanidm/radius and mounted externally owned TLS/CA references
  └── Service: UDP 1812

Referenced, NOT owned:
  Kanidm cluster; idm_radius_servers group and other members;
  NAS shared-secret Secrets; EAP certificate Secret; Kanidm CA ConfigMap/Secret;
  per-person RADIUS credentials, access groups and NAS equipment
~~~

- Never call `idm_group_set_members` or `idm_group_delete` against `idm_radius_servers`. Only `idm_group_get_members` + `idm_group_add_members` to add the exact identity; delete only that identity edge with `idm_group_remove_members`.
- Service-account identity must bind to CR **UID**, not just reusable namespace/name; detect conflicts instead of silently adopting an unrelated pre-existing account. Keep a durable identity marker/metadata and token IDs, with a recovery path for controller restart and status loss.
- Do **not** create a hidden child `KanidmServiceAccount` in MVP; its independent authoritative-token reconciler would compete with RADIUS rotation state.
- Owner references on namespace-local generated Kubernetes objects only. Never attach to external Secrets or to cross-namespace parents. Check owner UID before updates/deletes; refuse clashes.
- Operator account with `idm_radius_servers` membership and a **read-only** API token; do not assign `idm_radius_service_admins` or runtime write scope.

### Reconciliation and state

1. Fetch and authorize `kanidmRef`; check restore guard and namespace selector.
2. Read/validate all input Secret and CA/TLS keys, content, names, address ranges, VLAN mapping, image pair and Service topology **before** mutating Kanidm. Existing valid generations must survive bad desired changes.
3. Ensure dedicated identity only after proving ownership; add membership idempotently.
4. Mint/read the active token from a generated Secret; since Kanidm cannot return token plaintext later, mint a new one if the value is permanently lost. Never log credentials.
5. Deterministically render and validate TOML, safely quoting every string. Additionally account for upstream `bootstrap.rs` writing `clients.conf` using unescaped client name and Secret values: validate or reject any newline, brace/control character and FreeRADIUS syntax-breaking input instead of allowing generated-config injection.
6. Create revision-addressed generated config Secret. Apply Deployment with hash/revision annotations and appropriate TLS/CA mounts.
7. Apply a Service with UDP/1812, labels/selectors and external traffic settings. Avoid HTTP Ingress/Gateway abstractions.
8. Observe Deployment revision, ready/available replicas, target Secret presence and Service readiness; report conditions and requeue.
9. Rotate/revoke/garbage-collect only after a candidate is proven usable. If an upstream RADIUS-aware health check is unavailable, document a conservative safe gate and do not claim authentication continuity from basic Kubernetes readiness alone.
10. Update status on every state transition; support restart/retry/out-of-order events without leaking credentials or abandoning active tokens.

Status MUST include `observedGeneration`, conditions (`IdentityReady`, `ConfigurationReady`, `Available`, `Exposed`, `Rotating` or equivalent), active and pending token IDs/generations, generated config Secret name, Deployment and Service names, number of ready replicas, and external endpoint when reported. Conditions use list-map annotations keyed by `type` consistent with other Kaniop CRDs; status contains token **IDs only**, never token values or NAS secrets.

### Token rotation: overlap/rollback

~~~text
STEADY(A)
  ├─ preflight candidate inputs
  ├─ issue read-only B (unique UID+generation label)
  ├─ create config Secret B with B token
  ├─ patch Deployment to B, preserve A/token A while replacement rolls out
  ├─ verify B generation is in service; on failure preserve A and mark blocked
  ├─ promote B to active only after successful verification
  ├─ revoke A; retry revoke on transient failure
  └─ garbage collect A configuration only after old pods are gone
STEADY(B)
~~~

- NEVER destroy A before B is usable. Treat token issuance, Secret creation, status persistence and rollout as independently fallible: after crash/restart, list token metadata/labels to recover leaked candidates and reconcile to a single active + one pending generation.
- For replicas ≥2, `RollingUpdate` with `maxUnavailable: 0` and `maxSurge: 1`; define safe behavior for replicas=1 and node failures. If the candidate fails, keep/restore known-good ReplicaSet and credential.
- Differentiate periodic API-token rotation from input Secret and certificate/CA changes. Input changes should create a new configuration revision and rollout **without unnecessary token churn**. Update of referenced TLS/CA material changes pod-template digest and restarts processes (upstream parses at startup).
- Generated Secret must not be overwritten in place while old pods still use it; version it using collision-resistant, bounded names. Prevent unbounded Secret accumulation.

### Workload, health, Kubernetes networking

- Use official `kanidm/radius` image with explicit tag or safe derivation from parent tag; never derive a RADIUS image digest from a Kanidm server image digest. Explicit `spec.image` mandatory when no safe counterpart exists.
- Kaniop's existing Helm chart RBAC covers operator ServiceAccount; extend only as needed. Prefer no `list/watch` on input Secrets unless controlled informer necessity is demonstrated. Controller must never include plaintext in event/log/error/metrics or status.
- RADIUS pod has `automountServiceAccountToken: false`, `enableServiceLinks: false` and no Kubernetes API rights. Test `runAsNonRoot`, `readOnlyRootFilesystem`, dropped capabilities and writable `emptyDir` paths with upstream container; do not impose defaults the image cannot run with.
- Require real startup readiness and termination grace; a plain `Deployment.readyReplicas` is not enough to guarantee the Kanidm backend accepts credentials. Include real `radtest`/EAP test in E2E and consider authentication-focused synthetic readiness before revoking old tokens.
- Default exposure `ClusterIP`; optional `NodePort` / `LoadBalancer` with UDP support. For external source-IP matching, recommend `externalTrafficPolicy: Local` when supported. Avoid inventing LoadBalancer success from existence of a Service; report `Exposed=False` if required ingress never provisions. Source-IP/NAS allowlisting must never be broadened automatically as a workaround for SNAT.
- Authentication port `1812/UDP` only; accounting `1813` is a separate proposal. NetworkPolicy optional follow-up unless upstream image/network constraints and target cluster capabilities justify portable initial support.

### TLS, CA and privacy

- Two separate trust domains: verify outbound Kanidm HTTPS using an optional user-provided CA bundle, and mount user-managed EAP/RADIUS certificate/private key/CA (mandatory for upstream bootstrap). cert-manager is compatible but not coupled to Kaniop.
- Do not turn off `verify_certificate` or `verify_hostnames`. Validate PEM and key/certificate consistency where feasible and fail closed. Watch/hash only relevant Secret keys (not resourceVersion noise).
- Generated `radius.toml` contains NAS secrets and API token, so it must be stored in `Secret`. No credential values in CRD, ConfigMap, events or telemetry. Document Kubernetes Secret at-rest encryption and scoped RBAC.
- Review FreeRADIUS upstream requirement for `require_message_authenticator = yes` (current bootstrap emits it) and test compatible MikroTik/NAS firmware; do not weaken it for convenience. EAP/MSCHAPv2 user RADIUS credentials are Kanidm-owned and intentionally not projected into Kubernetes.

### Deletion, drift, failure, ownership

- Finalizer first stops creating new revisions and deletes/exits workload, revokes only owned tokens, removes only owned group member edge, deletes owned Kanidm service account, then lets GC remove owned Kubernetes children and removes finalizer.
- A non-reachable Kanidm during deletion is retryable/blocking, not a reason to discard credentials; documented manual break-glass procedure with explicit identity verification.
- If a referenced external Secret/CA disappears or is malformed, report `ConfigurationReady=False` and keep last known working revision and active token.
- Restore manually removed membership additively; do not disturb other members. If service-account ownership is ambiguous, `IdentityReady=False/OwnershipConflict`, not adoption.
- No implicit adoption of existing manual RADIUS containers/tokens/accounts. Document safe side-by-side migration. Explicit adoption is postponed to a separately reviewed API.
- `spec.kanidmRef` immutable. Namespace/CR rename is delete-and-create. Reusing the same name with a different UID must not grant ownership of old external identities.
- Protect against reconciliation during parent Kanidm restore; tolerate API timeouts, 5xx, cancellation and version skew. Never revoke a live token because an external API is temporarily unreachable.

## Acceptance criteria

- [ ] CRD generated, structural schema valid (including condition map annotations and immutable refs); examples generated, Helm packaging/RBAC documented.
- [ ] Controller uses existing shared cache, namespace selector and backoff/leader-election wiring.
- [ ] Additive built-in group membership test proves unrelated members retained across 2+ RADIUS CRs.
- [ ] Read-only token accepted by a real upstream RADIUS container.
- [ ] Single/dual replicas authenticate allowed user, deny disallowed group, incorrect NAS secret and unknown NAS IP.
- [ ] VLAN reply correct and valid input TOML, certificate files and FreeRADIUS `clients.conf` generated safely.
- [ ] Secrets/CA/certs mutation causes rollout without early API-token revocation.
- [ ] Failed B rotation, crash mid-rotation, missing generated Secret and revoked token recover or degrade visibly without deleting known-good A.
- [ ] Finalizer deletes only owned state; unrelated accounts/groups and external Secrets survive.
- [ ] Cross-namespace KanidmRef authorization, restore guards, immutable refs and conflict/adoption handling tested.
- [ ] Kubernetes UDP Service modes and source-IP preservation documented/tested in supported cluster path.
- [ ] No credential data in status/telemetry/logs; pod API token not mounted, upstream security smoke passes.
- [ ] Real image E2E included in CI; pinned compatibility pair, failures and upgrades covered.
- [ ] User docs, migration/break-glass runbook, versioning, and Helm/RBAC check complete.

## Non-goals

RADIUS user credential resets/generation; NAS device provisioning; RADIUS accounting; generic membership controller; dynamic FreeRADIUS plugins; operator-managed CA; implicit adoption; cross-cluster Kanidm references; raw TOML passthrough.

## Open technical decisions requiring verification before final API freeze

1. RADIUS runtime readiness probe that can distinguish a process listening from a backend and token actually working (especially safe revocation timing).
2. Supported Kanidm ↔ RADIUS image compatibility and digest-only parent image behavior.
3. Cert private key file permissions and writable paths with a restricted SecurityContext; test runtime from container image, do not guess.
4. Secret reference watch fan-out and namespace scoping consistent with kube-rs controller setup.
5. Explicitly define client SharedSecret accepted character set to prevent unsafe `clients.conf` output; upstream currently writes the unescaped value into FreeRADIUS syntax.
6. Whether a first release needs exposed transport-specific TLS/EAP options from upstream; prefer no speculative options.
7. Confirm Kanidm account naming/ownership proof and token-recovery semantics under namespace rename/UID conflict.

These items block declaring the **feature** complete; they do not block committing this reviewable specification.
