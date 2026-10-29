# Implementation plan: KanidmRadiusServer (#1119)

Derived from [spec.md](spec.md); issue [#1119](https://github.com/pando85/kaniop/issues/1119). Status: **in progress — configuration, TLS, UID identity and upstream bootstrap smoke implemented; CRD/controller still outstanding**.

This file is the **source of truth** for executing the complete curated design, not a claim that a new CRD already works. Every step below is an explicit gate; a PR must remain draft until MUST items, generated artifacts and E2E tests pass. Keep this checklist updated in commits; do not silently drop research recommendations. A superseding implementation PR must link this plan and retain traceability.

## Repository findings

- `AGENTS.md` requires the `intent/` three-artifact chain, approved design for high-risk changes, Conventional Commits with DCO, no hand-edited generated CRDs or examples, and an independent verification pass.
- `.opencode/skills/kaniop-development/SKILL.md`, `examples/SKILL.md` and `helm-chart/SKILL.md` govern implementation.
- `libs/operator/src/kanidm/reconcile/mail_sender.rs` provides concrete service-account, additive built-in group membership, token, CA, generated Secret, Deployment and cleanup implementations; do not copy its full monolithic reconciler.
- `libs/group/src/reconcile.rs` uses authoritative `idm_group_set_members`, explicitly unsafe for global `idm_radius_servers`.
- `libs/service-account/src/crd.rs` has read-only API tokens but destructive destroy/recreate rotation; do not reuse its rotation algorithm.
- `libs/operator/src/crd.rs` has immutable `KanidmRef`; `libs/operator/src/controller/context.rs` has Kanidm client/store caching and restore guard.
- `cmd/operator/src/main.rs` registers each controller, `cmd/crdgen/src/main.rs` generates every CRD and schema tests, `cmd/examples` generates examples, `charts/kaniop/templates/clusterrole.yaml` provides operator RBAC, `tests/e2e` runs Kind-backed tests with Makefile shards.
- Current runtime requirements: upstream `rlm_kanidm/entrypoint/src/bootstrap.rs` reads `/data/radius.toml` at start, writes `clients.conf` and requires server certificate/key files. Its direct interpolation of NAS client name/secret in FreeRADIUS syntax adds a **config-injection validation requirement** not sufficiently emphasized by the initial research report.
- Cross-namespace Kanidm references are already supported with namespace selectors in other identity resources; apply the same authorization model for the new CRD.

## Architecture decision and review gates

- [x] Research comparison: dedicated CRD vs `Kanidm.spec.radius` vs manual composition; choose standalone `KanidmRadiusServer`.
- [x] Research review: Kanidm radius config and bootstrap, mail sender, service-account and group reconciler, CloudNativePG Pooler, Strimzi, cert-manager, Vault Secrets Operator and Kubernetes guidance.
- [x] Record intended scope and non-goals in `intent.md`.
- [x] Record typed API, ownership, security and lifecycle in `spec.md`.
- [ ] Maintainer accepts intent/product boundary.
- [ ] Maintainer reviews CRD field names/defaults, mandatory TLS/authorization, namespace selector and compatibility pinning.
- [ ] Maintainer accepts documented failure, ownership, migration and rotation behavior.
- [ ] API review resolves outstanding runtime/health and FreeRADIUS escaping questions.
- [ ] Independent review against `REVIEW.md` before changing from draft to ready.

## Progress log

- **2026-10-29:** Implemented and exposed `libs/operator/src/kanidm/radius_config.rs`: typed upstream TOML renderer, mandatory authorization-group checks, client name/IP/CIDR/shared-secret validation, fixed TLS file paths, safe error messages and unit tests. Verified upstream v1.11.2 config/bootstrap source shape. This is not yet wired to a controller. Also implemented `libs/operator/src/kanidm/radius_tls.rs` with OpenSSL-based leaf PEM/private-key correspondence, validity-date and optional CA bundle checks; added unit tests. GitHub Actions verification remains the source of truth; E2E with real Kanidm RADIUS has not run.
- **Remaining immediate work:** validate generated TOML against the pinned v1.11.2 runtime; connect EAP PEM certificate/key preflight and test it with the official container; add UID ownership tests; then implement the CRD and controller wiring using a real Rust toolchain for mandatory generated artifacts.

- **2026-10-29 (additional):** Added `tests/e2e/scripts/radius-bootstrap-smoke.sh` and `.github/workflows/radius-bootstrap-smoke.yml`. Initial CI failed because the host-created fixture directory was mode 0700 and unreadable by non-root `radiusd`; fixed it. Published `kanidm/radius:1.11.2` image **passed** the dedicated bootstrap workflow on commit `aa584cd`, verifying expected configuration, EAP cert/key file paths and the generated `require_message_authenticator = yes` setting. This is a startup smoke test, **not** live Kanidm authentication. Added pure `radius_identity.rs` helper and unit tests for immutable Kubernetes UID-derived Kanidm identity names and non-adoption checks. Full Rust CI on latest branch is pending.

## Ordered implementation steps

### Phase 0 — Foundation and safety invariants

- [ ] Freeze the upstream Kanidm and `kanidm/radius` versions supported in CI; confirm official image tags and actual runtime behavior.
- [x] Reproduce bootstrap startup with cert/key mount, default writable paths, non-root restrictions and `require_message_authenticator` with the official v1.11.2 image in GitHub CI (commit aa584cd). This does not test K8s SecurityContext or real authentication.
- [ ] Confirm Kanidm API token scope (read-only) authenticates an actual RADIUS request when service account belongs to `idm_radius_servers`.
- [ ] Record and enforce full API/naming invariant: immutable Kanidm reference, dedicated UID-bound service-account identity, no account adoption, per-resource token labels, first-release same-namespace input Secret refs. UID name generator/unit tests implemented, but external identity reconciliation and conflict checks not wired yet.
- [ ] Implement narrowly-scoped reusable additive-membership and CA digest helpers **only if safe to extract**; add regression tests proving existing mail sender behavior unchanged. Do not refactor unrelated reconcilers.

Files: `libs/operator/src/kanidm/reconcile/mail_sender.rs`, `libs/operator/src/crd.rs` or narrow new integration helper modules, `tests/e2e/` if needed.

Verify: focused unit tests for membership and CA digest; existing mail-sender test filter; upstream image smoke in CI.

### Phase 1 — CRD, validation, registration, status

- [ ] Define `KanidmRadiusServerSpec` and `KanidmRadiusServerStatus` in a new Rust library/module following existing identity CRDs; use existing `KanidmRef` type and immutability validation.
- [ ] Add scope-specific `radiusServerNamespaceSelector` on Kanidm if consistent with controller authorization; ensure default and selector removal block mutation.
- [ ] Fields: Kanidm reference; image; replicas; required groups; default VLAN; group VLAN mappings; NAS clients and Secret keys; Kanidm CA ref; required EAP certificate/key Secret; Service UDP configuration; resources/scheduling; rotation period.
- [ ] Schema/CEL + runtime validation for names, unique client names/CIDRs, nonempty NAS refs and secrets, VLAN ranges, mandatory groups/TLS and valid Service settings; fail closed.
- [ ] Add condition-list map annotation keyed by type; status: observedGeneration, identity/config/availability/exposure conditions, deployment, Service, active/pending token IDs/generations, config revision and ready replicas; no plaintext or secret hashes disclosed.
- [ ] Register new controller in `cmd/operator/src/main.rs` using shared context, existing leader-election, error backoff and Kanidm client/restore guard. Record metrics safely.
- [ ] Add associated dependency, feature flags and CRD generation entries, webhook/CEL validation integration if applicable; generate and commit CRD **using make crdgen**, do not edit generated YAML.
- [ ] Add exemplar Rust generator in `cmd/examples`; commit generated examples only via `make examples`.
- [ ] Include typed API unit tests, CRD schema tests, default/validation tests.

Likely files: `libs/radius/{Cargo.toml,src/lib.rs,src/crd.rs,src/controller.rs,src/reconcile/*}` (or an equivalent minimal in-tree module based on current dependency graph), root `Cargo.toml`, `Cargo.lock` if changed, `cmd/operator/{Cargo.toml,src/main.rs}`, `cmd/crdgen/{Cargo.toml,src/main.rs}`, `cmd/examples/{Cargo.toml,src/...}`, `libs/operator/src/kanidm/crd.rs`, `charts/kaniop/crds/crds.yaml` **generated only**, `examples/radius-server.yaml` **generated only**.

Verify: `cargo fmt --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; targeted CRD tests; `make crdgen` and `make examples` with clean subsequent diff.

### Phase 2 — Kanidm-side identity and ownership

- [ ] Generate collision-resistant, UID-bound service-account name and description/identity marker within Kanidm constraints; detect pre-existing unmanaged identity as `OwnershipConflict`. Never update/adopt unrelated account.
- [ ] Create own dedicated Kanidm service account and add it to `idm_radius_servers` *additively* using `idm_group_add_members`; idempotent membership get; never `idm_group_set_members`.
- [ ] Issue least-privilege read-only token with uniquely identifiable label/ID. Persist bearer only in generated Kubernetes Secret, token IDs in status.
- [ ] Restore service account membership after out-of-band removal; keep every unrelated group member. Refuse to delete the built-in group.
- [ ] Prevent writes during Kanidm restore; handle 403, 404, timeouts, cancellation and 5xx. Isolate permissions; do not grant `idm_radius_service_admins`.
- [ ] Add finalizer at appropriate point before external mutations; test orphan-prevention under crashes.
- [ ] Make 0..N RADIUS CRs for one Kanidm safe: each owns one account, its token IDs and its membership edge.

Files: new `libs/radius/src/{controller,reconcile/identity,reconcile/token,reconcile/status}.rs`; shared metrics/helpers only as needed.

Verify: unit+real Kanidm integration tests for UID conflicts, membership preservation with two RADIUS CRs, read-only token, restart and repeated reconcile, parent restore and degraded Kanidm.

### Phase 3 — Kubernetes configuration, Deployment and Service

- [ ] Resolve NAS Secret keys and EAP cert/key/CA; validate complete configuration **before** modifying active Kanidm credentials or replacing Kubernetes workloads.
- [ ] Safely generate upstream-compatible deterministic TOML and escape all strings; **validate NAS client names and secrets against upstream unescaped FreeRADIUS clients.conf interpolation** to forbid newline, braces and syntax-breakers. Test adversarial inputs.
- [ ] Generate owned, revision-addressed Kubernetes `Secret` containing all sensitive `radius.toml` data; immutable-ish generations avoid mixed runtime state; do not place token/NAS secret in ConfigMap, status, annotations or logs.
- [ ] Mount generated Secret at `/data/radius.toml` as expected by upstream. Mount external EAP cert/key and optionally CA at paths matching generated TOML; ensure correct permissions, securityContext and writable `emptyDir` runtime locations.
- [ ] Deploy `kanidm/radius` with pinned compatible version or safe tag inference; explicit image required if parent is digest-only; do not inherit server digest.
- [ ] Pod: disable API token automount and Service env injection, minimum privileges compatible with official image; explicit read-only secret mounts. Use RollingUpdate with zero unavailable where feasible.
- [ ] Deployment health, termination grace, topology spread and scheduling/resource fields; prove readiness reflects new revision, not old pods.
- [ ] Reconcile `Service` on UDP 1812, ClusterIP default, optional NodePort/LoadBalancer, annotation overrides and `externalTrafficPolicy`; report exposure separately and document source-IP/SNAT limitations.
- [ ] Secret/CA/cert contents changing trigger config revision/hash and pod rollout; input changes must not rotate the Kanidm API token unnecessarily.
- [ ] No deletion/adoption of user-managed input Secrets or cert-manager resources. Verify UID ownership before mutating a generated object.
- [ ] Register focused watch or bounded poll on referenced Secrets/ConfigMaps; minimize `list/watch secrets` permissions and avoid leaked read access through logs.

Files: new `libs/radius/src/reconcile/{config,secret,deployment,service,tls}.rs`, Helm RBAC templates/schema/tests, controller event watchers.

Verify: golden TOML serialization; negative injection tests; Secret/CA hash idempotence; service protocol/schema tests; container startup; owner conflict tests; `helm unittest charts/kaniop`.

### Phase 4 — Overlapping token rotation, recovery, cleanup

- [ ] Explicit crash-consistent state machine: active token A + pending token B; issue B; create B Secret; roll candidate; verify B usable; promote B; revoke A; delete A Secret only after old pods terminate.
- [ ] Define and implement a reliable candidate validation gate **stronger than old-ReplicaSet readiness**; test compatibility with real Kanidm upstream. Never revoke A on mere Deployment progress with unidentified pods.
- [ ] Support periodic rotation and forced recovery from lost config Secret / externally revoked active token. Preserve working A across malformed desired input and Kanidm outages.
- [ ] After operator restart at each state-machine transition, reconcile token labels/IDs, pending Secret/revision and status; recover safely and garbage collect leaked candidates without touching another CR's tokens.
- [ ] Distinguish token renewal from NAS secret/CA/certificate config rollout; do not churn token on non-token config changes.
- [ ] Handle failed Deployment/Service rollout: preserve or roll back previous config/token as appropriate, status `RotationBlocked`; avoid permanent wedge or unbounded candidate creation.
- [ ] Deletion: stop scheduling rotation; drain/delete workload and Service; revoke only owned tokens; remove own membership edge; delete only own Kanidm account; GC generated Secrets; remove finalizer after successful external cleanup.
- [ ] On unreachable Kanidm during finalization, requeue indefinitely with clear condition/event and documented human break-glass process; never silently abandon privileged credentials.
- [ ] Explicitly test namespace/name reuse with new UID, empty/partial status, orphan leftovers, conflicting generated resources, and out-of-order watches.

Files: `libs/radius/src/reconcile/{rotation,finalizer,status}.rs` plus controller tests and real Kanidm fixtures.

Verify: crash-injection/fault matrix; read-only token overlap; successful and failed RADIUS authentication while rotating; deletion safety tests.

### Phase 5 — Packaging, e2e, documentation, upgrades

- [ ] Helm operator ClusterRole with only required verbs for RADIUS resources, Deployments, Services, generated Secrets, referenced CA; update Helm schema/values where adding chart values; Helm unit tests for both default/custom values.
- [ ] Include CRD in generated bundle and Helm CRD lifecycle, webhook/validation if present, CRD migration verification and chart documentation.
- [ ] Add actual `kanidm/radius` E2E fixture with real Kanidm and UDP NAS tester. Cover allowed/denied user, wrong NAS secret, unknown client IP, VLAN, cert/TLS loading, two RADIUS CRs, UDP Service and source IP within Kind test capabilities.
- [ ] E2E Secret input rotation, cert-manager-style TLS renewal, Kanidm CA change, image update, broken config preserving last-good generation, active token revoked, failed B rollout preserving A, crash mid-rotation and finalizer.
- [ ] Add E2E shard (`radius` or existing `misc` with updated shard filters) and test `make check-e2e-shards`. Pin image version; test currently supported and immediately previous Kanidm pairing.
- [ ] Update documentation navigation and publish usage page, clear MikroTik/NAS manifest and cert-manager examples, permissions/Secret/CA/Kanidm-RADIUS-password security model, UDP/SNAT and `externalTrafficPolicy`, migration from a manual deployment without automatic adoption, break-glass deletion, troubleshooting and versioning.
- [ ] Include realistic fields and values in **Rust example generator**, no hand-edits to `examples/`.
- [ ] Review against `REVIEW.md`; independent verification, CI and developer docs; record deviations against plan; ensure no expected-but-unimplemented feature is misrepresented as shipped.

Files: `charts/kaniop/templates/clusterrole.yaml`, `charts/kaniop/tests/clusterrole_test.yaml`, `charts/kaniop/values.schema.json` if needed, `cmd/examples/src/*`, `Documentation/src/{SUMMARY.md,usage/radius.md}`, `tests/e2e/*`, `Makefile`, `.github/workflows/rust.yml` as needed.

Verify: complete e2e matrix, chart tests, docs build, upgrade and CRD migration checks; independent security review.

## Research traceability (must not silently omit)

| Research recommendation | Phase / proof |
| --- | --- |
| Dedicated namespaced CRD; multi-instance; separate lifecycle | 1, 2; two independent CRs |
| Existing Kaniop mail sender reusable patterns | 0, 2–3; mail-sender regression |
| Shared built-in group **additive only** | 2, 4; preserve unrelated members |
| UID-bound identity and no implicit adoption | 2, 4; ownership-conflict tests |
| Read-only Kanidm API token, no admin rights | 2; real RADIUS auth E2E |
| Missing token/secret recovery and two-phase rotation | 4; crash/failure tests |
| Versioned private TOML Secret, no plaintext in CR | 3; log/privacy tests |
| External NAS Secrets, EAP TLS Secret, Kanidm CA | 1, 3; bad-ref and renewal tests |
| Mandatory cert/key in current upstream bootstrap | 0, 3; real image smoke |
| NAS values cannot inject FreeRADIUS clients.conf | 0, 1, 3; adversarial tests |
| Image tag inference and digest-only explicit image | 1, 3; version matrix |
| UDP 1812, source IP, LoadBalancer/NodePort | 3, 5; Kind/provider caveat |
| Secure Pod, no Kubernetes API token | 3; container/security smoke |
| Conditions, observability, no credential leaks | 1–5; status/metrics tests |
| Drift, unavailability, immutable ref, restore guards | 2, 4; failure tests |
| Finalizer cleanup without external Secret deletion | 4; deletion E2E |
| Helm RBAC/CRD, generated examples and docs | 1, 5; generator and chart CI |
| Version skew, upgrade and real RADIUS authentication | 0, 5; pinned end-to-end |
| Manual migration and break-glass, no implicit adoption | 4, 5; runbook |
| Deferred scope: user passwords, cert issuance, accounting | documented in spec; no MVP implementation |

## Dependencies, effort and coordination

Suggested reviewable PR boundaries (may be several PRs chained rather than one giant diff):
1. **Plan and ADR-equivalent design artifacts** (this branch; no runtime behavior).
2. API/controller registration + generated CRD/Helm rules + schema tests.
3. Kanidm account, additive permission and token bootstrap with integration tests.
4. Config Secret + Deployment/Service + real upstream-image E2E.
5. Rotation/finalizer/failure safety, observability and migration docs.
6. Full test matrix, security verification, complete docs, release readiness.

Initial research estimates 17–28 engineer-days including E2E; this is **not** a promised schedule or a measure of work already completed. A minimal feature cannot be called production-ready merely because a controller deploys a pod.

Avoid storing generated secret values in build logs. No upstream repository or production cluster mutations required. Follow existing license, chart and controller patterns.

## Verification checklist

Local (when checked out with toolchain and dependencies):
~~~bash
make agent-framework-check
cargo fmt -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
make test
make crdgen
make examples
git diff --exit-code -- charts/kaniop/crds/crds.yaml examples/
helm unittest charts/kaniop
make check-e2e-shards
~~~

As documented by `AGENTS.md`, **do not run E2E locally by default**. Push implementation PR and let CI run E2E, then reproduce a failed shard only as needed using `make e2e && make e2e-test-shard SHARD=<name>`. Never enable `integration-test` and `e2e-test` together. Review each generated artifact for drift. Run full Kanidm+RADIUS authentication tests and upgrade/rollback tests before declaring readiness.

Verification status for the present planning-only commit:
- [x] Cross-checked repository conventions and upstream radius configuration/bootstrap source.
- [ ] Rust lint/unit tests — implemented unit tests in `radius_config.rs`, GitHub Actions Rust workflow queued/running; no local Rust toolchain.
- [ ] Generated CRD and examples — not applicable until Rust CRD change.
- [x] Helm workflow — passed on configuration renderer commit; no Helm chart changes so far.
- [ ] Integration/E2E authentication — not run.
- [ ] CI all green — dedicated RADIUS image smoke **passed** on aa584cd, Rust jobs pending; full feature not implemented.

## Rollback

During feature development, deleting the CRD/controller is **not** a valid first rollback after external identities have been created: reconcile CR deletions with the finalizer while the controller is alive, verify tokens/group membership/accounts are cleaned, and only then remove CRD/controller. For a failed config/rotation, restore last-known-good versioned configuration and token A first; do not revoke A while its pods exist. For emergency finalizer removal, manually audit/delete the exact Kanidm service account and token IDs owned by the CR UID, then explicitly clear the finalizer. Document this operational runbook before release.

## Deviations

- None yet. The upstream bootstrap cert/key requirement and FreeRADIUS config-injection check were promoted from research observations to explicit hard gates. Record any further deviation here with rationale, impact and corresponding test changes.
