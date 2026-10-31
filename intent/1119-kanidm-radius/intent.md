# Intent: Declarative Kanidm RADIUS integration (#1119)

- Owner: Kaniop maintainers
- Status: Draft — proposed for technical/product review
- Work item: https://github.com/pando85/kaniop/issues/1119
- Related: https://github.com/pando85/kaniop/issues/433
- Design: [spec.md](spec.md)
- Execution: [plan.md](plan.md)

## Problem

Today an administrator can deploy the official Kanidm RADIUS container outside Kaniop, but must separately create a privileged-in-the-right-way Kanidm service account, grant membership in `idm_radius_servers`, generate and rotate its API token, wire the token to the runtime, maintain NAS client shared secrets and EAP certificates, configure network exposure, and clean up resources on removal. A plain workload Helm chart does not reconcile these identity and Kubernetes lifecycles. Issue #433 addressed only workload deployment; #1119 requests the missing integration.

A naïve use of `KanidmGroup` is unsafe: its `spec.members` list is authoritative and can replace members of a shared Kanidm built-in group.

## Proposed outcome

A GitOps-friendly `KanidmRadiusServer` namespaced custom resource declaratively manages independent RADIUS authentication front ends targeting a Kanidm cluster. Reconciliation performs safe and reversible identity/bootstrap operations, generates least-privilege read-only API tokens, consumes user-owned Kubernetes Secrets and CA material, deploys the official runtime behind UDP Services, preserves the last known working generation during failed changes, rotates credentials with overlap, and cleans up only owned state.

## Users and systems

- Operators of MikroTik routers, wireless infrastructure, VPNs, and other RADIUS NAS devices.
- Kaniop identity administrators managing one or multiple Kanidm installations.
- Platform operators deploying 0..N authentication edges in separate namespaces and network zones.
- Kanidm's API and `idm_radius_servers` shared built-in group; Kubernetes Secrets, Deployments, Services, and cert-manager-managed certificate Secrets.

## Constraints and non-goals

1. No user RADIUS password generation, reset, storage in Kubernetes or export into GitOps. Kanidm and its users own these credentials.
2. Never manage, replace, or delete the `idm_radius_servers` group or unrelated members.
3. Never infer ownership of an existing service account, workload, token or Secret. Manual migration is deliberate.
4. No implicit weakening of TLS verification, FreeRADIUS client validation, certificate permissions, or Kubernetes pod security.
5. No operator-owned PKI or cert-manager dependency; consume external TLS material.
6. No RADIUS accounting (UDP/1813), dynamic raw FreeRADIUS override, or router provisioning in the first release.
7. Match repository architecture, namespace-selector permissions, code generation, and release/version conventions.
8. A staged PR is not feature-complete until all MUST acceptance criteria and real RADIUS E2E gates pass.

## Success measures

- Two or more `KanidmRadiusServer` resources can independently use one Kanidm without overwriting each other's group membership.
- A RADIUS NAS authenticates an allowed test user; denies unauthorized users or incorrect NAS credentials; VLAN response matches policy.
- Missing/bad references leave any last-known-good runtime and active token intact, with actionable conditions.
- Changing an input Secret/CA/certificate rolls the consuming workload; rotation does not revoke a working token before the candidate is actually usable.
- Deletion removes only Kaniop-owned identity/permission/credentials and generated objects, not externally supplied Secret/CA resources.
- Generated CRD, examples, Helm RBAC, unit/integration/e2e tests, user docs and CI are coherent and reproducible.

## Review gates

- [ ] Intent accepted by issue owner.
- [ ] [Specification](spec.md) accepted by product/security maintainers.
- [ ] [Implementation plan](plan.md) accepted by technical maintainer.
- [ ] End-to-end feature acceptance and independent code review completed before merging.

## Open questions

- Verify TLS file expectations and non-root compatibility for each supported pinned `kanidm/radius` version.
- Define health verification stronger than Deployment readiness before completing token rotation.
- Confirm the minimum supported Kanidm/Kaniop image pair and current official Docker Hub tag policy.
- Decide whether cross-namespace references remain supported with explicit selector and namespace-based authorization; same-namespace input Secret references remain mandatory.
