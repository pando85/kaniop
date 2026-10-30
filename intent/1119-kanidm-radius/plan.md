# Implementation plan: KanidmRadiusServer (#1119)

Derived from [spec.md](spec.md); issue [#1119](https://github.com/pando85/kaniop/issues/1119). Status: **in progress — typed CRD, basic status-only controller, safety helpers and generators committed; real provisioning, token lifecycle, full generator/CI verification, E2E and release gates remain outstanding**.

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
- [ ] API review resolves outstanding runtime/health and FreeRADIUS escaping questions; current smoke only tests static configuration, not live authentication.
- [ ] Independent review against `REVIEW.md` before changing from draft to ready.

## Progress log

- **2026-10-30 (independent review / CI audit):** GitHub Actions on `0bb581a` failed Rust compile and generated examples because `SecretProjection` lacked its required `optional` field; fixed in `4313eb3` with `optional: Some(false)`. Published upstream image smoke revealed the assumed `497:497` UID/GID was incorrect; actual `kanidm/radius:1.11.2` identity is **497:496**. Corrected Deployment `runAsGroup/fsGroup` and smoke verification in `0febf08`; CI startup + FreeRADIUS `radiusd -C` then **passed**. Applied Rustfmt diffs from CI in `f997af6`. Hardened teardown to wait for Deployment and Service deletion to finish *as well as* pod drain in `5048ed0`. Restricted default RADIUS image inference to the only tested runtime release, `1.11.2`, in `7960ec2`; other releases require explicit image selection and runtime compatibility validation. **Do not treat these corrections as full feature validation**: newest Rust tests, generated examples, complete controller provisioning/rotation/finalizer, live auth E2E and independent review remain required. The PR is intentionally Draft.

- **2026-10-29:** Implemented and exposed `libs/operator/src/kanidm/radius_config.rs`: typed upstream TOML renderer, mandatory authorization-group checks, client name/IP/CIDR/shared-secret validation, fixed TLS file paths, safe error messages and unit tests. Verified upstream v1.11.2 config/bootstrap source shape. This is not yet wired to a controller. Also implemented `libs/operator/src/kanidm/radius_tls.rs` with OpenSSL-based leaf PEM/private-key correspondence, validity-date and optional CA bundle checks; added unit tests. GitHub Actions verification remains the source of truth; E2E with real Kanidm RADIUS has not run.
- **Remaining immediate work:** validate generated TOML against the pinned v1.11.2 runtime; connect EAP PEM certificate/key preflight and test it with the official container; add UID ownership tests; then implement the CRD and controller wiring using a real Rust toolchain for mandatory generated artifacts.

- **2026-10-29 (additional):** Added `tests/e2e/scripts/radius-bootstrap-smoke.sh` and `.github/workflows/radius-bootstrap-smoke.yml`. Initial CI failed because the host-created fixture directory was mode 0700 and unreadable by non-root `radiusd`; fixed it. Published `kanidm/radius:1.11.2` image **passed** the dedicated bootstrap workflow on commit `aa584cd`, verifying expected configuration, EAP cert/key file paths and the generated `require_message_authenticator = yes` setting. This is a startup smoke test, **not** live Kanidm authentication. Added pure `radius_identity.rs` helper and unit tests for immutable Kubernetes UID-derived Kanidm identity names and non-adoption checks. Added `radius_image.rs` with tests: infer Docker Hub RADIUS version only from a pinned official Kanidm server image tag, never inherit the server image digest, require explicit image for nonstandard registries/tags. Neither helper is wired to a running controller yet. Full Rust CI on latest branch is pending.

- **2026-10-29 (workload milestone):** Added `radius_workload.rs`, a pure, UID-owned Deployment and UDP/1812 Service builder. It uses one projected `/data` volume combining revisioned generated config, externally managed EAP TLS Secret and optional Kanidm CA, with API token automount and service-env injection disabled, capabilities dropped and privilege escalation forbidden. Added unit tests for UDP exposure, owner UID, config revision rollout and invalid input. **Not yet reconciled by a controller.** Added published image UID inspection to bootstrap smoke script; its newest CI run is pending. Previously queued Rustfmt CI surfaced formatting drift in `radius_config.rs`; applied the reported formatting changes. Full Rust test/CI outcome for the latest code is still pending.

- **2026-10-29 (API milestone):** Added `radius_crd.rs` defining a namespaced `KanidmRadiusServer` with typed authorization/client Secret references, EAP TLS, Kanidm CA, UDP Service, rotation policy, scheduling/resources, status conditions and token IDs. Updated nested `radiusTls.secretRef` and `kanidmTls.caBundleRef` to match the proposed manifests and rejected inline NAS shared-secret fields at deserialization. **This is Rust CRD source only.** Do not register it in `cmd/crdgen` or claim the API is installed until `make crdgen` and `make examples` can produce and verify the corresponding artifacts, with controller and Helm packaging wired in. No handwritten generated files.

- **2026-10-30 (continuation):** Fixed the compile-time Kubernetes Secret projection name mismatch and formatting issues from CI; pinned Docker smoke test confirmed published image UID 497. Added dedicated `radiusServerNamespaceSelector` to Kanidm with shared `KanidmResource` integration, read-only NAS/TLS/CA preflight in `radius_preflight.rs`, UID-bound no-adoption account and additive `idm_radius_servers` membership helper in `radius_identity_reconcile.rs`, a pure overlapping token-rotation decision model in `radius_rotation.rs` requiring exact token/revision **real authentication evidence** plus old pod drain before revocation, and `radius_secret.rs` immutable owner-verified generated config Secrets. These helpers are **not yet orchestrated by a running controller**; live Kanidm, rotation and recovery E2E are still mandatory. Registered Rust CRD/example generators; the branch-specific generator failed because `charts/kaniop/crds` is Git-ignored, and the workflow was fixed to force-add only the generated bundle and rebase before pushing. The corrected generator run and Rust CI are still pending. The temporary generation workflow must be removed after generated files are committed and verified. Do not mark the feature complete.

- **2026-10-30 (generated artifact audit):** The Rust generators successfully emitted the `KanidmRadiusServer` CRD and `examples/radius-server.yaml`. However, `charts/kaniop/crds/` is intentionally ignored by `.gitignore`, the main branch does not track the generated bundle, and the existing Helm release workflow runs `make crdgen` when packaging. The one-off workflow force-added an 876 KB generated bundle and its bot commit caused `action_required` check statuses with **no jobs executed**. Remove the mistakenly tracked CRD bundle and temporary write-enabled workflow, and use the repository's existing `make crdgen` release path and source-level CRD schema tests. Retain the generated example (examples are tracked). CI green for the latest human/agent commit is still mandatory; bot `action_required` is not a pass. This supersedes earlier checklist wording that incorrectly instructed committing the ignored CRD bundle.
- **2026-10-30 (controller and lifecycle scaffolding):** Added `radius_controller.rs` to both leader-elected and ordinary operator execution paths using the shared namespace authorization, Kanidm restore guard, error backoff and safe `ConfigurationReady` condition. The controller is **deliberately fail-closed and reports Ready=false**; it has **not** yet wired credential provisioning, Deployment/Service apply, token promotion or finalization, so a RADIUS CR is not operational. Added `radius_token.rs` for unambiguous **read-only** candidate token issuance with unique generation labels and no destruction of existing tokens, `radius_cleanup.rs` for owner-verified workload deletion and pod-drain gating, `cleanup_radius_identity_after_drain` for UID-owned account/token/group cleanup only after drain, `radius_apply.rs` for owner-verified Deployment/UDP Service apply, and `radius_generation.rs` for bounded UID-bound Secret revision names. These pieces must be orchestrated, integration-tested and independently reviewed. CI found Clippy/Rustfmt failures on earlier code and subsequent fix commits addressed the reported issues; latest jobs still queued. **CRD generation remains unverified**; initial workflows failed on ignored output and unstaged generator files, corrected workflow queued/running. Do not treat helper implementations as completed acceptance criteria.

- **2026-10-30 (persisted-state safety milestone):** Added a pure recovery classifier for UID-bound active/pending token IDs, generation UUIDs and immutable Secret names. It supports a persisted pre-issuance reservation, rejects partial issued checkpoints, duplicate generations and foreign Secret names, and gates the read-only controller's preflight status. Tightened the existing finalization helper: a syntactically valid `kaniop-radius-*` label is no longer sufficient to revoke a token; the exact token ID and generation must be recorded in CR status. Ambiguous/orphaned tokens now block automatic cleanup pending an explicit recovery protocol. Unit tests cover status loss, foreign identifiers and overlap. **No external provisioning/finalizer orchestration or real authentication probe is wired yet; CI for this commit is unverified.**

## Focused review findings (2026-10-30, PR #1120)

This is a source-level review against `REVIEW.md`, **not** an independent sign-off or a successful CI result. Do not remove any gate until implemented and actually verified.

1. **Critical — API is not operational:** `radius_controller.rs::reconcile_radius` intentionally reports `Ready=false` and invokes only read-only preflight/status; it does not call `ensure_radius_identity`, issue/read tokens, persist config Secrets, apply Deployment/Service, or report a real ready workload. The helper modules do not constitute an implemented controller.
2. **Critical — no persistent lifecycle or finalizer:** There is no wired crash-safe active/pending token persistence, live auth-probe promotion, old-pod drain/revocation, or finalizer cleanup path. Do not wire token issuance/destruction without the complete recovery matrix and E2E proof.
3. **Important — incompatible image UID risk:** The workload uses numeric UID/GID 497 from the official `kanidm/radius:1.11.2` image. The updated smoke test now checks that **both** IDs match the effective Kubernetes identity, but the newest test is queued; alternate tags and custom image overrides have no verified UID/GID compatibility policy. Limit support to tested pairs or add explicit, safe identity compatibility handling.
4. **Important — real authentication/rollout is untested:** A successful `radiusd -C` checks FreeRADIUS syntax only, not Kanidm read-only token validity or user RADIUS login. E2E must cover allowed/denied authentication, NAS secrets, VLAN, TLS, two CRs, failed candidate rollout, retry/restart and finalizer.
5. **Important — CI cannot currently certify the branch:** The generated-artifact bot commit `c458320` produced `action_required` check runs with **no jobs**. Subsequent non-bot commits triggered checks, but Rust/Helm/Pre-commit/verify-examples/smoke runs are still **queued**, not green. The temporarily force-added ignored 876 KB CRD bundle and write-enabled generator workflow were removed in `43e2fff`; Kaniop normally generates ignored CRDs during Helm release, while generated examples are tracked. The CRD schema now includes typed ConfigMap/Secret CA refs, so `make examples` must be rerun and its tracked output checked, not hand-edited.

Other remaining requirements: status conditions/observability, Helm RBAC audit, documentation, version-compatibility matrix, independent security review, and maintainer approval. **PR must remain draft.**

## Ordered implementation steps

### Phase 0 — Foundation and safety invariants

- [ ] Freeze the full upstream Kanidm and `kanidm/radius` compatibility matrix. Official `kanidm/radius:1.11.2` image bootstrap smoke passed; older supported releases and real authentication not tested.
- [x] Reproduce bootstrap startup with cert/key mount, default writable paths, non-root restrictions and `require_message_authenticator` with the official v1.11.2 image in GitHub CI (commit aa584cd). This does not test K8s SecurityContext or real authentication.
- [ ] Confirm Kanidm API token scope (read-only) authenticates an actual RADIUS request when service account belongs to `idm_radius_servers`.
- [ ] Record and enforce full API/naming invariant: immutable Kanidm reference, dedicated UID-bound service-account identity, no account adoption, per-resource token labels, first-release same-namespace input Secret refs. UID name generator/unit tests implemented, but external identity reconciliation and conflict checks not wired yet.
- [ ] Implement narrowly-scoped reusable additive-membership and CA digest helpers **only if safe to extract**; add regression tests proving existing mail sender behavior unchanged. Do not refactor unrelated reconcilers.

Files: `libs/operator/src/kanidm/reconcile/mail_sender.rs`, `libs/operator/src/crd.rs` or narrow new integration helper modules, `tests/e2e/` if needed.

Verify: focused unit tests for membership and CA digest; existing mail-sender test filter; upstream image smoke in CI.

### Phase 1 — CRD, validation, registration, status

- [x] Define initial `KanidmRadiusServerSpec` and `KanidmRadiusServerStatus` as Rust source in `libs/operator/src/kanidm/radius_crd.rs`, using shared immutable `KanidmRef` and list-map conditions. Its generated CRD and controller integration remain incomplete.
- [ ] Add scope-specific `radiusServerNamespaceSelector` on Kanidm if consistent with controller authorization; ensure default and selector removal block mutation. Source field and KanidmResource trait integration implemented, **controller enforcement/E2E not yet wired**.
- [ ] Fields: Kanidm reference; image; replicas; required groups; default VLAN; group VLAN mappings; NAS clients and Secret keys; Kanidm CA ref; required EAP certificate/key Secret; Service UDP configuration; resources/scheduling; rotation period.
- [ ] Schema/CEL + runtime validation for names, unique client names/CIDRs, nonempty NAS refs and secrets, VLAN ranges, mandatory groups/TLS and valid Service settings; fail closed.
- [ ] Add condition-list map annotation keyed by type; status: observedGeneration, identity/config/availability/exposure conditions, deployment, Service, active/pending token IDs/generations, config revision and ready replicas; no plaintext or secret hashes disclosed.
- [ ] Register a **fully functional** controller in `cmd/operator/src/main.rs` using shared context, existing leader-election, error backoff and Kanidm client/restore guard. Basic status/preflight controller wiring is committed; full provisioning and finalization are NOT yet connected.
- [ ] Add associated dependency, feature flags and CRD generation entries, webhook/CEL validation integration if applicable; generate CRD **using make crdgen** and verify the resulting schema; do not manually edit or force-commit the ignored CRD output.
- [ ] Add exemplar Rust generator in `cmd/examples`; commit generated examples only via `make examples`.
- [ ] Include typed API unit tests, CRD schema tests, default/validation tests.

Likely files: `libs/radius/{Cargo.toml,src/lib.rs,src/crd.rs,src/controller.rs,src/reconcile/*}` (or an equivalent minimal in-tree module based on current dependency graph), root `Cargo.toml`, `Cargo.lock` if changed, `cmd/operator/{Cargo.toml,src/main.rs}`, `cmd/crdgen/{Cargo.toml,src/main.rs}`, `cmd/examples/{Cargo.toml,src/...}`, `libs/operator/src/kanidm/crd.rs`, `charts/kaniop/crds/crds.yaml` **generated, ignored and packaged by Helm release**, `examples/radius-server.yaml` **generated and tracked**.

Verify: `cargo fmt --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; targeted CRD tests; `make crdgen` and `make examples` with clean subsequent diff.

### Phase 2 — Kanidm-side identity and ownership

- [ ] Generate collision-resistant, UID-bound service-account name and description/identity marker within Kanidm constraints; detect pre-existing unmanaged identity as `OwnershipConflict`. Never update/adopt unrelated account.
- [ ] Create own dedicated Kanidm service account and add it to `idm_radius_servers` *additively* using `idm_group_add_members`; idempotent membership get; never `idm_group_set_members`. A helper exists but live controller invocation and E2E are not implemented.
- [ ] Issue least-privilege read-only token with uniquely identifiable label/ID. Persist bearer only in generated Kubernetes Secret, token IDs in status.
- [ ] Restore service account membership after out-of-band removal; keep every unrelated group member. Refuse to delete the built-in group.
- [ ] Prevent writes during Kanidm restore; handle 403, 404, timeouts, cancellation and 5xx. Isolate permissions; do not grant `idm_radius_service_admins`.
- [ ] Add finalizer at appropriate point before external mutations; test orphan-prevention under crashes.
- [ ] Make 0..N RADIUS CRs for one Kanidm safe: each owns one account, its token IDs and its membership edge.

Files: new `libs/radius/src/{controller,reconcile/identity,reconcile/token,reconcile/status}.rs`; shared metrics/helpers only as needed.

Verify: unit+real Kanidm integration tests for UID conflicts, membership preservation with two RADIUS CRs, read-only token, restart and repeated reconcile, parent restore and degraded Kanidm.

### Phase 3 — Kubernetes configuration, Deployment and Service

- [ ] Resolve NAS Secret keys and EAP cert/key/CA; validate complete configuration **before** modifying active Kanidm credentials or replacing Kubernetes workloads. Pure read-only preflight implemented; runtime controller must call it prior to any mutation.
- [ ] Safely generate upstream-compatible deterministic TOML and escape all strings; **validate NAS client names and secrets against upstream unescaped FreeRADIUS clients.conf interpolation** to forbid newline, braces and syntax-breakers. Test adversarial inputs.
- [ ] Generate owned, revision-addressed Kubernetes `Secret` containing all sensitive `radius.toml` data; immutable-ish generations avoid mixed runtime state; do not place token/NAS secret in ConfigMap, status, annotations or logs. Pure immutable Secret builder, owner/content verification and idempotent create are implemented; no controller generation naming/status/rotation lifecycle yet.
- [ ] Mount generated Secret at `/data/radius.toml` as expected by upstream. Mount external EAP cert/key and optionally CA at paths matching generated TOML; ensure correct permissions, securityContext and writable `emptyDir` runtime locations.
- [ ] Deploy `kanidm/radius` with pinned compatible version or safe tag inference; explicit image required if parent is digest-only; do not inherit server digest. Image selection helper/unit tests exist; controller wiring and runtime image version matrix remain.
- [ ] Pod: disable API token automount and Service env injection, minimum privileges compatible with official image; explicit read-only secret mounts. Use RollingUpdate with zero unavailable where feasible.
- [ ] Deployment health, termination grace, topology spread and scheduling/resource fields; prove readiness reflects new revision, not old pods.
- [ ] Reconcile `Service` on UDP 1812, ClusterIP default, optional NodePort/LoadBalancer, annotation overrides and `externalTrafficPolicy`; report exposure separately and document source-IP/SNAT limitations.
- [ ] Secret/CA/cert contents changing trigger config revision/hash and pod rollout; input changes must not rotate the Kanidm API token unnecessarily.
- [ ] No deletion/adoption of user-managed input Secrets or cert-manager resources. Verify UID ownership before mutating a generated object.
- [ ] Register focused watch or bounded poll on referenced Secrets/ConfigMaps; minimize `list/watch secrets` permissions and avoid leaked read access through logs.

Files: new `libs/radius/src/reconcile/{config,secret,deployment,service,tls}.rs`, Helm RBAC templates/schema/tests, controller event watchers.

Verify: golden TOML serialization; negative injection tests; Secret/CA hash idempotence; service protocol/schema tests; container startup; owner conflict tests; `helm unittest charts/kaniop`.

### Phase 4 — Overlapping token rotation, recovery, cleanup

- [ ] Explicit crash-consistent state machine: active token A + pending token B; issue B; create B Secret; roll candidate; verify B usable; promote B; revoke A; delete A Secret only after old pods terminate. Pure decision model enforces exact token/revision probe evidence and old-pod drain, but persisted transitions, Kanidm APIs and real auth probe are NOT implemented.
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
git diff --exit-code -- examples/\n# CRD bundle is gitignored on master; validate generated file exists and contains\n# the KanidmRadiusServer schema, rather than relying on git diff.
helm unittest charts/kaniop
make check-e2e-shards
~~~

As documented by `AGENTS.md`, **do not run E2E locally by default**. Push implementation PR and let CI run E2E, then reproduce a failed shard only as needed using `make e2e && make e2e-test-shard SHARD=<name>`. Never enable `integration-test` and `e2e-test` together. Review each generated artifact for drift. Run full Kanidm+RADIUS authentication tests and upgrade/rollback tests before declaring readiness.

Verification status for the present planning-only commit:
- [x] Cross-checked repository conventions and upstream radius configuration/bootstrap source.
- [ ] Rust lint/unit tests — implemented targeted tests in `radius_config.rs`, `radius_tls.rs`, `radius_identity.rs`, and `radius_image.rs`; latest Rust CI queued/running; no local Rust toolchain.
- [ ] Generated CRD and examples — Rust generators emitted both; tracked example committed. Re-run make crdgen/make examples and validate new CRD/schema in CI before marking complete; chart release regenerates ignored bundle.
- [x] Helm workflow — passed on configuration renderer commit; no Helm chart changes so far.
- [ ] Integration/E2E authentication — not run.
- [ ] CI all green — dedicated RADIUS image smoke **passed** on aa584cd, Rust jobs pending; full feature not implemented.

## Rollback

During feature development, deleting the CRD/controller is **not** a valid first rollback after external identities have been created: reconcile CR deletions with the finalizer while the controller is alive, verify tokens/group membership/accounts are cleaned, and only then remove CRD/controller. For a failed config/rotation, restore last-known-good versioned configuration and token A first; do not revoke A while its pods exist. For emergency finalizer removal, manually audit/delete the exact Kanidm service account and token IDs owned by the CR UID, then explicitly clear the finalizer. Document this operational runbook before release.

## Deviations

- Runtime compatibility is deliberately limited to automatic inference of `kanidm/radius:1.11.2`, because UID 497/GID 496 and its FreeRADIUS bootstrap are verified only for that upstream release. Other tags require explicit image selection, but the current workload still pins UID 497/GID 496: the chosen image MUST be compatible with that runtime identity and bootstrap paths, or support for a different UID/GID must be implemented and tested first. Expand the matrix with image smoke and Kubernetes E2E before allowing auto-inference. The upstream bootstrap cert/key requirement and FreeRADIUS config-injection check were promoted from research observations to explicit hard gates. Record any further deviation here with rationale, impact and corresponding test changes.

### 2026-10-30 CI diagnosis (head `573dae2`, before default fix)

- Rustfmt and Clippy succeeded, but x86_64 and aarch64 Rust unit jobs failed: `radius_preflight::tests::rejects_invalid_spec_before_reading_any_secrets` received `InvalidSpec` for a valid fixture. Root cause: `RadiusTokenRotationSpec` derived Rust `Default` with `period_days = 0`, while Serde's field default is 30; omitted `tokenRotation` therefore fails preflight. Fix: explicit `Default` and regression assertions for omitted tokenRotation.
- All 16 existing e2e shards failed at Helm CRD installation before tests: the same invalid `periodDays: 0` schema default violates CEL 1..365. The explicit Rust `Default` fixes this source-level root cause; re-run Kind jobs to verify the generated CRD installs.
- `verify-examples` failed because `make examples` changed `examples/radius-server.yaml` CA-reference comments after the typed ConfigMap/Secret ref change. The example **must be regenerated by `make examples`** in a Rust-capable checkout and committed; do not manually copy the CI diff, hand-edit the generated YAML, add a write-enabled generator workflow, or track ignored Helm CRDs.
- Helm, pre-commit and published RADIUS bootstrap smoke succeeded at that head. CI results on any subsequent commit must be checked separately; real Kanidm-backed RADIUS login and lifecycle/finalizer E2E remain absent.
