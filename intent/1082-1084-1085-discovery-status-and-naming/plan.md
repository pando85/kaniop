# Implementation plan: Discovery status coherence, forced status applies, and the namespace-identity contract

Derived from: `spec.md` (draft). `intent.md` is the outcome record.

**Implementation has landed; workspace gates are green on the stable tree; the independent verifier
has completed with no code defect; CI is not.** The three
agents that wrote `libs/**`, `cmd/**`, `tests/**`, `docs/plans/**`, `examples/**` and the docs/e2e
correction have finished. Labels used below: **[in tree]** was read directly by this chain's author;
**[run]** is a gate executed by the verification agent and recorded in `/tmp/opencode/final-*.log`;
**[verifier]** is the independent verifier's read-only pass, now complete;
**[pending]** is not yet run — CI (no PR yet) and the e2e runtime on both arches (never run locally by
policy). Symbols are cited instead of line numbers.

## Repository findings

- `libs/backup/src/controller/discovery.rs` owns the discovery tick loop (`run`,
  `run_discovery_scan`, `process_discovery_for_schedule`), the discover Job builder
  (`build_discover_job`), the condition helpers (`merge_conditions`, `transition_time`), and the
  status write. At the base commit it had **seven** `update_discovery_status` call sites — failure
  arm, success arm, two truncation arms, invalid-result arm, fresh-tick arm, Job-created arm — and
  one `api.patch_status`. In the working tree the seven have collapsed to **five**
  `apply_discovery_status` call sites (the two truncation arms merged with the success signal into
  one `[Discovered, DiscoveryTruncated]` slice) and still exactly one `api.patch_status`.
  The apply's `.force()` is **pre-existing** (`PatchParams::apply(DISCOVERY_CONTROLLER_ID).force()`
  is already in `git show dde13107:…/discovery.rs`); this change reduces the number of applies, it
  does not add the force. There is no `force=true`/`fieldManager` string anywhere in
  `discovery.rs`, so the discovery apply has no query-level assertion — its status PATCH is checked
  at path level inside the seam test only. **[in tree]**
- `DiscoveryStatus` (`libs/backup-core/src/crd.rs`) has `skip_serializing_if` on every `Option`
  field and `conditions` annotated `x-kubernetes-list-type: map` / `x-kubernetes-list-map-keys:
  [type]`, which the CRD generator renders into `charts/kaniop/crds/crds.yaml`. That annotation is
  the reason an omitted condition is released **and deleted**, and it is why the fix has to declare
  the full merged list. **[in tree]**
- `merge_conditions` re-appends a replaced condition at the **end** of the list. Combined with the
  base gate's first-match lookup, that is the second, independent half of the cadence bug. **[in tree]**
- `transition_time(existing, type_, new_status, new_reason, now)` preserves the stored
  `lastTransitionTime` when `(type, status, reason)` all match — the property the fresh-tick
  re-declaration relies on, and the property that makes #1087's polarity non-trivial. **[in tree]**
- `intent/1081-discovery-retention-race/spec.md` requirement 9 and its conditions table, and that
  chain's `plan.md`, document the #1082 clobber as a caveat on the `, pruned N dangling` message.
  That chain is a **merged historical record** and is not edited by this work; this chain supersedes
  its "the forced apply is untestable in `libs/backup`" claim (requirement 11 of `spec.md`).
- The blessed wire-level test precedent exists: `libs/operator/src/kanidm/reconcile/mod.rs`
  (`get_test_context`, `ApiServerVerifier` wrapping `tower_test::mock::pair`, and assertions on
  `…?&force=true&fieldManager=kanidms.kaniop.rs`). It observes the request line, so it is
  non-vacuous. **[in tree]**
- `libs/oauth2/src/reconcile/status.rs` `update_status` early-returns with
  "status unchanged, skipping patch"; `update_image` in `libs/oauth2/src/reconcile/mod.rs` used the
  same manager string unforced. **[in tree]**
- `libs/backup/src/controller/repository.rs` has no finalizer, so its unforced apply is a
  backoff-loop/frozen-`Ready` defect, not a deletion deadlock. `reconcile_repository` reads
  `status.observed_generation` and never writes it, and
  `no_patch_when_condition_and_generation_unchanged` hand-builds `observed_generation: Some(5)` —
  both re-confirmed in the working tree, and both are #1086, out of scope here. **[in tree]**
- `run_discovery_scan` fetches the `Namespace` object (`get_namespace_object`) and uses it **only** as
  an existence/presence guard (`"namespace uid not available; skipping discovery"`); the UID never
  reaches the operation document, which carries `kanidm.metadata.namespace`. The function name is the
  phase-2 rename. **[in tree]**
- `compute_safety_manifest_key` (`libs/operator/src/kanidm/restore/legacy.rs`) hand-rolls the
  keyspace with `format!` instead of `RepositoryPath`, so it is the site most exposed to drift;
  `derive_backup_id` (`cmd/data-mover/src/commands/transport.rs`) hashes
  `"{namespace}:{kanidm_uid}:{stem}"` with UUIDv5. Both inputs are byte-frozen and now pinned by
  literal assertions. **[in tree]**
- `tower-test` was already resolved in `Cargo.lock` as an inline dev-dependency of `libs/operator`,
  so promoting it to `[workspace.dependencies]` adds no crate to the graph. The working tree's
  `Cargo.lock` diff is 2 lines. `make lint` runs clippy with `--locked`, so the lock must be
  committed. **[in tree]** / committed **[pending]**
- Chart RBAC already grants `kaniop.rs` `*` resources with `*` verbs and `""/namespaces` get/list/
  watch (`charts/kaniop/templates/clusterrole.yaml`), so no chart change is needed for the discovery
  namespace fetch or the forced applies. **[in tree]**
- `charts/kaniop/files/prometheusrules.yaml` defines `KaniopBackupDiscoveryStale` on
  `kaniop_backup_discovery_last_scan_success_timestamp`, which `run` sets on every successful tick —
  the reason the bug was invisible to alerting. **[in tree]**
- E2e cadence is configured in the `Makefile` `HELM_PARAMS` block
  (`BACKUP_DISCOVERY_SCAN_INTERVAL_SECS=30`, `BACKUP_DISCOVERY_STALE_SECS=60`), and `poll_until`
  defaults to `DEFAULT_E2E_WAIT_TIMEOUT_SECONDS = 180`. With the fixed gate the nominal e2e
  Job-creation period is `30*(floor(60/30)+2) = 120s`; the restored prune-test assertions run inside
  a 300s poll. **[in tree]** for the values and the budgets.
- `examples/backup.yaml` is generated from `cmd/examples/src/backup.rs`; the working tree shows both
  changed together, so `make examples` was run rather than the YAML hand-edited. **[in tree]**

## Files and components

Ownership below is the **agent split**, not a wish. Phase 1 sets are disjoint by construction;
`libs/backup/src/controller/discovery.rs` is deliberately touched by two phases and therefore
sequenced.

### Agent A — #1082 discovery status, gate and cadence (`libs/backup/src/controller/discovery.rs` only)

- `libs/backup/src/controller/discovery.rs` — `DiscoveryStatusUpdate`, `build_discovery_status`,
  `discovery_status_patch`, `apply_discovery_status`, `last_discovery_signal`,
  `evaluate_discovery_staleness`, the five converted call sites, the `transition_time` `now`
  parameter, the cadence test `healthy_schedule_recreates_a_job_on_the_stale_window_not_every_tick`,
  the discovery tests named in `spec.md` acceptance criteria, and (phase 2) `get_namespace_object`,
  the `namespace_name` renames and the seam test
  `run_discovery_scan_passes_namespace_name_not_uid_to_job`. **[in tree]**

### Agent B — #1085 forced applies and wire tests

- `libs/backup/src/controller/testutil.rs` — shared `#[cfg(test)] status_context::<K>()`. **[in tree, new file]**
- `libs/backup/src/controller/mod.rs` — registers `#[cfg(test)] mod testutil;`. **[in tree]**
- `libs/backup/src/controller/repository.rs` — `.force()` in `patch_repository_status` with a
  failure-mode comment; `patch_repository_status_applies_forced` plus its `accepted_repository_status`
  fixture. **[in tree]**
- `libs/backup/src/controller/backup.rs` — `patch_backup_status_applies_forced` (retroactive twin for
  PR #1083's force); the production `.force()` itself is unchanged from the base. **[in tree]**
- `libs/oauth2/src/reconcile/mod.rs` — `.force()` in `update_image` with the livelock comment; no
  test (accepted gap). **[in tree]**
- `Cargo.toml` (root `[workspace.dependencies]`), `libs/backup/Cargo.toml` (`http`, `tower-test`
  dev-deps), `libs/operator/Cargo.toml` (`tower-test = { workspace = true }`), `Cargo.lock`.
  **[in tree]** / committed **[pending]**

### Agent C — #1084 phase 1 internal renames, docs-of-record and contracts

- `libs/backup-core/src/paths.rs` — parameter renames, doc comments,
  `tenant_keyspace_uses_namespace_name_layout`. **[in tree]**
- `libs/backup-core/src/manifest.rs`, `libs/backup-core/src/operation.rs` — doc comments on the four
  `pub` wire fields that keep their names, plus the frozen-wire serde tests. **[in tree]**
- `cmd/data-mover/src/commands/transport.rs` — `derive_backup_id` parameter rename, derivation doc,
  `derive_backup_id_matches_frozen_input_bytes`, `…_frozen_stem_extraction`. **[in tree]**
- `cmd/data-mover/src/commands/upload_shared.rs` — `ManifestParams::namespace_uid` doc comment
  (name kept deliberately). **[in tree]**
- `libs/operator/src/kanidm/reconcile/transport.rs` — local rename and the pinning comment. **[in tree]**
- `libs/operator/src/kanidm/restore/legacy.rs` — dead `let _namespace_uid` binding removed from
  `ensure_safety_backup_job`, `compute_safety_manifest_key` doc, and
  `safety_manifest_key_matches_deployed_keyspace_layout`. **[in tree]**
- `cmd/examples/src/backup.rs` + generated `examples/backup.yaml` — name-shaped `manifestKey`. **[in tree]**
- `docs/plans/backup-transport.md`, `docs/plans/production-kanidm-backup-and-restore.md` — keyspace
  prose corrected. **[in tree]**
- `tests/tests/restore_hardening.rs` — `upload_backup_to_s3_remote` builds `tenants/<name>`. **[in tree]**

### Default decision — `BACKUP_DISCOVERY_STALE_SECS` 900 -> 300 (authorized)

Owned by the implementing agent, not by this chain. Three declaration sites, all read as `300`:

- `libs/operator/src/controller/mod.rs` — `DEFAULT_BACKUP_DISCOVERY_STALE_SECS = 300`
  (`DEFAULT_BACKUP_DISCOVERY_SCAN_INTERVAL_SECS` stays `300`). **[in tree]**
- `cmd/operator/src/main.rs` — `#[arg(long, default_value_t = 300, env)] backup_discovery_stale_secs`.
  **[in tree]**
- `Documentation/src/usage/backup-restore.md` — the `BACKUP_DISCOVERY_STALE_SECS` row reads `300`.
  **[in tree]**
- Test-side mirror: `STALE_THRESHOLD = 300` in the `discovery.rs` test module, asserted by
  `stale_threshold_constant_matches_getter_fallback_default`. **[in tree]**

### Phase 2 — landed in the tree (single owner at a time)

- `libs/backup/src/controller/discovery.rs`: `get_namespace_uid` -> `get_namespace_object`; the
  name-derived bindings/parameters in `run_discovery_scan`, `process_discovery_for_schedule`,
  `build_discover_job`, `reconcile_discovered_backups` are `namespace_name`; the pass-through
  operation-document test uses a name-shaped value; and the seam contract test
  `run_discovery_scan_passes_namespace_name_not_uid_to_job` drives the full `run_discovery_scan`
  against a mocked API server and asserts the POST Job body's operation JSON carries the namespace
  **name**, not the `Namespace` `metadata.uid`. **[in tree]**
- `tests/e2e/test/kanidm/backup.rs`: `backup_provider_managed_sse_roundtrip` builds its key and
  `namespaceUid` from the namespace name; `backup_discovery_prunes_dangling_invalid_record` latches
  the exact `, pruned 1 dangling` `Discovered` message (sticky `Arc<AtomicBool>`, since a later scan
  legitimately replaces it) and requires the CR to reach 404 — the discriminating pair — with the
  `lastSuccessfulScanTime`-past-baseline clause as corroboration, inside a 300s poll. **[in tree]**
- `tests/e2e/test/kanidm/mod.rs` / `restore.rs`: no remaining fixture uses a Namespace UID for the
  `tenants/` segment (the shared helpers and `restore.rs` were fixed in #1083). **[in tree]**
- `Documentation/src/usage/backup-restore.md`: "Discovery cadence" states the formula
  `T*(floor(S/T)+2)`, the 15-minute nominal period under the new defaults, and the strict boundary;
  the #1082 caveat is dropped from the "Leftovers are pruned" bullet.
  `Documentation/src/troubleshooting.md`: the same caveat is dropped, and the cadence wording is
  aligned. **[in tree]**; CI rendering remains **[pending]**.
- `intent/1082-1084-1085-discovery-status-and-naming/`, `evals/cases/discovery-status-apply-clobber/`
  — this chain and its eval case. **[in tree]**

### Phase 3 — integration, gates, review, commit (orchestrator)

- Workspace gates (run green on the stable tree), an independent verifier pass (complete: HIGH
  confidence, no Critical findings, no code defect), code review against `REVIEW.md`, and the commits.
  No agent edits shared files in this phase. Commits **[pending]**; the CI `e2e` run **[pending]**.

### Not touched by this change

`charts/**` (no schema, RBAC, values or rule change), `cmd/crdgen`, `cmd/webhook`, CRD CEL rules,
`libs/person`, `libs/group`, `libs/service-account`, `libs/k8s-util`, `intent/1000-backup-restore-hardening/`,
`intent/1081-discovery-retention-race/`.

## Ordered implementation steps

1. **[done in tree]** Agent A: introduce `DiscoveryStatusUpdate` + pure `build_discovery_status` +
   `discovery_status_patch` + `apply_discovery_status` (single forced apply, one manager), convert
   all seven base call sites, and keep the success path's `[Discovered, DiscoveryTruncated]` order so
   stored condition order is unchanged.
2. **[done in tree]** Agent A: companion gate fix — `last_discovery_signal` (latest
   `lastTransitionTime` over `Discovered`/`DiscoveryFailed`) + `evaluate_discovery_staleness`,
   replacing the first-match lookup that made the cadence stay broken for any schedule that had ever
   failed.
3. **[done in tree]** Agent A: unit tests for the fold, the never-drop invariant, the patch shape,
   and the gate boundary.
4. **[done in tree]** Agent B: promote `tower-test` to `[workspace.dependencies]`, add the shared
   `testutil::status_context`, force `patch_repository_status` and `update_image` with
   failure-mode comments, and add the two wire tests (repository, plus the retroactive backup twin).
5. **[done in tree]** Agent C: #1084 phase 1 renames, doc comments on the four frozen wire fields,
   fixture/example/plan corrections, and the golden + derivation + safety-key + frozen-wire tests.
6. **[done in tree]** Phase 2, `discovery.rs`: `get_namespace_object` rename, `namespace_name`
   bindings, the pass-through test literal made name-shaped, and the `run_discovery_scan` seam test.
7. **[done in tree]** Phase 2, e2e + docs: the SSE fixture aligned, the restored prune-test assertion
   (latched `, pruned 1 dangling` message + CR 404 as the discriminating pair, the scalar
   `lastSuccessfulScanTime` advance as corroboration) with its 300s poll, the cadence section corrected
   to the formula and the 15-minute nominal, and the #1082 caveats dropped from both user docs.
8. **[done in tree]** Default decision: `BACKUP_DISCOVERY_STALE_SECS` 900 -> 300 at all three
   declaration sites plus the test mirror (see "Default decision" above).
9. **[run]** Phase 3 gates on the stable tree: `make test` exits 0 (clippy `--locked --all-targets
   --all-features -- -D warnings` clean, `cargo fmt --check` clean, 997 passed / 0 failed across 27
   suites), e2e-target `cargo check`/`clippy` clean, `make check-e2e-shards` = 223 tests / 8 shards
   with `kanidm-backup: 23` unchanged, `make crdgen` no drift, `make examples` only the intended
   `manifestKey` difference, `make llms-check` up to date, `make agent-framework-check` 6 skills /
   4 eval cases. Full table in "Verification".
10. **[verifier complete, review pending]** The independent verifier finished with HIGH confidence, no
     Critical findings and no code defect (see "Independent review"). `REVIEW.md` Pass 1-4 review by a
     human owner is still the gate before merge; this chain does not approve its own change. Then
     commit with
    Conventional Commits, a scope from `.commitlintrc.json`, and a DCO sign-off. Suggested
    split: `fix(backup): …` for #1082 (including the 300s default), `fix(backup): force repository
    status apply …` + `fix(oauth2): …` (or one commit using `backup` and `oauth2` scopes separately)
    for #1085, `refactor(backup): name the namespace keyspace segment what it is …` plus
    `test(e2e)`/`docs(book)` for #1084, `test(evals): …` for the case, and `build(deps)` if the
    `tower-test` promotion is committed apart. `Cargo.lock` must be in the same commit as the manifests.
11. **[pending]** PR body: state the cadence and default change explicitly (formula
    `T*(floor(S/T)+2)`; 900s / 15 minutes nominal at the new 300s threshold; completion and
    reconciliation add latency) so a user-visible timing change is not buried, and list #1086/#1087 as
    the deferred follow-ups.

## Dependencies and coordination

- Shared names fixed here, asserted by more than one agent: `DiscoveryStatusUpdate`,
  `build_discovery_status`, `apply_discovery_status`, `last_discovery_signal`,
  `evaluate_discovery_staleness`, `DISCOVERY_CONTROLLER_ID` (`"backup-discovery"`), `status_context`,
  `get_namespace_object`, `patch_repository_status_applies_forced` and
  `patch_backup_status_applies_forced` (the two tests that assert the URI suffix
  `?&force=true&fieldManager=…`), and the condition types `Discovered`, `DiscoveryTruncated`,
  `DiscoveryFailed`. The landed phase-2 renames must not be undone by a later refactor.
- File-ownership rule in force: only one agent edits `libs/backup/src/controller/discovery.rs` at a
  time. Agent A owned phase 1; the phase-2 rename and seam-test owner took it after A's work settled
  in the tree.
- `Cargo.toml` / `Cargo.lock` are owned by Agent B alone. Agents A and C must not edit manifests.
- `examples/backup.yaml` is generated: Agent C edits `cmd/examples/src/backup.rs` and runs
  `make examples`. Nobody hand-edits `examples/**` or `charts/kaniop/crds/crds.yaml`
  (`opencode.json` denies those edits and CI verifies examples).
- The docs/e2e owner needed the final cadence arithmetic from `spec.md` requirement 8 before writing
  the documentation and before setting e2e budgets; the landed 300s poll is sized for the ~120s
  nominal e2e period rather than the pre-fix one-Job-per-30s-tick behaviour.
- #1086 is designed **against** the forced apply landed here (its `generation_changed` gate is what
  decides whether that apply runs), and #1087 against the post-#1082 gate shape (`last_discovery_signal`
  plus `transition_time` preservation). Neither is folded in.
- Commits are the orchestrator's: this chain does not branch, commit or push.

## Risks and blast radius

- **Cadence and default change are user-visible.** The nominal Job-creation period is
  `T*(floor(S/T)+2)` where `T` is the scan interval and `S` is the stale threshold, assuming the Job
  completes before the next tick. With the **new** defaults (300s tick, 300s threshold — approved by
  the user as part of this fix) that is 900s / 15 minutes. Retaining the old 900s stale default under
  the fixed algorithm would give 1500s / 25 minutes; that is a counterfactual, not the previous
  behaviour, which created a Job every tick. Completion and reconcile delay add latency beyond the
  Job-creation period, so no hard wall-clock catalogue guarantee is made. Mitigation: documented in
  the same PR; S3 `LIST` load drops, which is the point.
- **List-map ownership release.** The single apply must declare the full merged condition list. A
  later refactor that "optimises" the payload to only the changed condition would delete
  `Discovered`/`DiscoveryFailed` from live objects — a silent, upgrade-order-dependent state loss.
  Mitigation: `discovery_status_never_drops_conditions_it_does_not_update` plus the invariant comment
  in `build_discovery_status`; the same trap is documented in `spec.md` requirement 3.
- **Second field manager is the wrong fix and must not reappear.** Under `.force()` two managers
  fighting over one `status.discovery` make persisted state depend on upgrade order.
- **`serde` rename footgun (#1084).** All four structs carrying `namespace_uid` are
  `#[serde(rename_all = "camelCase")]`, so a Rust rename silently renames the wire field;
  `ManifestSource` is persisted in every `manifest.json` with no `#[serde(default)]`, so a rename
  makes every historical backup permanently unrestorable, with no expiry date. Mitigation: the two
  frozen-wire serde tests, and the field names are explicitly *not* renamed.
- **UUIDv5 derivation inputs.** `derive_backup_id` (transport re-upload idempotency, manifest
  conditional PUT) and `compute_safety_backup_id` (re-derived every reconcile, so an upgrade
  mid-SafetyBackup loses the already-uploaded safety backup) hash values that include the namespace
  identity. Changing an input byte orphans or duplicates objects. Mitigation: pinned literal
  assertions for both, and `compute_safety_manifest_key` is byte-compared against the deployed
  layout even though it hand-rolls `format!`.
- **`Cargo.lock` churn under `--locked`.** `make lint` runs clippy with `--locked`, so a manifest
  change committed without its lock fails CI, and a lock committed without the manifest fails the
  same way. Mitigation: one owner (Agent B), and the lock is in the same commit as the manifests;
  `tower-test` adds no new graph node.
- **Test-coverage trap.** Asserting `.force == true` on params built by the same expression is
  tautological. Mitigation: the wire assertions on the request URI at the two sites this change
  touched, each shown to fail by actually removing the `.force()` (restored afterwards); the same rule
  is stated for the eval case's forbidden changes. The discovery apply has no query-level assertion,
  so its force rests on the pre-existing code plus the fold tests, not on a wire check.
- **The oauth2 site ships untested.** Accepted, recorded gap, not a silent omission: `libs/oauth2`
  has no async test infrastructure. The change is one keyword plus a comment, and it is the same
  manager string as the already-forced site, so it cannot conflict with itself.
- **E2e budget regressions.** The restored assertions are the latched `, pruned 1 dangling`
  `Discovered` message plus the CR reaching 404; the `lastSuccessfulScanTime`-past-baseline clause is
  corroborating only. The prune test uses an explicit 300s poll rather than the 180s `poll_until`
  default, which is the size that survives the ~120s nominal e2e period. CI's `kanidm-backup` shard is
  the durable signal and has not run.
- **`DISCOVERY_CONFIRMATION_GRACE` bounds deletion, not catalogue latency.** 1800s bounds the
  deletion hold against a stuck pre-deletion discover Job; it does not bound how quickly a new backup
  appears in the catalogue. The invariant test asserts it outlives `BACKUP_JOB_TTL_SECONDS` (600s).
  Both the grace and the cadence are operator-configurable, so a deployment with a long tick can have
  a discovery period exceeding the grace, which only costs deletion latency (bounded by design).
  Recorded, no test asserts a relationship that is not guaranteed.
- **Concurrent edits and build locks (resolved).** The agents edited `libs/backup/src/controller/*`
  concurrently, so intermediate states were not compile-verified; that is no longer true — the gates
  ran on the stable tree with identical pre/post hashes (see "Verification"), and the independent
  verifier reviewed that same tree without finding a code defect. What still depends on runtime
  evidence is CI, above all the `kanidm-backup` e2e shard.

## Alternatives rejected

Recorded in `spec.md` (requirements 4, 18) and repeated here for the reviewer's convenience:

- **#1082** narrow the second apply to only `DiscoveryTruncated` (deletes the other conditions);
  give the truncation signal its own field manager (fights the first under `.force()`, makes state
  upgrade-order dependent); refresh the schedule before each apply (keeps two applies, leaves the
  invariant implicit, not unit-testable at the base commit); leave the gate's first-match lookup
  alone (cadence stays broken for any schedule that ever failed).
- **#1085** do nothing because both sites are latent (the population at risk is an incident runbook,
  a migration or a future co-writer, and the class was already load-bearing for #1081's deletion
  paths); assert `.force` on locally built params instead of the wire (vacuous).
- **#1084** rename the wire fields; migrate the keyspace to real UIDs; a `NamespaceName` newtype;
  webhook validation rejecting UID-shaped values; a serde alias transition. All five rejected for the
  reasons in `spec.md` requirement 18, plus the version-skew finding (requirement 19) that narrows
  the wire question: images are version-locked by default and in-flight Jobs/sidecars are
  self-consistent, so only an explicit `dataMoverImage.tag` override is exposed.
- **Fold #1086 or #1087 into this change**: rejected — #1086 changes when the forced apply runs at all
  and needs its unreachable test rewritten; #1087 is a user-visible condition-polarity change that
  alters what the staleness gate reads, so it must be designed against the post-#1082 shape.

## Verification

Gate results below come from the verification agent's runs on the stable tree, logged in
`/tmp/opencode/final-*.log`; snapshot stability was checked by hashing the code/test/doc tree before
and after the run (identical, `e065cf7e…`) and the full tracked diff (identical, `10f63894…`) — only
`intent/` changed during the run, so the results apply to the tree described here. CI and the e2e
runtime are **not** covered by these numbers and are not claimed green. Two kinds of evidence are
kept distinct: **[run]** = executed by the verification agent with a retained log under
`/tmp/opencode/`; **[run by implementer]** = a mutation check the implementing agents executed in the
working tree (for the #1082 items, in a throwaway worktree of the base commit, since removed) and
reported, with the failure output in their reports rather than in a retained log file. The independent
verifier's own pass was read-only, so where it comments on non-vacuity that is reasoned, not
re-executed.

| Command | Result | Status |
|---|---|---|
| `make test` (exit 0; includes what `make lint` checks) | `cargo clippy --locked --all-targets --all-features -- -D warnings` with zero warnings, `cargo fmt --check` with no diff, then 997 passed / 0 failed / 0 ignored across 27 suites | **[run]** `final-make-test.log` |
| Suite deltas | after-values read from the log: `kaniop-backup` 239 (discovery module 80), `kaniop-backup-core` 146, `kaniop-data-mover` 122, `kaniop-operator` 241, `kaniop-oauth2` 10; the baseline the verification agent compared against was `kaniop-backup` 221 (discovery 64), `backup-core` 142, `data-mover` 120, `operator` 240, `oauth2` 10 | **[run]** read from `final-make-test.log`; baseline figures are the verification agent's |
| `cargo check` / `cargo clippy -p kaniop-e2e-tests --features e2e-test --all-targets --locked` | both pass, zero warnings | **[run]** `final-cargo-check-e2e.log`, `final-cargo-clippy-e2e.log` |
| `make check-e2e-shards` | `Shards OK: 223 tests across 8 shards`; `kanidm-backup: 23`, unchanged (no test added or removed) | **[run]** `final-check-e2e-shards.log` |
| `make agent-framework-check` | `Agent framework valid: 6 skills, 4 eval cases` (was 3, plus `discovery-status-apply-clobber`) | **[run]** `final-agent-framework-check.log`, also run by this chain's author |
| `make llms-check` | `Documentation/llms.txt is up to date` | **[run]** `final-llms-check.log` |
| `make crdgen` | no drift: `charts/kaniop/crds/crds.yaml` unmodified, sha256 `f6dd3752…` | **[run]** `final-make-crdgen.log` |
| `make examples` | regenerated; the only tracked difference is the intended `examples/backup.yaml` `manifestKey` | **[run]** `final-make-examples.log` |
| Earlier formatting failure at `discovery.rs` and `tests/e2e/test/kanidm/backup.rs` | resolved — `cargo fmt --check` is clean in the run above; not an open blocker | **[run]** |
| Seam-test non-vacuity | changing the PRODUCTION `run_discovery_scan` argument from the namespace name to `&ns_uid` failed the seam test with `namespaceUid must carry the namespace name 'production', not the UID 'a1b2c3d4-…'`; restored, it passes | **[run by implementer]** in the working tree; output in their report, no retained log |
| Fold non-vacuity | folding each update from the stale base failed `build_discovery_status_applies_updates_in_order_without_clobbering` (`lastSuccessfulScanTime` stayed old) and `discovery_status_patch_excludes_schedule_conditions` (`lastDiscoveredCount` 8 vs 11) | **[run by implementer]** in a since-removed throwaway worktree of the base commit |
| Gate-selection non-vacuity | restoring the `.find()` selection failed `staleness_gate_uses_the_most_recent_of_discovered_and_failed` (chose `DiscoveryFailed` over `Discovered`) | **[run by implementer]**, same worktree |
| Condition-list non-vacuity | declaring only the updated condition failed 5 tests, including `discovery_status_never_drops_conditions_it_does_not_update` (1 condition vs 3) | **[run by implementer]**, same worktree |
| Cadence-pin non-vacuity | reverting the constants to 900s failed the pinned `ticks_until_stale` / period assertions in `healthy_schedule_recreates_a_job_on_the_stale_window_not_every_tick` | **[run by implementer]** |
| Wire-test non-vacuity | removing `.force()` from `patch_repository_status` failed with URI `…/status?&fieldManager=backup-repository` vs expected `…?&force=true&fieldManager=backup-repository` (25 passed, 1 failed); same for `patch_backup_status` (1 failed); both green again on restore | **[run by implementer]**; the independent verifier's pass was read-only, so from its side this is reasoned, not re-executed |
| `grep -c "api.patch_status" libs/backup/src/controller/discovery.rs` | `1` | read: `1` (`apply_discovery_status`) |
| `grep -rn "PatchParams::apply(" libs/backup/src/controller libs/oauth2/src/reconcile` | every status apply carries `.force()` | read: yes for the four touched sites |
| Default declarations read as 300 | `libs/operator/src/controller/mod.rs`, `cmd/operator/src/main.rs`, `Documentation/src/usage/backup-restore.md` | read: all `300`; scan interval stays `300` |
| CI, `e2e` job, shard `kanidm-backup` | `backup_discovery_prunes_dangling_invalid_record` green with the restored `Discovered` message and `lastSuccessfulScanTime` assertions (300s poll, ~120s nominal cadence); `backup_provider_managed_sse_roundtrip` green after the fixture alignment | **[pending]** — no PR created yet; e2e is never run locally by policy |
| `git status --short` after the commits | only the files in "Files and components" are modified; `Cargo.lock` committed with its manifests | **[pending]** — the orchestrator has not committed |
| Independent verifier agent | verdict below ("Independent review") | **[verifier]** complete: HIGH confidence, no Critical findings, no code defect |

### Independent review (completed)

- **Verdict:** HIGH confidence, no Critical findings, no code defect. **[verifier]**
- **Scope at review time:** branch HEAD equal to `origin/master` (`dde13107`), so all of this work was
  uncommitted — 26 tracked files, +1698/-225 as recorded then (re-read at this pass: 26 files,
  +1701/-227, the difference being later `docs/plans/**` edits). `libs/backup/src/controller/discovery.rs`
  +1038/-150, of which the additions from the `#[cfg(test)] mod tests` boundary are 798; the verifier
  put the production delta at +243/-147 and the same measure at this pass gives +240/-144 — the gap is
  boundary attribution of the `#[cfg(test)]` constants near the top of the file. Either way most of the
  growth is test code, not production logic. **[verifier]** plus this chain's re-read
- **Commands it ran:** `cargo fmt --all --check` clean; `make check-e2e-shards` 223 tests / 8 shards
  with `kanidm-backup` still 23; `make agent-framework-check` 6 skills / 4 eval cases;
  `cargo test --locked -p kaniop-backup --lib` 239 passed / 0 failed; targeted `libs/backup-core`
  namespace tests 6 passed; `derive_backup_id` tests in `cmd/data-mover` 10 passed; the
  `safety_manifest_key` tests in `kaniop-operator` 1 passed; `git diff --check` clean (this chain
  re-ran `git diff --check` and the test-name counts behind those targeted filters and agrees).
  It deliberately did **not** re-run full `make lint` / `make test` / `crdgen` / `examples` (the gate
  runner had already produced `/tmp/opencode/final-*.log`) and did not run e2e. **[verifier]**
- **Read-only confirmations against production code:** `apply_discovery_status` is the only
  `api.patch_status` in `discovery.rs` (count 1) reached from 5 call sites; the success arm folds
  `[Discovered=True/DiscoveryComplete, DiscoveryTruncated]` into one forced apply;
  `build_discovery_status` always merges the full condition list and `discovery_status_patch` declares
  only `status.discovery`, never the schedule controller's `status.conditions`;
  `last_discovery_signal` uses `max_by_key` over `lastTransitionTime` with a strict `elapsed >`
  threshold; PR #1083's dangling-prune guards and UID-precondition delete survive as rename-only hunks;
  the `repository.rs` and oauth2 force sites confirmed; producers and the consumer both wire the
  namespace name; the Namespace-UID check is **live control flow** (an empty UID logs and skips that
  schedule), not dead code as an earlier reviewer had claimed; the 300s default is wired
  `cmd/operator/src/main.rs` -> `set_backup_discovery_stale_threshold`; the docs state the formula with
  an explicit nominal-not-guaranteed caveat. **[verifier]**
- **Important finding (one):** the `intent.md` overstatement that wire-level tests proved the force for
  the discovery apply too. Corrected: the discovery `.force()` is pre-existing at the base commit and
  is covered at path level only; the URI-level assertions live in
  `patch_repository_status_applies_forced` and `patch_backup_status_applies_forced`.
- **Nits:** the e2e prune test's scalar `lastSuccessfulScanTime` clause is corroborating rather than
  discriminating (the test was deliberately left as written; now documented as such);
  `docs/plans/backup-transport.md` still named `get_namespace_uid` and the 900s staleness at review
  time — that file belongs to the docs agent, whose correction this chain observed landing afterwards
  (it now names `get_namespace_object` and the 300s default with the formula), so it stays **delegated /
  their edits may still be in flight**, not claimed done by this change; the same stale symbol in
  `spec.md`'s Security section was fixed here.
- **Stated limits:** read-only, so the negative controls (mutation checks) were reasoned by the
  verifier rather than re-executed — the executed mutations are the implementers', recorded above; the
  `crdgen` no-drift result was cited second-hand from the gate log; and these artifacts were being
  edited while it reviewed. **[verifier]**

## Rollback

Revert the branch. Nothing here persists a new value that a rollback could strand:

- #1082 returns to two applies per tick (frozen `lastSuccessfulScanTime`, `False/Discovering`
  reverting, one Job per tick) and to the first-match gate. No migration, no stored-version change.
- #1085 returns to unforced applies — latent, so the visible effect is usually nothing.
- #1084's renames and tests vanish; the keyspace, the wire names and the UUIDv5 inputs never changed,
  so no object in storage and no CR field has to be touched. The `Cargo.lock`/manifest promotion of
  `tower-test` reverts cleanly (the crate stays in the graph via `libs/operator`).

## Deviations

Update this section when implementation departs materially from the approved plan.

- **Authorized default change (approved by the user, not the issue's original intent):**
  `BACKUP_DISCOVERY_STALE_SECS` lowered from 900s to 300s; `BACKUP_DISCOVERY_SCAN_INTERVAL_SECS`
  stays 300s. This is the material deviation from the plan as originally written (which said "no
  default changes"); it is authorized and recorded here, implemented at all three declaration sites
  plus the test mirror (see "Default decision" above). The nominal period with the new defaults is
  `300*(floor(300/300)+2) = 900s ≈ 15 minutes`; retaining the old 900s stale default under the fixed
  algorithm would give `300*(floor(900/300)+2) = 1500s ≈ 25 minutes` — a counterfactual, not the
  prior behaviour. The eval case's forbidden changes are refined to allow this approved change while
  forbidding a bug-hiding default change.
- **Corrected record from PR #1083 (intentional, planned):** `intent/1081-*/spec.md` and `plan.md`
  claimed the forced status apply could not be covered in-crate. It can, and now is. The 1081 chain is
  left unedited as a historical record; `spec.md` requirement 11 supersedes the claim.
- **Phase 2 as planned, recorded late:** steps 6 and 7 (the `discovery.rs` renames + seam test, and
  the e2e + docs corrections) are in the tree; the earlier snapshot in this file that described them
  as "not started" was a reading taken before they landed, not a scope change. The seam contract is
  enforced by `run_discovery_scan_passes_namespace_name_not_uid_to_job`;
  `build_discover_job_operation_is_discover` no longer passes a UID-shaped literal, though it remains
  a pass-through and is not the enforcement point.
- **Gate status:** the formatting failure seen mid-work at `libs/backup/src/controller/discovery.rs`
  and `tests/e2e/test/kanidm/backup.rs` is resolved; `make test` (clippy `--locked --all-targets
  --all-features -- -D warnings` + `cargo fmt --check` + 997 tests) exits 0 on the stable tree, and the
  independent verifier completed with HIGH confidence, no Critical findings and no code defect. Still
  open: CI (no PR yet) and the e2e runtime on both arches (not run locally by policy). Nothing here
  claims CI green.
- **Deferred to follow-ups with pointers:** #1086 (`observedGeneration` never written; the
  `no_patch_when_condition_and_generation_unchanged` test asserts an unreachable path), #1087
  (`DiscoveryFailed=False`, `lastError` never populated), the oauth2 wire-level test, and any #1084
  wire rename or keyspace migration.
- **Docs/e2e correction settled:** the `Documentation/src` cadence text and the
  `backup_discovery_prunes_dangling_invalid_record` assertions are in the stable tree the gates ran
  against (code/test/doc hash identical before and after the run). What remains unproven is the e2e
  **runtime** on both arches, which CI owns.
- **Pending notes** (to be filled in after the CI run and human review):

  - *(empty)*
