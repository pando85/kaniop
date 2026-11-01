# Specification: One discovery apply per tick, forced status applies, and the namespace-name contract

Derived from: `intent.md` (draft)

Work items: #1082, #1084, #1085. Deferred with pointers: #1086, #1087.

**Status legend used throughout this file.** All authoring agents have finished; the tree was
stable while the workspace gates ran.

- **[in tree]** — the symbol or behaviour exists in the working tree and was read directly.
- **[run]** — a gate executed by the verification agent on that stable tree, logged in
  `/tmp/opencode/final-*.log`.
- **[pending]** — not yet run: CI (no PR yet) and the e2e runtime on both arches. Committing is also
  pending (the orchestrator owns it).
- **[verifier]** — the independent verifier's completed read-only pass (HIGH confidence, no Critical
  findings, no code defect); its confirmations and stated limits are recorded in `plan.md`
  "Independent review".

Line numbers are deliberately avoided; symbols are cited. Where a count is given (for example the
number of call sites) it was taken from `git show dde13107:…` and is exact for the base, approximate
for the working tree.

## Required behavior

### A. #1082 — one discovery status apply per tick

1. **[in tree]** A discovery tick records everything it learned in exactly **one** server-side
   apply of `status.discovery`, under one field manager, forced (the `.force()` itself is pre-existing
   at the base commit in `update_discovery_status`; what changes here is the number of applies, not
   the force). `apply_discovery_status` issues
   `api.patch_status(&name, &PatchParams::apply(DISCOVERY_CONTROLLER_ID).force(),
   &Patch::Apply(discovery_status_patch(&discovery)))`; `DISCOVERY_CONTROLLER_ID` is
   `discovery::CONTROLLER_ID` (`"backup-discovery"`) re-exported from
   `libs/backup/src/controller/mod.rs`. The seven `update_discovery_status` call sites of the base
   commit are converted: the failure arm, the invalid-result arm, the fresh-tick arm, the
   Job-created arm, and the success arm, which now passes **both** signals as a slice —
   `[Discovered=True/DiscoveryComplete, DiscoveryTruncated=True|False]` in that order, so the stored
   condition order is identical to what the two sequential applies produced.

2. **[in tree]** The merge is a pure function so it can be unit-tested without a client:
   `build_discovery_status(base, updates, generation, now) -> DiscoveryStatus` folds a
   `&[DiscoveryStatusUpdate]` in order onto a clone of the listed `status.discovery`. `now` is
   injected, so one tick has a single timestamp for both its scalar fields and the conditions it
   transitions. Per-field semantics are unchanged from the base: every tick refreshes
   `last_scan_time`; only `Discovered=True` advances `last_successful_scan_time`; only
   `DiscoveryFailed=True` records `last_error`; `last_discovered_count` is written only when the
   caller supplies a count.

3. **Hard constraint (not optional).** The applied document MUST declare the **complete** merged
   condition list. `KanidmBackupSchedule.status.discovery.conditions` is
   `x-kubernetes-list-type: map` keyed on `type` (`libs/backup-core/src/crd.rs` `DiscoveryStatus`,
   rendered into `charts/kaniop/crds/crds.yaml`), so an apply that omits a condition this same
   manager owns **releases and deletes** it. `build_discovery_status` therefore always calls
   `merge_conditions` with the full existing list plus each new condition, and
   `discovery_status_never_drops_conditions_it_does_not_update` asserts it. **[in tree]**

4. **[in tree]** Rejected as not viable, recorded so nobody re-litigates them:
   - *Narrow the second apply to only `DiscoveryTruncated`*: it would delete `Discovered` and
     `DiscoveryFailed`, because a list-map apply releases the elements it does not declare.
   - *Give the truncation signal its own field manager*: under `.force()` the two managers fight for
     the shared fields of one object and the persisted state becomes upgrade-order dependent.
   - *Re-read (refresh) the schedule before each apply*: fewer changed lines, but it keeps two
     applies per tick, leaves the "one writer per field per tick" invariant **implicit** (the next
     call site can reintroduce the clobber), and cannot be unit-tested in this crate at the base
     commit — there was no client-level test infrastructure there (see requirement 9, which corrects
     that claim for the *force*, not for the fold).

5. **Companion fix, required, not optional.** `merge_conditions` re-appends a replaced condition at
   the **end** of the list, while the base gate used a first-match `.find()` over stored order. After
   any failure tick, the gate therefore read `DiscoveryFailed` forever, and because
   `transition_time` preserves the timestamp when `(type, status, reason)` are unchanged, repeated
   identical failures preserved it — the cadence stayed broken for **every schedule that has ever
   failed**, independent of the clobber. **[in tree]** The selection is now a pure helper,
   `last_discovery_signal(conditions)`, which returns the `Discovered`/`DiscoveryFailed` condition
   with the **latest** `lastTransitionTime` (`max_by_key`; ties are irrelevant because both
   candidates would report the same elapsed time), and `evaluate_discovery_staleness(last_signal,
   now, threshold)` turns it into `(is_stale, elapsed, last_scan_time_str)` with a strict `>`
   comparison.

6. **[in tree]** A fresh tick (gate not stale, no Job in flight) re-declares the selected signal
   **verbatim** rather than skipping the write. `transition_time` keeps the stored
   `lastTransitionTime` for a matching `(type, status, reason)`, so this neither moves the stale
   window nor leaves the manager's declared set incomplete for the tick. `lastScanTime` still
   advances on every tick, which is the pre-existing documented behaviour.

7. **[run]** The workspace gates finished green on the stable tree: `make test` exit 0 — clippy
   `--locked --all-targets --all-features -- -D warnings` with zero warnings, `cargo fmt --check` with
   no diff, and 997 passed / 0 failed / 0 ignored across 27 suites (`kaniop-backup` 239, of which the
   `discovery` module is 80). The formatting failure seen mid-work at `discovery.rs` and
   `tests/e2e/test/kanidm/backup.rs` is resolved, not an open blocker. The independent verifier has
   completed (HIGH confidence, no Critical findings, no code defect); CI and the e2e runtime are still
   **[pending]**. The author of this chain ran only
   `make agent-framework-check`; the figures above are the verification agent's log, and snapshot
   stability was checked by hashing the tree before and after the run (identical).

8. **Behaviour change that must be stated honestly.** After the fix, Job creation follows the stale
   window instead of every tick, so the nominal discovery Job-creation period is:

   ```
   period = T * (floor(S/T) + 2)
   ```

   where `T` is the scan interval and `S` is the stale threshold, assuming the Job completes before
   the next tick. With the **new** defaults (`DEFAULT_SCAN_INTERVAL` 300s,
   `DEFAULT_BACKUP_DISCOVERY_STALE_SECS` 300s — approved by the user as part of this fix, implemented
   in `libs/operator/src/controller/mod.rs` and `cmd/operator/src/main.rs`) that is
   `300 * (floor(300/300) + 2) = 300 * 3 =` **900s ≈ 15 minutes**. Retaining the old 900s stale
   default under the fixed algorithm would give `300 * (floor(900/300) + 2) = 300 * 5 =` 1500s ≈ 25
   minutes; that figure is a counterfactual, not the previous production behaviour, which created a
   Job every tick because the gate read a frozen condition. In e2e
   (`BACKUP_DISCOVERY_SCAN_INTERVAL_SECS=30`, `BACKUP_DISCOVERY_STALE_SECS=60`, set in the
   `HELM_PARAMS` block of the `Makefile`) the nominal period is
   `30 * (floor(60/30) + 2) = 30 * 4 =` **120s ≈ 2 minutes**. Completion and reconcile delay add
   further latency beyond the Job-creation period, so these are nominal cadences, not a hard
   wall-clock catalogue guarantee. The cadence test
   `healthy_schedule_recreates_a_job_on_the_stale_window_not_every_tick` pins the arithmetic
   (`ticks_until_stale == 2`, period `== 3 ticks == 900s`) against `DEFAULT_SCAN_INTERVAL` and
   `STALE_THRESHOLD`. **[in tree]**, and its suite is **[run]** green. Consequences, all
   **[in tree]**:
   - `Documentation/src/usage/backup-restore.md` ("Discovery cadence") no longer states
     `max(scan_interval, stale_threshold)`; it states the formula, the strict `elapsed > threshold`
     boundary, and the nominal 15 minutes under the new defaults, and notes that completion and
     reconciliation add latency.
   - The #1082 caveats are gone from both user docs (`grep -rn 1082 Documentation/src/` is empty).
     (The 1081 chain is a merged historical record and is **not** edited here.)
   - The e2e assertion PR #1083 deliberately omitted is restored in
     `backup_discovery_prunes_dangling_invalid_record`. The discriminating pair is the latched exact
     `, pruned 1 dangling` `Discovered` message (kept sticky through an `Arc<AtomicBool>` because a
     later scan legitimately replaces it) plus the dangling CR reaching 404; the
     `lastSuccessfulScanTime` advanced-past-baseline clause is corroborating only, because unrelated
     fresh ticks also refresh that scalar. Poll budget 300s against the ~120s nominal e2e cadence.
   - `DISCOVERY_CONFIRMATION_GRACE` (1800s, `libs/backup/src/controller/backup.rs`) bounds the
     deletion hold against a stuck pre-deletion discover Job; it does not bound catalogue latency.
     `discovery_confirmation_grace_outlives_job_ttl` asserts it outlives `BACKUP_JOB_TTL_SECONDS`
     (600s). **[in tree]** for the existing assertion.

### B. #1085 — forced status applies, proven on the wire

9. **[in tree]** `patch_repository_status` (`libs/backup/src/controller/repository.rs`, manager
   `CONTROLLER_ID = "backup-repository"`) and `update_image` (`libs/oauth2/src/reconcile/mod.rs`,
   manager `OAUTH2_OPERATOR_NAME` = `kanidmoauth2clients.kaniop.rs`, the same string as the forced
   site in `libs/oauth2/src/reconcile/status.rs`) apply status with `.force()`, each with a comment
   naming **that site's** real failure mode rather than a generic one:
   - repository: a rejected apply is a permanent error/backoff loop with `Ready` and
     `EncryptionKeyReady` frozen. That file has **no** finalizer, so the failure mode is not a
     deletion deadlock; it is that `Ready`/`EncryptionKeyReady` gate transport-sidecar injection
     (no sidecar → no backups at all), schedule acceptance, and discovery's repository listing.
   - oauth2: the same class at lower severity. `update_status` early-returns with
     "status unchanged, skipping patch", so no forced apply is guaranteed to follow, and a foreign
     owner of any status field livelocks image-status convergence.
   Both are latent: no in-tree component writes those statuses today. The population at risk is a
   `kubectl patch --subresource=status` runbook, a migration or GitOps job, or a future co-writer.

10. **[in tree]** The force is proven at the wire, not at the builder. `libs/backup` now has a
    shared `#[cfg(test)]` helper module (`libs/backup/src/controller/testutil.rs`, registered in
    `libs/backup/src/controller/mod.rs`) exposing `status_context::<K>(controller_id)` which builds a
    `kaniop_operator::controller::context::Context<K>` over
    `tower_test::mock::pair::<Request<Body>, Response<Body>>()` and returns the handle that yields
    every request. `patch_repository_status_applies_forced` asserts the exact request URI ends with
    `?&force=true&fieldManager=backup-repository` **and** the applied condition types
    (`["Ready", "EncryptionKeyReady"]`); `patch_backup_status_applies_forced` is the twin that
    retroactively covers PR #1083's `patch_backup_status` (`…kanidmbackups/<name>/status?&force=true
    &fieldManager=backup`, applied `phase` and condition types). Because kube-core appends
    `force=true` to the query string, removing `.force()` fails the assertion — non-vacuous, unlike
    asserting `.force == true` on params built by the same expression. **Executed, not merely
    reasoned:** the implementing agent removed `.force()` from `patch_repository_status` and the test
    failed with URI `…/status?&fieldManager=backup-repository` against the expected
    `…?&force=true&fieldManager=backup-repository` (25 passed, 1 failed); the same mutation in
    `patch_backup_status` produced 1 failure; both went back to passing when the code was restored.
    The discovery apply is deliberately excluded from this claim (see "Concerns").

11. **The record from PR #1083 is corrected.** That chain stated that its own `.force()` in
    `patch_backup_status` was untestable in `libs/backup` ("no client-level test infrastructure").
    That was wrong: the repository already has a blessed, narrow precedent in
    `libs/operator/src/kanidm/reconcile/mod.rs` (`get_test_context`, `ApiServerVerifier`, and
    assertions on `…?&force=true&fieldManager=kanidms.kaniop.rs`). `intent/1081-*/` is a merged
    historical record and is left untouched; this chain supersedes that claim here and in `plan.md`.
    **[in tree]** for the tests, **[run]** for their execution (`kaniop-backup` 239 green).

12. **Dependency placement and lock file.** `tower-test` was already resolved in `Cargo.lock` as an
    inline dev-dependency of `libs/operator`, so no new crate enters the graph; per `AGENTS.md` it
    moves to the root `[workspace.dependencies]` and both `libs/backup` and `libs/operator`
    reference it with `{ workspace = true }`. **[in tree]** — `libs/backup` also gains `http` and
    `tower-test` as `{ workspace = true }` dev-dependencies, and `Cargo.lock` is modified. `Cargo.lock`
    changes MUST be committed: `make lint` runs clippy with `--locked`. **[pending]** for the commit.

13. **Accepted gap, recorded rather than silently omitted.** The oauth2 change ships **without** a
    new test: `libs/oauth2` has no async test infrastructure, and building it is out of scope here.
    The force there rests on the same class argument as the repository site plus the comment naming
    its failure mode. **[in tree]** for the absence of a test; no assertion of coverage is made.

14. No CRD, chart, RBAC, example (other than requirement 18) or user-facing documentation change
    comes from #1085.

### C. #1084 — the namespace-identity contract, phase 1 and phase 2 in the same PR

15. **[in tree]** Internal Rust identifiers only are renamed so the code says what it means, with
    zero wire and zero keyspace effect:
    - `libs/backup-core/src/paths.rs`: `RepositoryPath::{tenant_path, cluster_path, backup_path,
      payload_key, manifest_key, staging_path, manifests_prefix}` take `namespace` instead of
      `namespace_uid`. The parameters are positional, so callers do not change at all. The
      `validate_component(namespace, "namespaceUid")` label is **deliberately kept**: it names the
      wire field, not the parameter.
    - `cmd/data-mover/src/commands/transport.rs`: `derive_backup_id`'s parameter becomes
      `namespace`; the UUIDv5 input string is unchanged byte-for-byte (see 17).
    - `libs/operator/src/kanidm/reconcile/transport.rs`: the local feeding `"namespaceUid"` is
      `namespace`; `libs/operator/src/kanidm/restore/legacy.rs`: the dead `let _namespace_uid = &ns;`
      binding in `ensure_safety_backup_job` is removed and the locals around the safety upload say
      "name".
    - `libs/backup-core/src/upload_shared.rs` counterpart: `ManifestParams::namespace_uid` keeps its
      name (it is not a wire field but is copied verbatim into `ManifestSource.namespace_uid`) and
      gains a doc comment. **[in tree]**
    - Doc comments are added to the four `pub` wire fields that keep their NAMES:
      `ManifestSource::namespace_uid` (`libs/backup-core/src/manifest.rs`) and the three
      `namespace_uid` fields in `libs/backup-core/src/operation.rs`.
    - **[in tree]** `libs/backup/src/controller/discovery.rs`: the helper is renamed
      `get_namespace_uid` -> `get_namespace_object` (it returns `Result<Option<Namespace>>`, not a
      UID). Its value is used only as an existence/UID-presence guard in `run_discovery_scan`
      (`"namespace uid not available; skipping discovery"`); it is never passed to the discover Job.
      The name-derived bindings and parameters in `run_discovery_scan`,
      `process_discovery_for_schedule`, `build_discover_job` and `reconcile_discovered_backups` are
      `namespace_name`, and the short-lived `ns_uid` local keeps the guard's UID.

16. **[in tree]** Fixtures, plans and the generated example stop modelling the name as a UID:
    `tests/tests/restore_hardening.rs` (`upload_backup_to_s3_remote`) builds the `tenants/<name>`
    segment from the namespace name; `docs/plans/backup-transport.md` and
    `docs/plans/production-kanidm-backup-and-restore.md` describe `tenants/<namespace-name>/` and no
    longer show a UUID there; `cmd/examples/src/backup.rs` uses a name-shaped `manifestKey` (with no
    explanatory comment) and `examples/backup.yaml` is regenerated from it (`make examples`, never
    hand-edited); `backup_provider_managed_sse_roundtrip` in `tests/e2e/test/kanidm/backup.rs` now
    builds its key and `namespaceUid` from the namespace **name** (`"default"`), so it agrees with
    what discovery scans.

17. **Prose replaced by executable contracts.** **[in tree]**, all of them:
    - golden test pinning `manifests_prefix` with a **name-shaped** value
      (`tenant_keyspace_uses_namespace_name_layout` in `paths.rs`);
    - golden test pinning the `derive_backup_id` derivation bytes
      (`derive_backup_id_matches_frozen_input_bytes`, plus
      `…_frozen_stem_extraction`), because re-uploads after a restart must converge on the same ID;
    - strengthened `compute_safety_manifest_key` coverage
      (`safety_manifest_key_matches_deployed_keyspace_layout`): that function hand-rolls the key with
      `format!` instead of going through `RepositoryPath`, so it is the site most exposed to drift;
      the safety backup ID it feeds is re-derived every reconcile, so a changed value mid-SafetyBackup
      loses the already-uploaded safety backup;
    - frozen-wire serde tests: `manifest_source_namespace_uid_wire_field_is_frozen` and
      `operation_documents_namespace_uid_wire_field_is_frozen` assert the JSON names verbatim, and
      `namespace_uid_field_round_trips_a_namespace_name` shows a NAME round-trips through them;
    - the seam contract test `run_discovery_scan_passes_namespace_name_not_uid_to_job` in
      `libs/backup/src/controller/discovery.rs` invokes the full `run_discovery_scan` against a
      mocked API server (`testutil::status_context`, `tower_test::mock`), fixtures the namespace
      **name** (`"production"`), the `Namespace` UID and the Kanidm UID as unmistakably different
      values, and asserts the `POST .../jobs` body's operation JSON has
      `namespaceUid == "production"` and `!= <Namespace UID>`. That seam is exactly what the reverted
      automation commit inverted: changing the production argument from `namespace_name` to the
      Namespace UID (`&ns_uid`) was executed by the implementing agent and failed the request-body
      assertion, and restoring the name made it pass again. `build_discover_job_operation_is_discover`
      no longer passes a UID-shaped literal — it now uses the name-shaped value `"production"` — but
      it remains a pass-through of whatever the caller supplies, so it cannot by itself prove which
      identity is used; the seam test is the enforcement point, and no test asserts a UID-shaped
      `namespaceUid`.

18. **Rejections, recorded so nobody re-litigates them.**
    - *Rename the wire fields*: all four structs are `#[serde(rename_all = "camelCase")]`, so
      renaming a Rust field silently renames the wire field; `ManifestSource` is persisted inside
      every `manifest.json` in object storage and has no `#[serde(default)]`, so every historical
      backup becomes permanently unrestorable, with no expiry date on that breakage.
    - *Migrate the keyspace to real UIDs*: `KanidmBackup.spec.manifestKey` and
      `KanidmRestore.status.safetyBackupManifestKey` are persisted strings and `spec` is
      CEL-immutable, so the webhook forbids rewriting them; the value is a UUIDv5 derivation input
      for both `derive_backup_id` (transport re-upload idempotency) and `compute_safety_backup_id`
      (re-derived every reconcile, so an upgrade mid-SafetyBackup loses the already-uploaded safety
      backup); and the collision a UID keyspace would prevent cannot occur here, because the next
      segment is `clusters/<kanidm-uid>`.
    - *A `NamespaceName` newtype*: roughly 150+ lines across four crates, a breaking change to
      published crates, and it does not prevent the bug — constructing one from a UID type-checks
      perfectly.
    - *Webhook validation rejecting UID-shaped values*: a UUID is a syntactically valid DNS-1123
      label, so there is no discriminator, and it would not have caught the incident, which changed
      the consumer rather than a CR.
    - *A serde alias transition for the operation documents*: defensible only if a wire cleanup is
      independently scheduled, and never for the manifest field, where the alias would have to live
      forever.

19. **Version skew, recorded because it makes the wire question narrower than it looks.** Operator
    and data-mover images are version-locked by default through the chart; in-flight Jobs and
    transport sidecars are self-consistent by construction (immutable pod templates, create-only
    ConfigMap, image and operation document co-specified in the same container). The only population
    exposed to a mixed-version wire contract is an explicit `dataMoverImage.tag` override.

### D. Explicitly out of scope, with pointers

- **#1086** — `KanidmBackupRepositoryStatus.observedGeneration` is read in `reconcile_repository`
  (`generation_changed`) but never assigned, so the skip-the-apply optimisation can never trigger and
  `patch_repository_status` fires on every reconcile; `no_patch_when_condition_and_generation_unchanged`
  asserts a path production cannot reach. Both facts were re-read in the working tree at the time of
  writing and still hold. Fix it against the forced apply landed here, not instead of it.
- **#1087** — `DiscoveryFailed` is written with `status: "False"` on the failure paths while
  `build_discovery_status` records `last_error` only for `DiscoveryFailed == "True"`, so `lastError`
  is permanently empty. Flipping the polarity is a user-visible condition change **and** changes what
  the staleness gate reads (`last_discovery_signal`), so it must be designed against the post-#1082
  shape, including `transition_time`'s timestamp preservation under repeated identical failures.
- **#1084's wire rename**, any keyspace migration, and new async test infrastructure for
  `libs/oauth2`.

## Acceptance criteria

### #1082

- `build_discovery_status` unit tests (present in the tree, execution **[run]** green):
  `build_discovery_status_applies_updates_in_order_without_clobbering`,
  `build_discovery_status_job_failure_tick_keeps_every_other_field`,
  `build_discovery_status_successful_scan_tick_advances_success_time_and_count`,
  `build_discovery_status_truncated_tick_sets_truncated_true_and_keeps_counters`,
  `build_discovery_status_complete_tick_sets_truncated_false_and_is_idempotent`,
  `build_discovery_status_invalid_result_tick_keeps_failure_polarity`,
  `build_discovery_status_fresh_tick_reapplies_the_signal_verbatim`,
  `build_discovery_status_job_created_tick_marks_discovered_false_discovering`,
  `discovery_status_never_drops_conditions_it_does_not_update`,
  `discovery_status_patch_excludes_schedule_conditions`.
  The two that carry the bug: a tick that folds `[Discovered, DiscoveryTruncated]` keeps
  `lastSuccessfulScanTime`, `lastDiscoveredCount` and the `Discovered` message (including a `pruned`
  suffix), and an update set that does not mention a condition leaves that condition declared.
- Staleness-gate tests: `staleness_gate_uses_the_most_recent_of_discovered_and_failed`,
  `staleness_gate_other_conditions_are_not_signals`,
  `staleness_gate_boundary_equal_threshold_is_not_stale` (strict `>`: `elapsed == threshold` is
  **fresh**, the next tick past it is stale),
  `staleness_gate_without_any_signal_is_stale` (bootstraps the first Job).
- **Bug-fix proof by executed mutation** (the implementing agents ran each of these in the working
  tree, observed the expected failure, then restored the code; the #1082 mutations were done in a
  throwaway worktree of the base commit, since removed):
  - folding each update from the stale base instead of the running result failed
    `build_discovery_status_applies_updates_in_order_without_clobbering`
    (`lastSuccessfulScanTime` stayed at the old value) and
    `discovery_status_patch_excludes_schedule_conditions` (`lastDiscoveredCount` 8 instead of 11);
  - restoring the `.find()` gate selection failed
    `staleness_gate_uses_the_most_recent_of_discovered_and_failed` (it selected `DiscoveryFailed`
    instead of `Discovered`);
  - declaring only the updated condition failed 5 tests including
    `discovery_status_never_drops_conditions_it_does_not_update` (1 condition instead of 3);
  - reverting the cadence constants to 900s failed the pinned
    `healthy_schedule_recreates_a_job_on_the_stale_window_not_every_tick` assertions;

  Caveat: these outputs live in the implementers' reports; no mutation logs are retained under
  `/tmp/opencode`, and the independent verifier's own pass was read-only, so from the verifier's side
  the non-vacuity is reasoned rather than re-executed.
- No call site in `libs/backup/src/controller/discovery.rs` may issue a second `patch_status` for
  `status.discovery` within one tick; `grep -c "api.patch_status" libs/backup/src/controller/discovery.rs`
  is expected to be `1` (currently `1`, in `apply_discovery_status`).
- E2e (CI `kanidm-backup` shard): `backup_discovery_prunes_dangling_invalid_record`'s **discriminating**
  assertion is the latched exact `, pruned 1 dangling` `Discovered` message (sticky through
  `Arc<AtomicBool>` because a later scan legitimately replaces it) together with the dangling CR
  reaching 404. The `lastSuccessfulScanTime` advanced-past-baseline clause is **corroborating**, not
  discriminating: unrelated fresh ticks also refresh it, so it cannot on its own prove the prune.
  Budgets are sized to the ~120s nominal e2e cadence inside the 300s poll; the test is deliberately
  left as written. **[in tree]**, CI execution **[pending]**.
- Documentation: the cadence section states the formula `T*(floor(S/T)+2)` with the new default
  arithmetic (900s / 15 minutes) and notes that completion and reconciliation add latency; the #1082
  caveats are removed from both user docs. **[in tree]**
- Default decision, all three declaration sites read as 300: `DEFAULT_BACKUP_DISCOVERY_STALE_SECS`
  (`libs/operator/src/controller/mod.rs`), the `--backup-discovery-stale-secs` clap default
  (`cmd/operator/src/main.rs`), and the `BACKUP_DISCOVERY_STALE_SECS` row in
  `Documentation/src/usage/backup-restore.md`; the test mirror `STALE_THRESHOLD` is 300 and the scan
  interval stays 300s at all sites. **[in tree]**

### #1085

- `patch_repository_status_applies_forced` asserts `?&force=true&fieldManager=backup-repository` and
  the applied condition types; `patch_backup_status_applies_forced` asserts
  `?&force=true&fieldManager=backup`, the applied `phase`, and the condition types. Both were shown to
  fail by actually removing `.force()` (executed by the implementing agent: 1 failure each, restored
  to green), not merely reasoned about. **[in tree]**, execution on the restored tree **[run]** green.
- `grep -rn "PatchParams::apply(" libs/backup/src/controller libs/oauth2/src/reconcile` shows every
  status apply forced; `libs/oauth2/src/reconcile/mod.rs` (`update_image`) carries a comment naming
  the livelock, and `repository.rs` one naming the frozen-`Ready` backoff loop. **[in tree]**
- `tower-test` appears exactly once, in root `[workspace.dependencies]`, referenced by
  `libs/backup` and `libs/operator` as `{ workspace = true }`. `Cargo.lock` is modified in the working
  tree (2-line diff) and travels with the manifests into the commit — the commit itself is **[pending]**
  (the orchestrator owns it, nothing is committed yet).
- Accepted gap, stated: no test for the oauth2 force.

### #1084

- Contract tests listed in requirement 17, plus: `RepositoryPath` has no parameter named
  `namespace_uid`; `derive_backup_id` has no parameter named `namespace_uid`; the discovery helper is
  `get_namespace_object`; `grep -rn "_namespace_uid" libs/operator/src` finds no dead binding.
  **[in tree]**
- `grep -rn "namespace_uid" tests/e2e/test/kanidm/` shows only name-valued bindings; no e2e fixture
  fetches a `Namespace` UID to build a `tenants/` segment. **[in tree]**
- Golden keyspace and derivation assertions are literal-valued (a pinned prefix string, pinned UUIDs),
  so renaming anything internal cannot silently change them.
- Seam-test non-vacuity was **executed**: changing the PRODUCTION `run_discovery_scan` argument from
  the namespace name to `&ns_uid` failed `run_discovery_scan_passes_namespace_name_not_uid_to_job`
  with `namespaceUid must carry the namespace name 'production', not the UID 'a1b2c3d4-…'`, and the
  code was then restored. Reported by the implementing agent; no mutation log is retained under
  `/tmp/opencode`.
- `make crdgen` produces no drift (no CRD field is touched); `make examples` output is committed and
  matches `cmd/examples/src/backup.rs`.

## Design

```
#1082, one tick, one apply

  listed schedule (immutable ref, start of tick)
        |  status.discovery (base, possibly stale)
        v
  updates: [Discovered, DiscoveryTruncated]      <- order == stored order
        |  fold in order, pure, injected `now`
        v
  build_discovery_status -> full DiscoveryStatus (complete condition list)
        |
        v
  apply_discovery_status: one Patch::Apply, manager `backup-discovery`, force()

  gate: last_discovery_signal(conditions) = max lastTransitionTime over {Discovered, DiscoveryFailed}
        -> evaluate_discovery_staleness(elapsed > threshold)  (strict >)
        -> fresh tick: re-declare that signal verbatim; its condition lastTransitionTime is
           preserved (type/status/reason unchanged), while the scalar lastScanTime still refreshes

#1084, the contract that must not drift

  producers (operator transport, legacy safety upload)      consumer (discovery)
        |  namespace NAME -> "namespaceUid" wire field            |
        v                                                         v
  <prefix>/v1/tenants/<namespace-name>/clusters/<kanidm-uid>/backups/<id>/manifest.json
        ^                                                         ^
        |__ UUIDv5 inputs: "<namespace>:<kanidm_uid>:<stem>" ______|  (frozen bytes)
```

## Interfaces and data

### Conditions and fields written by one discovery tick

| Field / condition | Written when | Notes |
|---|---|---|
| `status.discovery.lastScanTime` | every tick that applies | advances even on a fresh tick |
| `status.discovery.lastSuccessfulScanTime` | `Discovered=True` only | was reverted or deleted by the second apply |
| `status.discovery.lastDiscoveredCount` | the caller supplies a count (`Discovered=True` success arm) | omitted when `None`, so ownership was released |
| `status.discovery.conditions[]` | always the complete merged list | list-map keyed on `type`; omission deletes |
| `Discovered` | `True/DiscoveryComplete` (success), `False/Discovering` (Job created), re-declared verbatim (fresh tick) | carries `, pruned N dangling` from #1083 |
| `DiscoveryTruncated` | success arm only | `True/ResultLimitReached` or `False/ResultComplete` |
| `DiscoveryFailed` | failure arms | polarity unchanged: `False/DiscoverJobFailed`, `False/DiscoverResultInvalid` (#1087) |
| `status.discovery.lastError` | `DiscoveryFailed == "True"` only | therefore never populated today (#1087) |

### Status-apply sites touched by #1085

| Site | Manager | Before | After | Coverage |
|---|---|---|---|---|
| `patch_repository_status` (`repository.rs`) | `backup-repository` | unforced | forced | `patch_repository_status_applies_forced` |
| `update_image` (`libs/oauth2/src/reconcile/mod.rs`) | `kanidmoauth2clients.kaniop.rs` | unforced | forced | none — accepted gap, no async test infra in that crate |
| `patch_backup_status` (`backup.rs`) | `backup` | forced in #1083, untested | unchanged | `patch_backup_status_applies_forced` (retroactive) |
| `apply_discovery_status` (`discovery.rs`) | `backup-discovery` | forced (pre-existing) | forced, one apply per tick | fold/patch-shape unit tests; no query-level `force` assertion, path level only in the seam test |

### Keyspace and wire names (#1084, frozen)

| Item | Value / name | Change here |
|---|---|---|
| `tenants/<x>` segment | Kubernetes namespace **name** | none (bytes unchanged) |
| `OperationDocument` field | `namespaceUid` (camelCase from `namespace_uid`) | doc comment only |
| `manifest.json` `source.namespaceUid` | same, required, no `#[serde(default)]` | doc comment only |
| `derive_backup_id` UUIDv5 input | `"{namespace}:{kanidm_uid}:{stem}"` | parameter rename only |
| `compute_safety_backup_id` / `compute_safety_manifest_key` | namespace name in the derivation chain | docs + stricter tests |
| `KanidmBackup.spec.manifestKey` example | `v1/tenants/identity-prod/…` | regenerated via `make examples` |

### Metrics and alerts (unchanged by this work, stated for the record)

| Item | Definition | Relevance |
|---|---|---|
| `KaniopBackupDiscoveryStale` | `time() - kaniop_backup_discovery_last_scan_success_timestamp > 7200`, `for: 15m` (`charts/kaniop/files/prometheusrules.yaml`) | never fired for #1082: `set_last_scan_success` runs on every successful *tick* regardless of Jobs |
| `kaniop_backup_discovery_last_scan_timestamp` | set at the top of every tick in `run` | proves the loop ran, not that discovery worked |
| `kaniop_backup_discovery_dangling_pruned_total{repository}` | from #1083 | durable prune signal; the condition message becomes durable here too |

## Security and privacy

- No credential handling changes; the data mover remains the only component reading objects.
- Forcing a status apply only changes who wins a field conflict: the controller, which owns it. It
  does not widen RBAC — the chart's ClusterRole grants `kaniop.rs` `*` resources with `*` verbs,
  which already covers these `patch …/status` calls, and `""/namespaces` `get` (which is what
  `get_namespace_object` in `discovery.rs` calls). It also does not let the controller write fields
  another controller owns: in-tree audits show `backup-discovery` is the only writer of
  `KanidmBackupSchedule.status.discovery` (`schedule_status_patch_excludes_discovery` asserts the
  schedule controller never sends it) and `backup-repository` the only writer of its repository
  status.
- Condition messages and `lastError` carry manifest keys and provider error text; no secrets.
- The #1084 rename removes a class of silent-failure confusion (a caller that trusted `Uid` wrote to
  a prefix nobody scanned) without changing any authorization, label or ownership semantics.

## Kubernetes, upgrade, and compatibility considerations

- No CRD schema change: no `cmd/crdgen` edit, `make crdgen` must produce no drift, and no CEL rule
  changes. `make examples` does change one example value, which is generated, not hand-edited.
- Upgrade effect on persisted state: fields that the clobber deleted or reverted reappear. Objects
  that had `lastSuccessfulScanTime` removed are re-added by the next successful tick; nothing has to
  be migrated and no stored version changes.
- Cadence effect is a real behaviour change (requirement 8): fewer discover Jobs per unit time, and
  a longer worst case for cataloguing. Version-skew safe: only one manager writes `status.discovery`,
  so a rollback and an upgrade in the same cluster never mix two shapes of that object.
- SSA ownership: the single apply declares the full condition list, so no condition is released
  during the upgrade; a rollback simply returns to two applies (the bug), stranding nothing.
- Forced applies: `force` is a request parameter, not stored state, so it is neutral across upgrades
  and rollbacks.
- #1084 is byte-compatible by construction: keyspace, wire names, and UUIDv5 inputs are pinned by
  tests rather than by comments.

## Operational behavior and rollback

- Healthy discovery: `status.discovery` stops contradicting the metrics; a new backup is nominally
  catalogued within roughly 15 minutes at the new defaults (the counterfactual with the old 900s
  stale default would be roughly 25 minutes), and S3 `LIST` load per schedule drops. Job completion
  and reconciliation add latency, so no hard bound is asserted.
- `KaniopBackupDiscoveryStale` still does not cover a schedule whose status is frozen — it is
  tick-based — so a frozen `status.discovery` remains a "read the CR" symptom, not an alert. #1087
  is where a status-based signal would be designed.
- After a failing discovery tick the gate now follows the newest signal, so a schedule that recovers
  is treated as fresh again instead of creating a Job every tick. A schedule that keeps failing with
  the same reason stays stale and is retried every tick, which is the intended retry cadence for
  failures.
- A `KanidmBackupRepository` whose `.status` was patched by an outside tool now converges instead of
  looping in backoff with `Ready` frozen; the same for an OAuth2 client's image status.
- Rollback: revert the branch. #1082 returns to the clobber (frozen status, Job every tick); #1085
  returns to unforced applies (latent, no state to unwind); #1084's renames and tests vanish but the
  deployed keyspace never changed, so no object or CR has to be touched. Nothing here persists a new
  value that a rollback could strand.

## Concerns and unresolved questions

- **Cadence expectation mismatch is user-visible.** Anyone who sized alert windows, runbooks, or
  "how soon will my backup show up" against the old advertised 900s now sees the correct formula
  `T*(floor(S/T)+2)`. With the new defaults (300s tick, 300s threshold) the nominal period is 900s /
  15 minutes. This is a behaviour change to document, not a follow-up.
- **`DISCOVERY_CONFIRMATION_GRACE` bounds deletion, not catalogue latency.** 1800s bounds the
  deletion hold against a stuck pre-deletion discover Job; it does not bound how quickly a new backup
  appears in the catalogue. Both the grace and the cadence are operator-configurable
  (`BACKUP_DISCOVERY_STALE_SECS`, `BACKUP_DISCOVERY_SCAN_INTERVAL_SECS`), so a deployment with a long
  tick can have a discovery period exceeding the grace. The grace is a bound on *deletion* latency,
  not on discovery, so this is a documented-tuning concern rather than a defect; a reviewer should
  confirm no test hard-codes the relationship that does not exist.
- **Fresh-tick writes cost one apply per schedule per tick.** Deliberate: it keeps the declared set
  complete while preserving the selected condition's `lastTransitionTime` (type/status/reason
  unchanged), which is what the gate reads; only the scalar `lastScanTime` moves. If that apply load
  matters, the correct follow-up is an unchanged-state short-circuit inside `apply_discovery_status`,
  not a second manager.
- **`status.discovery.lastError` stays empty** by design here (#1087). The gate fix does not depend
  on it.
- **The discovery apply is not asserted at query level.** Its `.force()` is pre-existing at the base
  commit, not introduced here; `grep -n "force=true\|fieldManager" libs/backup/src/controller/discovery.rs`
  is empty, and the seam test asserts only that the status request PATH contains
  `…/kanidmbackupschedules/<name>/status`. The fold's behaviour is covered by `build_discovery_status`
  unit tests plus `discovery_status_patch_excludes_schedule_conditions` (document shape), not by a
  wire assertion on `force`. `apply_discovery_status` takes a `&Client` rather than a `Context<K>`, so
  a dedicated mock assertion there is a small signature change that was not made in this PR.
- **The #1084 slice's explanatory comment in `cmd/examples/src/backup.rs` was removed** by the
  owning agent (verified: no `namespaceUid`/`misnomer` comment remains in that file), so the
  repository-contract deviation noted in earlier revisions of this chain no longer exists.
- **Gate evidence is second-hand to this chain's author.** The workspace gates are green on the
  stable tree per the verification agent's logs (`/tmp/opencode/final-*.log`, itemized in `plan.md`
  "Verification"); this author ran only the Python `make agent-framework-check` directly. The
  independent verifier has completed with HIGH confidence, no Critical findings and no code defect
  (recorded in `plan.md` "Independent review"). **Still pending, and not claimed green here:** CI
  (no PR yet) and the e2e runtime on both arches (never run locally by policy).
