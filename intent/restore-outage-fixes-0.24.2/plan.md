# Implementation plan: restore outage fixes for 0.24.2

Derived from: `spec.md` at d84b421f

## Repository findings

- Restore Job builders (v0.24.1 lines): `ensure_database_job` (legacy.rs:1416),
  `ensure_source_check_job` (:1812), `ensure_safety_backup_job` (:2350),
  `ensure_source_prep_job` (:2607). All mount the primary PVC; none set placement or a deadline.
  Kanidm pods get placement in `reconcile/statefulset.rs:364-368` (read only, not edited).
- `job_state` (:1493-1506) reads only `status.failed`/`status.succeeded`.
- `validate()` (:702-772) never touches the safety repository; `safety_repository_usage_locked`
  (:2233-2257) returns the 404 with `?` from the Validating and SafetyBackup arms, outside
  `validate()`, so the error loops instead of failing the restore.
- `decide_cleanup` (restore/controller.rs:160-167) and force-release need phase `Failed`.
- Restore integration coverage lives in e2e (`tests/e2e/test/kanidm/restore.rs`, shard
  `kanidm-restore`); `make integration-test` has no restore tests.

## Files and components

- `libs/operator/src/kanidm/restore/legacy.rs`: helpers, Job specs, validation, unit tests.
- `libs/operator/src/kanidm/restore/controller.rs`: pre-mutation 404 handling, unit tests.
- `tests/e2e/test/kanidm/restore.rs`: placement/deadline assertions, two new failure tests.
- `Documentation/src/usage/backup-restore.md`, `Documentation/src/troubleshooting.md`.
- `evals/cases/restore-pending-job-hang/`.

## Ordered implementation steps

1. Extract `job_state_from(&Job)`; add a failing unit test for `Failed=True`/`DeadlineExceeded`
   with `failed=0`; confirm it fails with `Running`; then honor the `Failed` condition.
2. Add `restore_job_scheduling` and the two deadline constants; set them on the four Job specs.
3. Add `validate_restore_repository` (+ pure helpers) and call it from `validate()` for the
   safety repository (`allow_unreported = true`) and from `validate_backup_ref()` for the source
   repository (`allow_unreported = false`).
4. Unit tests (pure helpers and `tower_test` mock API for 404 / missing Secret / unreported).
5. e2e tests and docs; eval case.
6. After independent review: resume and fail on a pre-mutation 404 in `SafetyBackup`/
   `PreparingSource` (`controller.rs`); retry 429/5xx in `Validating`; defer the KEK Secret check
   for an unreported safety repository; report the usage-lock wait in `status.message`.

## Dependencies and coordination

Another change set edits `cmd/data-mover`, `cmd/webhook`, `libs/backup/src/controller/schedule.rs`,
`libs/backup-core` and `reconcile/{statefulset,transport}.rs`; this plan touches none of them.

## Risks and blast radius

- A legitimately slow Job is killed at 30 min: safe before mutation (target resumed), and
  recoverable with force-release after.
- Source-repository validation now also requires a readable KEK; a restore that would have hung
  at source preparation now fails in `Validating` instead.
- Copying a primary `nodeAffinity` that no longer matches any node makes the Job Pending, as
  the Kanidm pod would be; the deadline bounds it.

## Alternatives rejected

- Copying the full `affinity` and topology spread: can deadlock the Job against the quiesced
  Kanidm pods' own rules.
- A per-phase timeout in the controller: duplicates the Job API's deadline and needs new status.
- Treating `FailureTarget` as failed: could resume Kanidm while a Job pod still holds the PVC.
- Placement for backup/discovery Jobs in `libs/backup`: they mount no Kanidm PVC.

## Verification

- `make lint` (zero warnings), `make test` (all unit tests), `make agent-framework-check`.
- `cargo test -p kaniop-operator --lib restore::` with the `job_state` regression test.
- e2e shard `kanidm-restore` in PR CI (not run locally per AGENTS.md).

## Rollback

Revert the commit; no CRD or data change.

## Deviations

- Safety backup Job deadline is 30 min, not the triage's 15 min (see spec, Required behavior 2).
- An unreported safety repository passes status checks so the existing usage-lock wait handles
  the create-together flow (spec, Required behavior 4).
- Added pre-mutation 404 handling (spec, Required behavior 5) and retry of transient errors in
  `Validating`, found in independent review: without them a dependency deleted after quiesce
  still held Kanidm at zero replicas, and an API blip could fail a valid restore.
- A second review found that the first repository status write can carry the usage lock with
  `Ready=False`, so the safety repository is re-checked strictly after the lock is seen, and the
  resume path also waits for terminating pods of the phase Job.
