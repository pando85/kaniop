# Specification: Kanidm RADIUS servers (#1119)

Derived from: [intent.md](intent.md). Status: **design accepted; implementation in progress** (see [plan.md](plan.md) for the work packages). The token lifecycle is derived from observed Kanidm and Kubernetes state on every reconcile, not from a persisted generation journal.

## Evidence and architecture rationale

This design follows:
- Kaniop `libs/operator/src/kanidm/reconcile/mail_sender.rs`: companion Deployment, internally created service account, additive built-in group membership, generated token/config Secret, CA hashing, cleanup.
- Kaniop `libs/group/src/reconcile.rs` and `libs/group/src/crd.rs`: `KanidmGroup.spec.members` is **authoritative** (`idm_group_set_members`), so it must NOT own `idm_radius_servers`.
- Kaniop `libs/service-account/src/crd.rs`: API tokens support read-only purpose, but its current rotation destroys/recreates tokens, unsuitable for an availability-sensitive active RADIUS workload.
- Kaniop `libs/operator/src/crd.rs`: shared immutable `KanidmRef` with namespace support; namespace-selector authorization is part of the controller contract.
- Kanidm `kanidm_proto` `ApiToken` carries `issued_at` and expiry, so rotation age and token inventory come from Kanidm itself.
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
- No spec field selects the Kanidm URL. The URI the RADIUS runtime uses is derived from the referenced Kanidm: `spec.origin` when set, otherwise `https://{spec.domain}`. Split-horizon or non-default-port setups are expressed on the `Kanidm` resource, not on `KanidmRadiusServer`.
- The CR name MUST be at most 52 characters (root CEL rule) so the derived Service name is a valid DNS label. `Ready` and `Endpoint` are exposed as printer columns.
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
  │   ├── every API token on the account (read-only, expiring when rotation is enabled)
  │   └── own ONLY membership edge -> idm_radius_servers
  ├── generated Secret(s): immutable, content-addressed radius.toml with bearer token + resolved NAS secrets
  ├── Deployment: upstream kanidm/radius and mounted externally owned TLS/CA references
  └── Service: UDP 1812

Referenced, NOT owned:
  Kanidm cluster; idm_radius_servers group and other members;
  NAS shared-secret Secrets; EAP certificate Secret; Kanidm CA ConfigMap/Secret;
  per-person RADIUS credentials, access groups and NAS equipment
~~~

- Never call `idm_group_set_members` or `idm_group_delete` against `idm_radius_servers`. Only `idm_group_get_members` + `idm_group_add_members` to add the exact identity; delete only that identity edge with `idm_group_remove_members`.
- Service-account identity must bind to CR **UID**, not just reusable namespace/name; detect conflicts instead of silently adopting an unrelated pre-existing account. Keep a durable identity marker (UID-derived name plus display-name marker) in Kanidm. Because the account is dedicated to one CR UID, every token on it belongs to that CR; no per-token ownership bookkeeping in CR status is required, and controller restart or status loss needs no recovery path.
- Do **not** create a hidden child `KanidmServiceAccount` in MVP; its independent authoritative-token reconciler would compete with RADIUS rotation state.
- Owner references on namespace-local generated Kubernetes objects only. Never attach to external Secrets or to cross-namespace parents. Check owner UID before updates/deletes; refuse clashes.
- Operator account with `idm_radius_servers` membership and a **read-only** API token; do not assign `idm_radius_service_admins` or runtime write scope.

### Reconciliation and state

The reconciler is **world-derived**: on every pass it observes Kanidm (tokens on the owned account) and Kubernetes (config Secrets, the Deployment template, owned ReplicaSets, pods) and computes what to do with a pure planner. CR status is a report and cache only; losing it MUST NOT change behavior.

1. Fetch and authorize `kanidmRef`; check restore guard and namespace selector. An unauthorized new CR gets no finalizer and a `ResourceNotWatched` event.
2. Install the finalizer, then read/validate all input Secret and CA/TLS keys, content, names, address ranges, VLAN mapping, image pair and Service topology **before** mutating Kanidm. A bad desired change leaves the running workload and token untouched.
3. Ensure the dedicated identity only after proving ownership; add membership idempotently.
4. Observe the world and plan tokens (see below).
5. Deterministically render and validate TOML, safely quoting every string. Upstream `bootstrap.rs` writes `clients.conf` using the unescaped client name and Secret values: reject any newline, brace or control character instead of allowing generated-config injection.
6. When a new token is needed: mint it read-only, verify it with `whoami` using the candidate token, and only then create the immutable config Secret and apply the Deployment. A candidate that fails verification never reaches pods.
7. Apply the Deployment with annotations for the token id and the TLS/CA inputs digest, and a UDP/1812 Service. Foreign objects of the same name are reported as a conflict, never adopted.
8. Re-observe, then revoke retired tokens and garbage-collect unused config Secrets.
9. Report conditions on every pass, including error paths, and requeue. Transient API errors drive backoff and keep their source; expected blocks carry a stable reason and never include secrets.

Status MUST include `observedGeneration`, `ready` and conditions (`IdentityReady`, `ConfigurationReady`, `Available`, `Exposed`, `Rotating`, all always present, keyed by `type` as list-map). It MUST also report `tokenId`, `tokenIssuedAt`, `configSecretName`, `deploymentName`, `serviceName`, `replicas`, `readyReplicas`, `endpoint` (when the Service reports one) and `retiringTokenIds` (informational). `ready` is true only when identity and configuration are ready, the rollout is complete and available, and the Service is exposed. Status contains token **IDs only**, never token values or NAS secrets. There are no active/pending checkpoint fields.

### Token lifecycle: overlap and revocation

~~~text
observe(K = tokens on account, template token, ReplicaSets, pods, owned Secrets)
  -> plan: keep current | mint (Initial, BearerLost, Expired, Due, Requested) | blocked
  -> mint read-only B (expiry = issued + 2 x periodDays if rotation enabled)
  -> whoami with B
  -> create immutable Secret B, apply Deployment pointing at B (maxUnavailable 0)
  -> re-observe; revoke A only when no template, ReplicaSet or pod uses A
  -> garbage-collect config Secrets nothing references
~~~

- The current token is kept while it exists in Kanidm, its bearer is readable from the template's Secret, it is not expired or inside the safety margin, rotation is not due (`issued_at + periodDays`) and the `kaniop.rs/radius-rotate-token` CR annotation equals the value recorded on the Secret. Any other state mints a new token.
- NEVER revoke a token while the Deployment template, any ReplicaSet with replicas, or any non-terminal pod (Terminating counts) uses it. A pod uses a token if it carries the token-id annotation or mounts one of the CR's owned config Secrets. Revoke only tokens that are not current and not in use, and that either still have an owned Secret (retired) or have no Secret and are older than a 10-minute grace period (orphan).
- Never mint without bound: more than four tokens on the account blocks minting and is reported as a condition.
- Token expiry: `issued + 2 x periodDays` when `tokenRotation.enabled`, none otherwise. Manual rotation is requested with the `kaniop.rs/radius-rotate-token` annotation on the CR (for example after a suspected leak).
- Crashes between steps leave either an orphan token (revoked after the grace period) or an unused Secret (garbage-collected); neither revokes a token a pod uses.
- Use `RollingUpdate` with `maxUnavailable: 0` and `maxSurge: 1` (replicas 1 included). The `whoami` check proves the candidate token is accepted by Kanidm; it does not prove RADIUS login, because the operator holds no user credential. Do not claim authentication continuity from Kubernetes readiness alone.
- Input Secret, NAS secret, CA and certificate changes re-render configuration and roll the pods **without minting a token**, using the template's existing bearer. TLS/CA changes roll pods through a pod-template annotation holding a digest of the public certificate and CA bytes only (a private key cannot change without its certificate changing).
- Generated config Secrets are immutable and named `<cr>-<first 10 hex of sha256(radius.toml)>`. They are never overwritten in place while old pods use them, and unreferenced ones are garbage-collected, so they cannot accumulate.

### Workload, health, Kubernetes networking

- Use official `kanidm/radius` image with explicit tag or safe derivation from parent tag; never derive a RADIUS image digest from a Kanidm server image digest. Explicit `spec.image` mandatory when no safe counterpart exists.
- Kaniop's existing Helm chart RBAC covers operator ServiceAccount; extend only as needed (ReplicaSet read access is required for the in-use scan). Watch referenced input Secrets and ConfigMaps with metadata-only watchers mapped to the referencing CRs; never cache their data. Controller must never include plaintext in event/log/error/metrics or status.
- RADIUS pod has `automountServiceAccountToken: false`, `enableServiceLinks: false` and no Kubernetes API rights. The pod runs as non-root with dropped capabilities and no privilege escalation. `readOnlyRootFilesystem` is deliberately left unset: the upstream bootstrap writes `/etc/raddb/clients.conf` and `/etc/raddb/certs` at startup, and a read-only root would require a verified set of writable `emptyDir` paths that is not known to work. The rationale is documented and a test asserts the field stays unset until a runtime test with the official image proves a working writable-path set. Do not impose defaults the image cannot run with.
- Require real startup readiness and termination grace. `Deployment.readyReplicas` alone does not show that Kanidm accepts the token, which is why the candidate token is verified with `whoami` before rollout and old tokens are revoked only when unused. Real `radtest` authentication is covered by E2E, including during rotation.
- Default exposure `ClusterIP`; optional `NodePort` / `LoadBalancer` with UDP support. For external source-IP matching, recommend `externalTrafficPolicy: Local` when supported. Avoid inventing LoadBalancer success from existence of a Service; report `Exposed=False` if required ingress never provisions. Source-IP/NAS allowlisting must never be broadened automatically as a workaround for SNAT.
- Authentication port `1812/UDP` only; accounting `1813` is a separate proposal. NetworkPolicy optional follow-up unless upstream image/network constraints and target cluster capabilities justify portable initial support.

### TLS, CA and privacy

- Two separate trust domains: verify outbound Kanidm HTTPS using an optional user-provided CA bundle, and mount user-managed EAP/RADIUS certificate/private key/CA (mandatory for upstream bootstrap). cert-manager is compatible but not coupled to Kaniop.
- Do not turn off `verify_certificate` or `verify_hostnames`. Validate PEM and key/certificate consistency where feasible and fail closed. Watch/hash only relevant Secret keys (not resourceVersion noise).
- Generated `radius.toml` contains NAS secrets and API token, so it must be stored in `Secret`. No credential values in CRD, ConfigMap, events or telemetry. Document Kubernetes Secret at-rest encryption and scoped RBAC.
- Review FreeRADIUS upstream requirement for `require_message_authenticator = yes` (current bootstrap emits it) and test compatible MikroTik/NAS firmware; do not weaken it for convenience. EAP/MSCHAPv2 user RADIUS credentials are Kanidm-owned and intentionally not projected into Kubernetes.

### Deletion, drift, failure, ownership

- Finalizer order: delete the Deployment (foreground), wait until no pod or ReplicaSet references the CR's config Secrets, remove only the CR's own `idm_radius_servers` membership edge, delete the owned Kanidm account (which removes all of its tokens), then let owner-reference GC remove owned Kubernetes children and remove the finalizer.
- If the Kanidm account exists but is not owned by this CR (UID/marker mismatch), or the referenced Kanidm CR no longer exists, skip the Kanidm cleanup, emit a Warning event and remove the finalizer; never block deletion forever on something the CR does not own or cannot reach by design. A Kanidm that exists but is unreachable is retryable with backoff, not a reason to discard credentials; a documented manual break-glass procedure covers stuck cases.
- If a referenced external Secret/CA disappears or is malformed, report `ConfigurationReady=False` and keep the running workload and current token.
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
- [ ] Secrets/CA/certs mutation causes rollout without minting or revoking an API token.
- [ ] Failed candidate verification, operator crash mid-rotation, wiped status, deleted config Secret and externally revoked token recover or degrade visibly without revoking a token any pod still uses.
- [ ] Finalizer deletes only owned state; unrelated accounts/groups and external Secrets survive; an unowned account or missing Kanidm CR is skipped with a warning and does not block deletion.
- [ ] Cross-namespace KanidmRef authorization, restore guards, immutable refs and conflict/adoption handling tested.
- [ ] Kubernetes UDP Service modes and source-IP preservation documented/tested in supported cluster path.
- [ ] No credential data in status/telemetry/logs; pod API token not mounted, upstream security smoke passes.
- [ ] Real image E2E included in CI; pinned compatibility pair, failures and upgrades covered.
- [ ] User docs, migration/break-glass runbook, versioning, and Helm/RBAC check complete.

## Non-goals

RADIUS user credential resets/generation; NAS device provisioning; RADIUS accounting; generic membership controller; dynamic FreeRADIUS plugins; operator-managed CA; implicit adoption; cross-cluster Kanidm references; raw TOML passthrough.

## Technical decisions and remaining verification

Resolved by the accepted design:
- Backend/token usability gate: `whoami` with the candidate token before rollout plus revoke-when-unused (no RADIUS-aware probe is possible without a user credential).
- Ownership proof and token-recovery semantics: UID-bound dedicated account; all tokens on it are the CR's; state is world-derived.
- Secret reference watch fan-out: metadata-only watchers mapped to referencing CRs.

Still requiring verification (tracked as spikes in plan.md; they block declaring the **feature** complete):
1. Supported Kanidm and RADIUS image compatibility and digest-only parent image behavior; automatic inference stays limited to `kanidm/radius:1.11.2`.
2. Writable paths under a restricted security context, to decide whether `readOnlyRootFilesystem` can ever be enabled.
3. Client SharedSecret accepted character set to prevent unsafe `clients.conf` output (upstream writes the unescaped value into FreeRADIUS syntax).
4. Whether a first release needs exposed transport-specific TLS/EAP options from upstream; prefer no speculative options.
5. How the e2e RADIUS pod reaches Kanidm over verified TLS using test-only means, with no new CRD field.
