# Intent: Coherent discovery status, unblockable status applies, and an honest namespace-identity contract

- Owner: pando85
- Status: draft — implementation has landed on `fix/discovery-status-keyspace-naming`
  (base `origin/master` = `dde13107`); the workspace gates are green on the stable tree, while CI
  (no PR yet) and the e2e runtime are still pending; the independent verifier has completed with
  HIGH confidence and no code defect. This
  chain records the decisions that were made, including the authorized change of the
  `BACKUP_DISCOVERY_STALE_SECS` default from 900s to 300s; `plan.md` records what is done and what
  remains to verify.
- Work items:
  - https://github.com/pando85/kaniop/issues/1082 — discovery status clobbered by the second
    server-side apply of a tick
  - https://github.com/pando85/kaniop/issues/1085 — unforced `KanidmBackupRepository` and oauth2
    image-status applies
  - https://github.com/pando85/kaniop/issues/1084 — `namespaceUid` and the `tenants/<x>` keyspace
    segment carry the namespace **name**
  - deferred, deliberately not in this change:
    https://github.com/pando85/kaniop/issues/1086 (`observedGeneration` never written) and
    https://github.com/pando85/kaniop/issues/1087 (`DiscoveryFailed` polarity, `lastError` never
    populated)

## Problem

Three defects, one shared root: the operator writes cluster state through server-side apply using a
base object it can no longer trust, and the code calls a Kubernetes namespace *name* a *UID*.

**#1082 — the status a healthy operator reports is a lie, and the cadence it drives is wrong.**
One discovery scan tick issues two forced applies under the same field manager
(`backup-discovery`, `libs/backup/src/controller/discovery.rs`), and `update_discovery_status`
rebuilds the whole `status.discovery` object from the `KanidmBackupSchedule` that was `LIST`ed at
the start of the tick. That object is passed by immutable reference, so the second apply can never
see what the first one wrote: it re-declares every field from the stale base and wins.
`lastSuccessfulScanTime` reverts, or is deleted outright when the stale base had `None` (an
`Option` field is omitted by `skip_serializing_if`, the manager releases ownership, and
structured-merge-diff removes the value); `lastDiscoveredCount` reverts; and the `Discovered`
condition reverts to `False/Discovering` with its old transition time, reason and message —
including the `, pruned N dangling` suffix PR #1083 added. Second order: the staleness gate reads
the transition time of the *first* stored condition matching `Discovered` or `DiscoveryFailed`, so
the reverted `False/Discovering@T0` makes elapsed grow without bound and a discover Job is created
**every tick** instead of once per stale window (five times the `LIST` rate under the shipped 900s
stale default). Git-consistent with
the reported range: the paired second apply (`DiscoveryTruncated` on both the truncated and the
complete branch) arrived in `aa4f653a`, first shipped in v0.16.2, and is present through v0.19.0.
No alert ever fired, because `KaniopBackupDiscoveryStale` is metric-based
(`time() - kaniop_backup_discovery_last_scan_success_timestamp > 7200`) and that metric is set on
every successful *tick* regardless of whether a Job ran.

**#1085 — two status applies can be blocked by a foreign field owner.**
`patch_repository_status` (`libs/backup/src/controller/repository.rs`, manager
`backup-repository`) and `update_image` (`libs/oauth2/src/reconcile/mod.rs`, manager
`kanidmoauth2clients.kaniop.rs`) apply their whole status object *without* `.force()`. The
repository site's real failure mode is not a finalizer deadlock (that file has no finalizer): a
rejected `409 Conflict / FieldManagerConflict` becomes a permanent error/backoff loop with
`Ready`/`EncryptionKeyReady` frozen, which gates transport-sidecar injection — no sidecar means no
backups at all — plus schedule acceptance and discovery's repository listing. The oauth2 site is
the same class at lower severity: a foreign owner of any status field livelocks image-status
convergence, because `update_status` early-returns when nothing changed and so no forced apply is
guaranteed afterwards. Both are latent: no in-tree component writes those statuses today; the
exposure is a `kubectl patch --subresource=status` runbook, a migration or GitOps job, or a future
co-writer.

**#1084 — `namespaceUid` is not a UID.**
The `tenants/<x>` keyspace segment and the operation-document field `namespaceUid` both carry the
namespace NAME. That misnomer already produced two real defects: an automation pushed a commit
inverting the consumer to the real Namespace object UID (reverted in PR #1083 — merged, it would
have orphaned every existing object and stopped the catalogue advancing), and the e2e fixtures
uploaded under the UID, so discovery scanned a prefix nobody wrote to and reported `found: 0`.
Prose comments are holding the contract together, which is not enforcement.

## Proposed outcome

- A discovery tick reports everything it learned in **one** apply, so nothing it wrote earlier in
  the same tick can be reverted, and the fields a scan is meant to advance do advance:
  `lastSuccessfulScanTime`, `lastDiscoveredCount`, the `Discovered` condition, and the
  `, pruned N dangling` message from #1083.
- Job creation follows the stale window instead of the tick, so a healthy schedule costs one
  discover Job per stale window rather than one per tick, and the gate honours the most recent
  discovery signal even after a schedule has failed at least once.
- The **real** cadence is stated honestly in the documentation and in the tests: after the fix the
  nominal Job-creation period is `T*(floor(S/T)+2)` seconds (where `T` is the scan interval and `S`
  is the stale threshold), assuming the Job completes before the next tick. With the **new** defaults
  (300s tick, 300s threshold — approved by the user as part of this fix) that is
  `300*(floor(300/300)+2) = 900s ≈ 15 minutes`. Retaining the old 900s stale default under the fixed
  algorithm would give `300*(floor(900/300)+2) = 1500s ≈ 25 minutes`; that figure is a counterfactual,
  not the previous production behaviour, which was one Job per tick because the gate read a frozen
  condition. In e2e (30s tick, 60s threshold) the nominal period is `30*(floor(60/30)+2) = 120s`.
  Completion and reconcile delay add further latency beyond the Job-creation period, so the figures are
  nominal and no hard wall-clock catalogue guarantee is made.
- Controller-owned status applies cannot be blocked by a field owner that is not the controller, and
  that property is proven on the wire rather than asserted about a builder.
- The namespace identity contract is stated in one place, exercised by tests that fail if a producer
  or consumer diverges, and never again relies on a reader trusting the word `Uid`. The wire names
  stay frozen; the internal names, fixtures, plans and examples tell the truth.
- Every deferred item is recorded with a pointer instead of being silently skipped: the
  `DiscoveryFailed` polarity and the never-populated `lastError` (#1087), the never-written
  `observedGeneration` and the test that asserts an unreachable path (#1086), the oauth2 wire test
  gap, and #1084's wire rename and any keyspace migration.

## Affected users and systems

- Operators running backup discovery: `status.discovery` becomes trustworthy again
  (`lastSuccessfulScanTime`, `lastDiscoveredCount`, `Discovered`), and their persisted state changes
  shape on upgrade — a field that was silently deleted reappears.
- Object-storage cost and S3 request load: a healthy schedule issues one discover Job per stale
  window instead of one per tick (fewer `LIST` requests).
- Anyone reading the docs for the discovery cadence: the effective period is now correctly described
  by the formula `T*(floor(S/T)+2)`, and the documentation is corrected to state it. With the new
  defaults (300s tick, 300s threshold) the nominal period is 900s / 15 minutes. This is a behaviour
  change users must be told about, not a free fix.
- Operators triaging `phase: Invalid` leftovers and pruned dangling records: the `, pruned N
  dangling` message and the `Discovered` condition's `lastTransitionTime` become durable, and the
  assertions PR #1083 deliberately omitted from e2e are restored in
  `backup_discovery_prunes_dangling_invalid_record`.
- Repository and OAuth2 users: an incident runbook, migration or GitOps job that patches
  `.status` can no longer wedge `Ready`/`EncryptionKeyReady` on a `KanidmBackupRepository` (which
  gates the transport sidecar, so effectively all backups) or stop a `KanidmOAuth2Client`'s image
  status from converging.
- Anyone who writes or reads the object-store layout: `tenants/<namespace-name>/` is documented and
  tested as the namespace name, and the four frozen `namespaceUid` wire fields carry a doc comment
  saying so.
- Test infrastructure: `libs/backup` gains a shared `#[cfg(test)]` mock-client helper and `tower-test`
  becomes a workspace dependency, which is the pattern future wire-level controller tests should use.

## Constraints and non-goals

- `status.discovery.conditions` is `x-kubernetes-list-type: map` keyed on `type`. The single apply
  MUST still declare the complete merged condition list this manager owns: an apply that omits a
  condition the same manager owns **releases and deletes** it.
- One field manager for `status.discovery` (`backup-discovery`), and the apply stays forced
  (`PatchParams::apply(DISCOVERY_CONTROLLER_ID).force()`). No second manager, no per-condition apply.
- `DISCOVERY_CONFIRMATION_GRACE` (1800s, `libs/backup/src/controller/backup.rs`) bounds the
  deletion hold against a stuck pre-deletion discover Job; it does not bound catalogue latency.
  The invariant test asserts it outlives `BACKUP_JOB_TTL_SECONDS` (600s).
- Do not change the `DiscoveryFailed` polarity or populate `lastError` here (#1087): flipping a
  condition status is user-visible and changes what the staleness gate reads, so it must be designed
  against the post-#1082 shape.
- Do not rename a `namespaceUid` wire field, do not migrate the keyspace, and do not add webhook
  validation for UID-shaped values (a wire rename or keyspace migration stays out of scope).
  `#[serde(rename_all = "camelCase")]` means a Rust rename silently renames the wire, and
  `ManifestSource` is persisted inside every `manifest.json` in object storage with no
  `#[serde(default)]`.
- The only operator-default change authorized here is `BACKUP_DISCOVERY_STALE_SECS` 900s -> 300s
  (scan interval unchanged at 300s). No other default changes, and no default change made merely to
  make the cadence match the documentation.
- Do not change the derivation inputs of `derive_backup_id` (transport re-upload idempotency) or
  `compute_safety_backup_id` (re-derived every reconcile) — both are UUIDv5 over the namespace name.
- No CRD schema, chart, RBAC or new user-facing field change; `make crdgen` must stay clean.
  `make examples` intentionally changes one example value (`spec.manifestKey`) and must be committed.
- `Cargo.lock` changes must be committed: `make lint` runs clippy with `--locked`.
- Never enable `integration-test` and `e2e-test` together; e2e stays in CI's `kanidm-backup` shard.

## Success measures

- Unit tests in `libs/backup/src/controller/discovery.rs` prove the fold: a tick that reports both
  `Discovered` and `DiscoveryTruncated` keeps the success timestamp, the count and the message; a
  tick never deletes a condition it does not update; the gate selects the latest of
  `Discovered`/`DiscoveryFailed`; and a fresh tick re-declares the current signal, preserving that
  condition's `lastTransitionTime` while the scalar `lastScanTime` still refreshes. Each of these was
  shown to fail by reintroducing the old behaviour (stale-base fold, `.find()` gate selection,
  omitting the untouched conditions) in a throwaway worktree of the base commit, then restored.
- Wire-level tests prove the force at the two sites where it is asserted on the request:
  `patch_repository_status_applies_forced` (added here) and `patch_backup_status_applies_forced`
  (retroactive coverage for PR #1083's force), each asserting the request URI carries
  `?&force=true&fieldManager=…`. Removing `.force()` makes those assertions fail — executed by the
  implementing agent and restored, not just reasoned. The discovery apply's `.force()` is
  **pre-existing** (present at the base commit in `apply_discovery_status`'s predecessor
  `update_discovery_status`), is not asserted at query level, and is covered at path level only: the
  seam test checks the status request path contains `…/kanidmbackupschedules/<name>/status`, not the
  `force` parameter.
- Contract tests prove the namespace identity: a golden keyspace test with a name-shaped value, a
  pinned `derive_backup_id` derivation, a strengthened `compute_safety_manifest_key` test, and the
  seam test `run_discovery_scan_passes_namespace_name_not_uid_to_job`, which drives the full
  `run_discovery_scan` against a mocked API server and shows the value handed onward is the namespace
  name and not the `Namespace` object UID. That test was shown to be non-vacuous by changing the
  production argument to the UID: it failed with the name-vs-UID message, then passed once restored.
- `make test` (clippy `--locked --all-targets --all-features -- -D warnings` with zero warnings,
  `cargo fmt --check` clean, 997 passed / 0 failed / 0 ignored across 27 suites),
  `cargo check` / `clippy -p kaniop-e2e-tests --features e2e-test`, `make check-e2e-shards` (223 tests
  across 8 shards, `kanidm-backup` still 23), `make agent-framework-check`, `make llms-check`,
  `make crdgen` (no drift) and `make examples` (no drift beyond the intended
  `examples/backup.yaml` `manifestKey`) are green on the stable tree, and the independent verifier
  completed with HIGH confidence, no Critical findings and no code defect. CI's `kanidm-backup` shard
  and the e2e runtime on both arches are still outstanding.
- `Documentation/src/usage/backup-restore.md` states the real cadence; the #1082 caveats in
  `Documentation/src/usage/backup-restore.md` and `Documentation/src/troubleshooting.md` are gone.

## Open questions

None blocking. Recorded as decisions in `spec.md`: the single-apply shape, the latest-transition
gate selection, forcing both status sites, the internal-only `#1084` rename, and the authorized
lowering of the `BACKUP_DISCOVERY_STALE_SECS` default from 900s to 300s. Recorded as deferred with
pointers: #1086, #1087, the oauth2 wire test, and any `#1084` wire change. `plan.md` records what is
in the tree, which gates have been run green, what the independent verifier confirmed, and what CI
still has to prove.
