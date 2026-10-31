# Implementation plan: KanidmRadiusServer (#1119)

Derived from: [spec.md](spec.md) (rewritten in the same commit as this plan; previous baseline `89f75986`). Issue: [#1119](https://github.com/pando85/kaniop/issues/1119), PR [#1120](https://github.com/pando85/kaniop/pull/1120).

## Status

**Refactor planned and approved; WP-0 to WP-4 done.**

The branch currently contains a typed CRD, a finalizer-backed controller, a pile of helper modules under `libs/operator/src/kanidm/radius_*.rs` (16 files, about 4,800 lines) and a token lifecycle driven by a status "generation journal" (active/pending checkpoint fields). That journal is the root cause of most review findings: any loss of status, config Secret or ordering wedges the CR in a pending state. The refactor replaces the journal with a **world-derived lifecycle**: every reconcile reads Kanidm and Kubernetes, and a pure planner decides what to do. Status becomes a cache and report only.

CI blockers already fixed and committed (do not redo):

- [x] `a185daf1` rustfmt on the RADIUS modules.
- [x] `d1f54ee7` CRD list bounds (`maxItems` 100, `maxLength` 63/253) to fit the CEL cost budget.
- [x] `63113521` crd-migrator installs the KanidmRadiusServer CRD on upgrade.
- [x] `89f75986` Chart.yaml lists KanidmRadiusServer in the artifacthub CRDs.

This file is the single source of truth that coding agents follow. Do not execute from memory of earlier plans. A PR stays draft until WP-9 and Iteration 2 pass.

## Scope

In scope: move the feature into a new crate `libs/radius` (`kaniop-radius`), rebuild the token lifecycle as a pure planner plus executors, rebuild status/finalizer/wiring, add RBAC, an e2e shard with live RADIUS authentication, user documentation, then a review/fix iteration.

Out of scope (see `intent.md` and `spec.md` non-goals): user RADIUS credential management, accounting (1813), raw FreeRADIUS overrides, operator-managed PKI, adoption of existing accounts, **any new CRD spec field for the Kanidm URL**, reuse of `KanidmServiceAccount`.

## Decisions (authoritative)

Human decisions override the earlier design text wherever they differ.

| # | Decision | Status | Notes |
|---|---|---|---|
| D1 | Architecture: world-derived token lifecycle; new crate `libs/radius`; no reuse of `KanidmServiceAccount` | Approved | Service-account rotates by delete-then-create (no overlap), overwrites Secrets in place, deletes tokens not in spec, has no UID marker in account names and no group-membership support. Its helpers are one-call wrappers; extraction gains nothing. |
| D2 | Promotion gate: `whoami` with the candidate token **before** rollout; revoke an old token only when unused by the Deployment template, every ReplicaSet with replicas, and every non-terminal pod | Accepted | Rollout uses `maxUnavailable: 0`. No live RADIUS probe (operator holds no user credential); readiness alone is never claimed as auth continuity. |
| D3 | Token expiry: `issued + 2 x periodDays` when rotation is enabled, none otherwise | Accepted | Rotation age is read from Kanidm `ApiToken.issued_at`. |
| D4 | Manual rotation via CR annotation `kaniop.rs/radius-rotate-token` | Accepted | Compared with the value recorded on the config Secret. For leaked-token incidents. |
| D5 | `kanidmUri` CRD field | **Rejected** | No new CRD spec field for the Kanidm URL. URI keeps deriving from `Kanidm.spec.origin`, else `https://{domain}`. The e2e shard reaches Kanidm over verified TLS with test-only means (see D6). Remove from WP-2 and every spec/CRD text. |
| D6 | E2E connectivity to Kanidm | Open, decided by WP-7 spike | Candidates: (a) give the e2e Kanidm CR `domain: idm.example.com` plus `origin: https://idm.example.com:8443` (matches the e2e certificate SAN `idm.example.com`) and a CoreDNS rewrite to the Kanidm Service; (b) regenerate the e2e certificate with extra SANs. The spike must check which port the derived URI uses (no port means 443) against the Service port (8443). Record the outcome in the Deviations section. |
| D7 | Deletion edge cases: if the Kanidm account exists but is not ours, or the referenced Kanidm CR is gone, skip Kanidm cleanup with a Warning event and remove the finalizer | Accepted | An unreachable Kanidm (CR present) still retries with backoff. |
| D8 | `readOnlyRootFilesystem` left unset, rationale documented and asserted by a test | Accepted | Upstream bootstrap writes `/etc/raddb/clients.conf` and `/etc/raddb/certs`. Revisit only if the WP-2 spike finds a working writable-path set. |
| D9 | Delete `radius-bootstrap-smoke` workflow and script | Accepted | Static-config smoke is superseded by the e2e shard using the real renderer. |
| D10 | Process: Iteration 1 (WP-0..WP-9) ends with the local behavioral gate (WP-9) before any review; Iteration 2 = Opus correctness review + maintainability review, fixes with failing-first tests, re-run WP-9, then push | Accepted | One agent at a time. |
| D11 | Config Secret name derived from content: `<cr>-<sha256(radius.toml)[..10]>`, immutable | Kept | Same practice as kustomize secretGenerator. |
| D12 | TLS/CA input changes roll pods via template annotation `kaniop.rs/radius-inputs-digest` (hash of public cert and CA bytes only) | Kept | A key cannot change without its certificate changing. |
| D13 | `status.retiringTokenIds` is informational only; no generation counter in names | Changed | Replaces the old active/pending checkpoint fields. |

## Target architecture

### Crate layout (`libs/radius/src/`)

| File | Responsibility |
|---|---|
| `lib.rs` | crate root |
| `crd.rs` | CRD types (moved from `radius_crd.rs`); root CEL rule `size(self.metadata.name) <= 52`; `Ready`/`Endpoint` print columns |
| `controller.rs` | Context with Deployment, Service and RADIUS stores; `run()` |
| `reconcile/mod.rs` | `#[instrument]`, trace id, `reconcile_count_and_measure`, restore guard, `ResourceNotWatched` event, finalizer, orchestration |
| `reconcile/inputs.rs` | former preflight; Kanidm URI resolution; public-material digest |
| `reconcile/config.rs` | TOML renderer; CIDR normalization for duplicate-client detection |
| `reconcile/tls.rs` | PEM/key validation (moved) |
| `reconcile/image.rs` | image inference (moved); named `RADIUS_UID`/`RADIUS_GID` constants (497/496) |
| `reconcile/identity.rs` | naming, ensure account and additive membership, delete |
| `reconcile/token.rs` | pure planner plus mint, verify, revoke |
| `reconcile/observe.rs` | builds planner input from Kanidm and API lists (not the cache) |
| `reconcile/workload.rs` | Deployment, Service and config Secret builders; one labels/annotations module; Service type as an enum; `revisionHistoryLimit: 1` |
| `reconcile/apply.rs` | generic `ensure_not_foreign<K>` then SSA `.force()`; create-only Secret; delete with UID + resourceVersion preconditions |
| `reconcile/status.rs` | conditions; status applied with SSA `.force()` |
| `reconcile/cleanup.rs` | finalizer |

Only coupling back to `kaniop-operator` is `Kanidm.spec.radius_server_namespace_selector` (stays in the operator crate, like `service_account_namespace_selector`). Reuse from `kaniop_operator::controller::*` (Context, `backoff_reconciler!`, watcher helpers, `is_resource_watched`, `kanidm_write_allowed`, `record_kanidm_sdk_call`) and `kaniop_k8s_util` (`Error::kube_error`/`kanidm_client_error`, `last_transition_time`, `normalize_spn`). Code that changes imports: `cmd/{operator,crdgen,examples,crd-migrator}` and `tests/Cargo.toml`.

Removed by the refactor: `radius_bootstrap` (319 lines), `radius_rotation` (490), `radius_generation` (65), most of `radius_identity_reconcile` (464 to about 150) and `radius_cleanup` (235 to about 80); `radius_token` and `radius_secret` shrink by about half. Net 1,573 to about 400 lines in the lifecycle code.

### What each reconcile observes

Status is a cache. The reconciler reads:

- `K`: tokens on the owned Kanidm account (id, `issued_at`, expiry). The account is dedicated to one CR UID, so every token on it belongs to this CR; deleting the account removes all tokens.
- Owned config Secrets (immutable), each annotated `kaniop.rs/radius-token-id` and `kaniop.rs/radius-rotation-request`, with `auth_token` and `radius.toml` in data.
- The token id the Deployment pod template points at, and the bearer read from that template's Secret.
- **In-use tokens:** the template, plus every ReplicaSet owned by our Deployment that still has replicas, plus every non-terminal pod in the namespace (Terminating pods count). A pod counts if it carries our token-id annotation or mounts one of our owned Secrets. Scanning pods rather than a label closes the "label-only drain" gap.

### Token planner (`plan_tokens(world, policy, now) -> TokenPlan`, pure)

| Field | Outcome |
|---|---|
| current | **Keep(template token)** if it is in `K`, its bearer is readable, it is not expired or inside the safety margin (1 day), rotation is not due (`issued_at + periodDays`), and the CR `kaniop.rs/radius-rotate-token` annotation is absent/empty or equals the value recorded on the Secret. The template's token is the one named by the token-id annotation on the template's config Secret (written with the bearer); the template's own annotation only feeds the in-use set |
| | Otherwise **Mint** with reason Initial (no template or no reference), BearerLost (Secret missing, bearer unreadable, token id unparsable, or token gone from `K`), Expired (expired or inside the margin), Due or Requested |
| | **Blocked(TooManyTokens)** instead of Mint if `K` already holds 4 tokens, so a mint never raises the count above 4 (stops a crash loop from minting without limit) |
| revoke | Any token other than the current one that is not in use and is at least 10 minutes old (orphan grace, applied to retired and orphan tokens alike). Nothing is revoked while any consumer reference is unresolved (unparsable token-id annotation, mounted Secret missing or without a parsable token id, or a consumer with no reference at all) |
| gc_secrets | Owned config Secrets not referenced by the template, a ReplicaSet or a pod, except those whose token is younger than the grace |

### Reconcile order

1. Fetch/authorize `kanidmRef`; restore guard; namespace selector; `ResourceNotWatched` event when unauthorized (no finalizer).
2. Install finalizer, then validate inputs (read-only preflight) before any mutation.
3. Ensure Kanidm account (UID-bound, no adoption) and additive `idm_radius_servers` membership.
4. Observe the world; compute `TokenPlan`.
5. If minting: mint read-only token (expiry `2 x periodDays` when rotation enabled), then `whoami` with the candidate token. A failing candidate never reaches pods.
6. Create the immutable config Secret if absent (on 409 verify owner and content).
7. Apply Deployment (SSA, foreign-object check) and Service.
8. Re-observe; revoke and garbage-collect per plan.
9. Write status (conditions first, also on error paths).

A crash at any step leaves either an orphan token (revoked after the grace period) or an unused Secret (garbage-collected). Neither revokes a token a pod uses. A config change never mints a token; it re-renders using the template's bearer. Status loss changes nothing.

### Wiring

- `CONTROLLER_ID = "radius-server"` (unchanged); field manager `kanidmradiusservers.kaniop.rs`; generated objects labelled `managed-by=kaniop-radius-server`.
- `.owns_shared_stream` for Secrets (metadata watcher), Deployments and Services.
- `.watches_stream` on metadata-only watchers for Secrets and ConfigMaps, mapped by name to the RADIUS CRs that reference them (pattern: `libs/operator/src/kanidm/controller/mod.rs:354-378`).
- `reconcile_all_on(reload)`, debounce 500 ms. Add `KANIDM_RESOURCE_RADIUS_SERVER` for metrics.
- Errors: transient API failures propagate as `k8s_util::Error` (keeps the source) and drive backoff. Expected blocks use a `RadiusBlock` thiserror enum with `reason()`; Display text contains no secrets. Both paths write the condition first; no `Err(_)` flattening.

### Status model

`observedGeneration`, `ready`, conditions keyed by `type`, all five always emitted: `IdentityReady`, `ConfigurationReady`, `Available`, `Exposed`, `Rotating`. Plus `tokenId`, `tokenIssuedAt`, `configSecretName`, `deploymentName`, `serviceName`, `replicas`, `readyReplicas`, `endpoint`, `retiringTokenIds`. Removed: all active/pending checkpoint fields and `observedConfigRevision`. `ready` = identity ready AND configuration ready AND rollout complete and available AND exposed. Token IDs only; never token values or NAS secrets.

### Finalizer

1. Delete the Deployment in the foreground.
2. Wait until no pod or ReplicaSet references our Secrets.
3. Remove our membership edge and delete the account (which removes all tokens).
4. Account exists but is not ours: skip it, emit a Warning event (D7).
5. Referenced Kanidm CR gone: skip Kanidm cleanup, emit a Warning event, remove finalizer (D7).

Owner-reference GC removes Secrets and the Service. An unreachable Kanidm retries with backoff.

## Work packages (Iteration 1)

Each WP must pass `make lint` and `cargo test -p kaniop-radius` (plus listed extras). Commits use scope `radius` (valid in `.commitlintrc.json`, verified) and DCO sign-off. Run one agent at a time. Update this checklist in the same commit that completes a WP; if a WP departs from this plan, record it under Deviations.

- [x] **WP-0 Plan rewrite** (model: Sonnet)
  - Goal: make `plan.md`/`spec.md` the source of truth for the refactor.
  - Files: `intent/1119-kanidm-radius/{plan,spec,intent}.md`.
  - Acceptance: `make agent-framework-check`.
  - Commit: `docs(radius): replan RADIUS server around world-derived token lifecycle`

- [x] **WP-1 Move into the new crate** (Sonnet, mechanical)
  - Goal: relocate with no behavior change.
  - Files: `git mv` all 16 `libs/operator/src/kanidm/radius_*.rs` into `libs/radius/src/` (path fixes only); new `libs/radius/Cargo.toml` copied from service-account plus `thiserror`, `url`, `openssl`, `sha2`/`hex` from the workspace; root `Cargo.toml` members and dependencies; `cmd/{operator,crdgen,examples,crd-migrator}` imports; metrics controller list (`cmd/operator/src/main.rs`); `tests/Cargo.toml`; remove the radius modules from `libs/operator/src/kanidm/mod.rs`.
  - Tests: existing tests move unchanged and still pass.
  - Acceptance: `make lint`, `cargo test -p kaniop-radius`, `make crdgen`, `make examples` with empty `git diff examples/`.
  - Commit: `refactor(radius): move KanidmRadiusServer into kaniop-radius crate`

- [x] **WP-2 Pure building blocks and CRD spec** (Sonnet)
  - Goal: pure builders and small CRD changes; no Kanidm URL field.
  - Files: `reconcile/{workload,config,image,inputs}.rs`, `crd.rs`, `cmd/examples` updates.
  - Work: content-addressed config Secret builder; inputs digest; CIDR normalization; `RADIUS_UID`/`RADIUS_GID` constants; Service type enum; `revisionHistoryLimit: 1`; root CEL rule `size(self.metadata.name) <= 52` (verify it fits the CEL cost budget fixed in `d1f54ee7`); `Ready`/`Endpoint` print columns; `readOnlyRootFilesystem` rationale as a code comment plus assertion.
  - Failing-first tests: an unknown Service type returns the correct error variant (today wrong variant at `radius_workload.rs:123`); `10.0.0.1` and `10.0.0.1/32` detected as duplicates.
  - Other tests: same TOML gives same Secret name, different TOML a different name; digest changes with cert or CA change and ignores metadata noise; no secret in metadata; template carries token-id and digest annotations; the CRD has no `kanidmUri`; name-length CEL rule rejects 53 characters; `readOnlyRootFilesystem` is unset.
  - Spike: run the upstream image with `docker run --read-only` to settle D8; record the result under Deviations.
  - Acceptance: `make crdgen`, `make examples`, `make lint`, `cargo test -p kaniop-radius`.
  - Commit: `feat(radius): add content-addressed config and workload building blocks`

- [x] **WP-3 Token planner** (**Opus**, subtlest piece)
  - Goal: `reconcile/token.rs` pure planner per the table above.
  - Tests (table-driven, written first): empty world mints (Initial); status loss with valid template token keeps it; config change keeps the token; due, expired, requested and bearer-lost each mint; old token held while any pod, ReplicaSet or the template uses it; retired token revoked once unused; orphan revoked only after the 10-minute grace; more than 4 tokens blocks minting; the current token is never revoked; foreign or unparsable annotations never cause revocation of an in-use token.
  - Acceptance: `make lint`, `cargo test -p kaniop-radius`.
  - Commit: `feat(radius): plan token rotation from observed Kanidm and pod state`

- [x] **WP-4 Executors** (Sonnet)
  - Goal: effectful pieces the planner drives.
  - Files: `reconcile/{identity,observe,apply,token}.rs`.
  - Work: identity ensure (additive membership, UID-bound, no adoption) and delete; observe builds planner input from API lists (pods across the namespace, owned ReplicaSets, owned Secrets, token list); mint (read-only, with expiry), verify (`new_session` + `set_token` + `whoami` using the cached client's address and TLS settings), revoke, each wrapped in `record_kanidm_sdk_call`; generic `ensure_not_foreign<K>`, SSA `.force()`, create-only Secret.
  - Tests: in-use calculation from pod fixtures (volume match, annotation match, Terminating pods count, Succeeded/Failed pods ignored); foreign-owner rejection; keep existing ownership tests.
  - Acceptance: `make lint`, `cargo test -p kaniop-radius`.
  - Commit: `feat(radius): add Kanidm identity, token and apply executors`

- [ ] **WP-5 Controller, reconcile, status, cleanup; delete old code** (**Opus**)
  - Goal: wire everything, delete the journal-based code.
  - Files: `controller.rs`, `reconcile/{mod,status,cleanup}.rs`, `crd.rs` (status struct); delete `bootstrap`, `rotation`, `generation`, old `secret`/`apply`/`cleanup`/`identity_reconcile`/`controller`.
  - Failing-first tests: status builder always emits all five conditions; `lastTransitionTime` preserved when a condition does not change; `ready` follows the condition matrix; endpoint correct for ClusterIP, NodePort and LoadBalancer (pending and assigned); cleanup order as a pure step function including the D7 skip cases; transient error keeps its source and writes the condition first.
  - Acceptance: `make crdgen`, `make examples`, `make test`.
  - Commit: `feat(radius): reconcile RADIUS server with world-derived lifecycle`

- [ ] **WP-6 RBAC and CI cleanup** (Sonnet)
  - Goal: operator can read what the in-use scan needs; remove obsolete smoke.
  - Files: `charts/kaniop/templates/clusterrole.yaml` (add `apps` `replicasets` get/list/watch; pods already allow list/watch, verify), `charts/kaniop/tests/clusterrole_test.yaml`; delete `.github/workflows/radius-bootstrap-smoke.yml` and `tests/e2e/scripts/radius-bootstrap-smoke.sh`.
  - Tests: helm unittest asserting the new rule.
  - Acceptance: `helm unittest charts/kaniop`, `make agent-framework-check`.
  - Commits: `fix(chart): allow operator to read ReplicaSets for RADIUS drain` and `ci(radius): drop bootstrap smoke superseded by e2e`

- [ ] **WP-7 E2E shard** (Sonnet; escalate to Opus if timing is flaky)
  - Goal: live authentication and lifecycle proof.
  - **Starts with a spike** to settle D6: check which port the derived URI uses (`https://{domain}` has no port) versus the Kanidm Service port (8443), the e2e certificate SAN (`tests/e2e/test/kanidm/mod.rs`, only `idm.example.com`) and the harness domain (`{name}.localhost`, `tests/e2e/test/mod.rs`). Pick the test-only approach (D6 candidates), implement it, and record the decision and reasoning under Deviations. No CRD change.
  - Files: `tests/e2e/test/radius.rs` (and `mod.rs`), Makefile `E2E_SHARDS += radius` with `E2E_SHARD_FILTER_radius := test::radius`, `.github/workflows/rust.yml` shard matrix, shard list in `.opencode/skills/kaniop-development/SKILL.md` (or the skill that lists shards).
  - Setup: Kanidm `radius-e2e` via `setup_kanidm_connection`; CoreDNS rewrite helper if chosen (once-lock; CoreDNS `reload` takes about 30 s); CA ConfigMap with the e2e CA (second PEM block in `CERT`); EAP TLS Secret reusing `CERT`/`KEY`; client pods running pinned multi-arch `freeradius/freeradius-server:3.2.x` with `sleep infinity`, NAS client created as `<podIP>/32`. Spike: can `idm_admin` call `idm_account_radius_credential_regenerate`? If not, add it to `idm_radius_service_admins` additively.
  - Test cases (radtest with `-t mschap`):
    1. Lifecycle: Ready, fields and conditions set; account exists with membership and a pre-seeded unrelated member is kept; allowed user gets Access-Accept with `Tunnel-Private-Group-Id = 20`; disallowed group Access-Reject; wrong NAS secret no reply; a second pod as unknown NAS no reply; bearer and NAS secret absent from status and operator logs.
    2. NAS secret change: new config Secret appears, pods roll, `tokenId` and Kanidm token list unchanged, new secret authenticates, old Secret garbage-collected.
    3. TLS renewal (`[CERT, b"\n"]`): digest annotation changes, pods roll, token unchanged.
    4. Annotation rotation: radtest in a 1 s loop throughout with zero failures; two tokens during rollout, then one; `tokenId` changes.
    5. Operator restart mid-rotation: delete the operator pod right after annotating; rotation converges to one token and auth works.
    6. Broken input: delete a NAS Secret key; `ConfigurationReady=False`, template and token untouched, auth still works; restore key.
    7. Status wiped (`{}`): no new token.
    8. Config Secret deleted: new token minted, old one revoked after pods drain.
    9. Two CRs, delete one: other CR's membership and the unrelated member survive.
    10. Deletion: pods drained; account, tokens and membership gone; generated objects gone; user Secrets and ConfigMap remain.
    11. Unauthorized namespace: no finalizer, `ResourceNotWatched` event.
    12. Foreign Deployment with the same name: reported as conflict, not adopted.
  - Acceptance: `make check-e2e-shards`. Do not run e2e here; WP-9 does.
  - Commit: `test(e2e): add RADIUS shard with live authentication and rotation`

- [ ] **WP-8 Documentation** (Sonnet)
  - Goal: user docs and break-glass runbook.
  - Files: `Documentation/src/usage/radius.md`, `Documentation/src/SUMMARY.md`.
  - Content: manifest; MikroTik and cert-manager setup; security model; UDP, SNAT and `externalTrafficPolicy`; side-by-side migration with no adoption; rotation and the annotation; break-glass (identify `kaniop_radius_<uid-hex>` by its display-name marker, remove the membership edge, delete the account, clear the finalizer); troubleshooting by condition reason; how the Kanidm URL is derived (`origin`, else `https://{domain}`) and what to do for split-horizon (use `Kanidm.spec.origin`, not a RADIUS field); `readOnlyRootFilesystem` rationale.
  - Acceptance: docs build as in repo conventions; `make examples` clean.
  - Commit: `docs(book): document KanidmRadiusServer usage and break-glass`

- [ ] **WP-9 Local behavioral gate, before any review** (Sonnet runs it; failures go back to the WP that owns the file)
  - Prerequisites: docker, kind, kubectl, helm, `kind-chart-testing` context.
  - Commands, in order:
    ```bash
    make delete-kind; make e2e
    make e2e-test-shard SHARD=radius
    make e2e-test-shard SHARD=kanidm-core
    make e2e-test-shard SHARD=resources
    make e2e-test-shard SHARD=misc      # metrics test lists controllers
    make check-e2e-shards
    helm unittest charts/kaniop
    make test
    make crdgen && make examples && git diff --exit-code -- examples/
    make agent-framework-check
    ```
  - Record actual results (pass/fail, shard timings, flakes) in the Progress log. This is the one place where AGENTS.md's "do not run e2e locally by default" is deliberately overridden by human decision D10.
  - Commit: `docs(radius): record local e2e gate results` (plan update only).

## Iteration 2: review, fix, re-verify

- [ ] 1. **Opus correctness review:** `code-review` skill at high effort against `REVIEW.md`. Focus: completeness of the in-use set, races around revocation, finalizer edge cases, secret leakage in logs/events/status.
- [ ] 2. **Maintainability review:** `simplify` skill.
- [ ] 3. **Fixes:** one WP per finding cluster, each with a regression test confirmed failing first (`fix(radius): ...`). Add an eval case under `evals/cases/` for any escaped defect.
- [ ] 4. **Re-run WP-9 in full**, then push and watch the CI matrix (now including the radius shard). Human code owner approves; no agent approves or merges.

## Traceability: review findings to resolving WP

Findings R1-R5 come from the 2026-10-30 focused review recorded in the previous plan. R6-R22 were raised in the later correctness review of the refactor design (the originating review text is not in the repository; each is recorded here with its evidence so it is not lost).

| ID | Finding | Resolved by |
|---|---|---|
| R1 | Critical: controller only did read-only preflight and reported `Ready=false`; helpers not orchestrated | WP-3, WP-4, WP-5 |
| R2 | Critical: no persistent crash-safe lifecycle, no real finalizer path | WP-3 (planner), WP-5 (finalizer), WP-7 cases 5, 8, 10 |
| R3 | Important: image UID/GID 497/496 assumed; no policy for other tags | WP-2 (named constants, image-compat assertions), WP-7 |
| R4 | Important: real authentication/rollout untested; `radiusd -C` is syntax only | WP-7 (all cases), WP-9 |
| R5 | Important: CI could not certify the branch | Fixed `a185daf1`, `d1f54ee7`, `63113521`, `89f75986`; final proof WP-9 and Iteration 2 step 4 |
| R6 | Pending-state wedges: status loss, lost config Secret, config change while pending, orphaned tokens | WP-3 planner cases; WP-7 cases 2, 7, 8 |
| R7 | Drain detection by pod label only can miss pods, orphan ReplicaSets | WP-4 (`observe.rs` in-use set), WP-5 finalizer step 2 |
| R8 | Transient API errors flattened by `Err(_)`; source lost | WP-5 (`RadiusBlock` + propagated `k8s_util::Error`) |
| R9 | Wrong error variant for unknown Service type (`radius_workload.rs:123`) | WP-2 (enum + failing-first test) |
| R10 | Duplicate client detection misses `10.0.0.1` vs `10.0.0.1/32` | WP-2 (CIDR normalization + failing-first test) |
| R11 | CR name can exceed what the generated Service name allows | WP-2 (CEL rule, `<= 52`) |
| R12 | Magic UID/GID numbers | WP-2 |
| R13 | Status shape: checkpoint fields, conditions not always emitted, no endpoint | WP-5 (status model), WP-2 (print columns) |
| R14 | Operator cannot read ReplicaSets needed for drain | WP-6 |
| R15 | Bootstrap smoke covers static config only and is obsolete | WP-6 (delete), WP-7 (replacement) |
| R16 | Apply path ownership and update semantics inconsistent (no SSA `.force()`, per-type checks) | WP-4 (`apply.rs`) |
| R17 | Missing `ResourceNotWatched` event; no watches on referenced Secrets/ConfigMaps | WP-5 wiring; WP-7 cases 6, 11 |
| R18 | `tokenRotation` accepted but not effective | WP-3, WP-4 (expiry), WP-7 case 4 |
| R19 | Deletion edge cases: unowned account, missing Kanidm CR | WP-5 (D7), WP-7 case 10 |
| R20 | `readOnlyRootFilesystem` unresolved | WP-2 spike and assertion (D8), WP-8 |
| R21 | Unbounded CRD lists exceeded CEL cost budget | Fixed `d1f54ee7`; WP-2 re-checks with the new CEL rule |
| R22 | CRD migrator and artifacthub omitted the new CRD | Fixed `63113521`, `89f75986` |
| R23 | No way for the RADIUS pod to reach Kanidm over verified TLS in e2e | WP-7 spike (D6), test-only; no CRD field (D5) |
| R24 | Secrets must not appear in status, events, logs, metrics | WP-5 (`RadiusBlock` text), WP-7 case 1, Iteration 2 step 1 |

## Risks and blast radius

- Rotation correctness hinges on the in-use set. Missing a consumer revokes a live token. Mitigation: pods scanned by annotation and volume, ReplicaSets and template included, Terminating pods counted, WP-3/WP-4 tests, Iteration 2 focus.
- `whoami` proves token validity, not RADIUS login. Documented limitation; `maxUnavailable: 0` bounds the exposure.
- Deleting the account at finalization removes all tokens; ownership check (UID-bound name plus marker) must precede it (D7).
- Namespace-wide pod listing needs the existing pods list/watch RBAC; ReplicaSets RBAC added in WP-6.
- WP-1 touches workspace wiring in several binaries; keep it a pure move.
- Local e2e (WP-9) is long and flaky-prone on Kind; do not weaken tests to pass.

## Alternatives rejected

- `Kanidm.spec.radius` (couples a network service to the Kanidm lifecycle, blocks multi-instance).
- `KanidmServiceAccount` plus hand-written Deployment, and reuse of its code (see D1).
- Keeping the status journal and hardening it further (earlier approach; each hardening added states and recovery code).
- A new `kanidmUri` spec field (D5).
- Blocking forever at deletion when the account is not ours or Kanidm is gone (D7).

## Rollback

During development, deleting the CRD or controller is not a valid first rollback once Kanidm identities exist: delete CRs while the controller is alive and verify accounts, tokens and membership are gone, then remove the CRD. For a failed rotation the previous Secret and token stay in place until unused, so no manual restore is normally required. Emergency finalizer removal: identify the account `kaniop_radius_<uid-hex>` by its display-name marker, remove the `idm_radius_servers` membership edge, delete the account, then clear the finalizer (runbook delivered in WP-8). Each WP is an independent commit and can be reverted separately; WP-1 must be reverted last.

## Verification

Per WP: acceptance commands listed above. Final: WP-9 commands and Iteration 2 step 4. Always include actual results in the Progress log. Never enable `integration-test` and `e2e-test` together. Never hand-edit `charts/kaniop/crds/crds.yaml` or `examples/`.

## Deviations

Record material departures from this plan here with rationale, impact and test changes.

- **D8 spike result (WP-2, docker 29.8.2, `kanidm/radius:1.11.2`, user 497:496, caps dropped, `--read-only`):** the image fails with `Read-only file system` because the bootstrap rewrites `/etc/raddb/clients.conf` and writes `/etc/raddb/certs/{ca,server}.pem` (verified with `docker diff`; nothing else is written). A tmpfs only over `/etc/raddb/certs` is not enough (`clients.conf` is a file inside `/etc/raddb`). It runs, and `radiusd -C` passes, when `/etc/raddb` is a writable copy of the image directory (a Docker named volume seeded from the image; tmpfs for `/run/radiusd`, `/var/log/radius` and `/tmp` is not needed). In Kubernetes that copy needs an init container, and `radiusd` then refuses to start with `Configuration directory /etc/raddb is globally writable` because an emptyDir mount point is 0777; fixing the mode needs a root init container. Decision: `readOnlyRootFilesystem` stays unset (D8 unchanged); the builder carries the rationale as a comment and `root_filesystem_stays_writable_because_upstream_bootstraps_into_etc_raddb` asserts it.
- **WP-2 name-length rule test:** the CRD test asserts the root `x-kubernetes-validations` rule (`size(self.metadata.name) <= 52`, generated with `#[kube(validation = ...)]`) and that `52 + 1 + 10 <= 63`. It does not evaluate the rule against a 53-character name: that needs kube's `cel` feature, which would add the `cel` crate to the workspace. The apiserver evaluates it; WP-7/WP-9 can create a 53-character CR if wanted.
- **WP-2 additions beyond the checklist:** `status.endpoint` (optional string) is added now so the `Endpoint` print column points at a schema field; WP-5 fills it. Generated objects gain `app.kubernetes.io/managed-by=kaniop-radius-server` (metadata only, not in selectors). `config`, `image`, `tls` and the former `preflight` (now `inputs`) moved under `reconcile/`; `reconcile/meta.rs` holds the labels/annotations constants. The old controller keeps compiling: `workload_for_generation` takes the inputs digest and the legacy `kaniop.rs/radius-revision` annotation is kept until WP-5. No `cmd/examples` change was needed (`examples/` is unchanged).
- **WP-3 planner semantics (tightened, all toward never revoking a used token):**
  - *Uniform grace.* Retired tokens (with an owned Secret) are also held until 10 minutes old, and Secrets of such young tokens are not garbage-collected. Rationale: a reconcile that crashed (or a previous leader's request still in flight) after creating Secret B but before the Deployment apply lands would otherwise have B revoked and its Secret deleted while the apply may still reference it. Cost: a young retired token lingers at most 10 minutes. With the grace uniform, retired versus orphan no longer changes the revoke rule.
  - *Unresolved consumers hold all revocation.* If any consumer reference cannot be resolved to a token id, the planner cannot prove which token that consumer holds, so it revokes nothing that pass and reports the consumers in `TokenPlan.unresolved_consumers` (for status). A foreign pod carrying a garbage token-id annotation can therefore delay revocation but never cause one. The observer (WP-4) must list every config Secret a consumer mounts that it identifies as ours, even when the Secret no longer exists.
  - *Token bound.* Blocking at "already 4" (rather than "more than 4") keeps the account at most 4 tokens; Keep is never blocked, and revocation of unused tokens continues while blocked so the block clears itself.
  - *Rotate annotation.* Only a non-empty CR value that differs from the Secret's recorded value requests rotation; removing the annotation after a rotation does not rotate again.
  - *Clock.* `issued_at`/`expiry` use the Kanidm clock and `now` the operator clock; skew is assumed well below the 10-minute grace and 1-day margin. Kanidm ahead makes tokens look younger (held longer). Date arithmetic is checked; overflow means "not yet" for due/grace and "already" for expiry.
  - *Dependency.* `time = "0.3"` added to root `[workspace.dependencies]` (sibling crates pin it locally; left unchanged) because `kanidm_proto::ApiToken` uses `time::OffsetDateTime`.
  - *In-use from pod fixtures* stays in WP-4 as planned; the planner consumes `Consumer { kind, name, token_id, config_secrets }` with raw annotation values so it can classify unparsable ones.
- **WP-4 executors (layout and semantics):**
  - *Files.* Beyond `reconcile/{identity,observe,apply}.rs`: the token executors live in `reconcile/kanidm_token.rs` (not `token.rs`) so the planner stays pure with no I/O imports; `reconcile/error.rs` holds the single typed `ExecError` (transient API failures keep their source as `ExecError::Api(k8s_util::Error)`; expected blocks are variants with `is_retryable()`; Display never carries token or Secret material); `reconcile/sdk.rs` is the one `record_kanidm_sdk_call` wrapper; `reconcile/{kanidm_mock,kube_mock}.rs` are test-only. `identity.rs` was `git mv`-ed from the crate root into `reconcile/` and extended. Operator metrics gained `KANIDM_RESOURCE_RADIUS_SERVER` and `KANIDM_OP_WHOAMI` (needed now; WP-5 only registers the controller).
  - *Ownership rule.* `is_controlled_by` accepts an object when its controller reference (at most one per object) has our UID and kind; extra non-controller references no longer make it foreign, and the owner name is not compared (UID is unique). The old rule required exactly one reference.
  - *SSA.* Always `.force()` with manager `kanidmradiusservers.kaniop.rs`. SSA cannot be conditional on non-existence, so an absent object is still applied by SSA (a POST would leave an `Update` manager entry that owns fields forever) and the response is re-checked for a foreign owner; the window is one API round trip. `observe` runs the foreign check on the Deployment before anything is minted.
  - *Mint.* The token id comes from listing before and after the mint and matching a unique `kaniop-radius-<uuid>` label (`generate_api_token` returns only the bearer); anything but exactly one new match is `MintedTokenNotFound` and the token is left for the planner's orphan grace. Read-only, `compact=false`.
  - *Verify.* `whoami` on `new_session()` plus `set_token`, retried 5 x 1 s by default (`VerifyRetry`): right after a mint a lagging Kanidm replica answers 401. A persistent 401 is `CandidateRejected` (retryable), other errors keep their source. `whoami` returns `Ok(None)` on 401 in kanidm_client 1.11.2; the returned entry is not compared to the account name.
  - *Revoke.* A 404 or `OperationError::NoMatchingEntries` on token destroy is `AlreadyAbsent` (success). Identity delete likewise.
  - *In-use scan.* Pods count when not Succeeded/Failed (Terminating counted) and they carry the token-id annotation, carry our UID label, or mount a Secret that is ours. "Ours" means an owned Secret, a name shaped `<cr>-<10 lowercase hex>` (so already-deleted Secrets are still reported in `Consumer.config_secrets`), or, for UID-labelled pods and templates, any projected Secret with a `radius.toml` key. Secrets mounted through `env`/`envFrom` are also read. ReplicaSets count while `spec.replicas > 0` or `status.replicas > 0` (omitted `spec.replicas` defaults to 1), found via the Deployment controller reference or the UID label (so ReplicaSets of an already deleted Deployment are still seen). Lists are paginated (500) and namespace-wide; ReplicaSets are listed before pods.
  - *Config Secret create-only.* GET, then POST; on 409 the existing Secret must be controlled by us and equal in type, immutability, data and the token-id/rotation-request annotations, otherwise `SecretConflict`; never patched.
  - *Tests.* Failing-first: `apply_params_force_and_manager` and `apply_creates_absent_object_with_forced_server_side_apply` failed with SSA `force` missing (the old apply path had no `force`) and passed after adding it. Kanidm executors are tested against the real `KanidmClient` and a small fake HTTP server; Kubernetes executors through a `tower_test` mock API.
- Pending decisions to record: WP-7 e2e connectivity approach (D6); `idm_admin` radius-credential permission spike result.
- Standing constraint carried from earlier work: automatic image inference is limited to `kanidm/radius:1.11.2`, the only release whose UID 497/GID 496 and bootstrap were verified; other tags require explicit `spec.image`.

## Progress log

- **2026-10-31 (WP-0, replan):** Rewrote this plan and `spec.md` around a world-derived token lifecycle in a new `libs/radius` crate; human decisions D1-D10 recorded (`kanidmUri` rejected). Collapsed earlier history: the branch previously accumulated a typed CRD, config renderer, TLS and image helpers, UID-bound identity helpers, workload builder, finalizer-backed controller, a generation-journal bootstrap/rotation/recovery stack, and a published-image bootstrap smoke that verified static startup only (UID/GID 497/496 and `radiusd -C`). CI fixes `a185daf1`, `d1f54ee7`, `63113521`, `89f75986` are committed. No code changed in WP-0; no tests run beyond `make agent-framework-check`. WP-1 is next.
- **2026-10-31 (WP-1, move):** Moved the 16 `radius_*.rs` files via `git mv` into `libs/radius/src/` (prefix dropped, e.g. `radius_crd.rs` -> `crd.rs`; flat interim layout, later WPs restructure) as new crate `kaniop-radius`; path/import fixes only, no visibility changes in `libs/operator`; `sha2`/`hex` not added (unused until later WPs). `make lint`, `cargo test -p kaniop-radius` (58 passed), `make test`, `make crdgen`, `make examples` pass with no diff under `examples/` or `charts/`.
- **2026-10-31 (WP-2, building blocks):** Added `reconcile/{mod,meta}.rs` and moved `config`, `image`, `tls` and `preflight` (as `inputs`) under `reconcile/`; new pure pieces: `workload::{config_secret_name, build_config_secret}` (name `<cr>-<sha256(radius.toml)[..10]>`, immutable, credentials only in `data`, token-id and rotation-request annotations), `inputs::inputs_digest` (tagged, length-prefixed hash of EAP cert, EAP CA and Kanidm CA bytes) stored on `PreparedRadiusInputs`, `config::normalize_nas_address` (`10.0.0.1` equals `10.0.0.1/32`; host bits masked), `image::{RADIUS_UID, RADIUS_GID}`, `RadiusServiceExposure::from_spec` returning the new `InvalidServiceType` error (old code returned `InvalidExternalTrafficPolicy`), template annotations `radius-token-id` and `radius-inputs-digest`, `revisionHistoryLimit: 1`, CRD root rule `size(self.metadata.name) <= 52`, `Endpoint` print column. Tests written first and confirmed failing for the expected reason: unknown Service type (`InvalidExternalTrafficPolicy` instead of `InvalidServiceType`), `10.0.0.1` vs `10.0.0.1/32` (`Ok(())` instead of `DuplicateClient`), `Endpoint` column and CEL rule. Spike result recorded under Deviations (read-only root filesystem not viable). `sha2` and `hex` added to `kaniop-radius` from the workspace.
- **2026-11-01 (WP-3, token planner):** Added pure `reconcile/token.rs`: `plan_tokens(&TokenWorld, &TokenPolicy, OffsetDateTime) -> TokenPlan` plus `TokenPolicy::{from_spec, new_token_expiry}`. Tests written first and confirmed failing with `not implemented: WP-3` (policy tests aside), then implemented: 44-case table, a cross-case invariant check (current/in-use/young never revoked, no revoke while unresolved, no mint at the bound, sorted output), input-order independence, unresolved reporting, duplicate token entries, extreme dates, policy clamping, expiry `2 x period`, due-before-margin for every period 1..365, stable reasons. Mutation spot-checks (drop unresolved hold, drop grace, `>` bound, lenient partial resolution, gc of young Secrets, wrong reason, drop in-use check) each fail at least one test. Semantics tightened vs the table; recorded under Deviations and in `spec.md`. `make lint`, `cargo test -p kaniop-radius` (88 passed, 11 of them planner tests), `make test` pass. Not wired yet (WP-4/WP-5).
- **2026-11-01 (WP-4, executors):** Added `reconcile/{identity,observe,apply,kanidm_token,error}.rs`: `ensure_identity`/`delete_identity` (UID marker, additive `idm_radius_servers` edge, foreign account is `AccountNotOwned`), `observe` plus pure `build_world`/`pod_consumer`/`replicaset_consumer`/`template_ref`, `mint_token`/`verify_token`/`revoke_token`/`list_tokens`, and `ensure_not_foreign`/`apply_owned`/`ensure_config_secret`/`delete_owned`. Failing-first SSA `force` tests confirmed failing then fixed; 61 new tests (apply with mock API incl. 409 race, foreign rejection, delete preconditions; consumer extraction from pod fixtures; observe to plan rollout, Terminating pod, deleted-Secret pod; Kanidm executors against a fake HTTP server). Not wired yet (WP-5). `make lint`, `cargo test -p kaniop-radius` (149 passed) pass.
