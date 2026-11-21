# Specification: bounded restore Jobs and pre-quiesce repository validation

Derived from: `intent.md` at d84b421f

## Required behavior

1. **Placement.** Every restore Job (safety backup, source preparation, source check, restore,
   verify) mounts the primary PVC and therefore copies, from the primary replica group:
   `nodeSelector`, `tolerations` and `affinity.nodeAffinity`. `podAffinity`,
   `podAntiAffinity` and `topologySpreadConstraints` are not copied: they target Kanidm pods,
   could make a Job unschedulable while the target is quiesced, and node-local PV affinity
   already pins the node.
2. **Deadline.** Every restore Job sets `activeDeadlineSeconds`:
   - 1800 s (30 min) for Jobs that move a whole database payload: safety backup (dump and
     upload), source preparation (download), restore and verify;
   - 900 s (15 min) for the local source check Job, which only reads a file.
   The values bound how long a stuck Job holds the target quiesced; the restore as a whole can
   still run for several Job deadlines in sequence. The triage suggested 15 min for
   the safety Job; it moves the same payload as source preparation, and a false timeout aborts a
   valid restore, so it gets 30 min.
3. **Job failure detection.** A Job counts as failed when `status.failed > 0` or it carries the
   condition `Failed=True` (for example `DeadlineExceeded`, where the deleted Pending pod is not
   guaranteed to be counted). `FailureTarget` alone is not treated as failed, so no phase
   resumes the target while a Job pod may still be terminating with the PVC mounted.
   Existing phase handling then applies: before the mutation boundary the target is resumed and
   the restore is `Failed`; after it the restore is `Failed` with `databaseMutationStarted=true`
   and force-release can release the lock.
4. **Repository validation in `Validating`**, for the safety repository (when the safety
   backup is not skipped) and for the source repository of a `backupRef` restore:
   - the repository must exist; a 404 fails with
     `<usage> KanidmBackupRepository '<name>' not found in namespace '<ns>'`;
   - it must report `Ready=True` with reason `Accepted`;
   - if it references a KEK (`encryption.keyRef`, client-side mode) it must report
     `EncryptionKeyReady=True`, and the KEK Secret must exist and contain the referenced key
     (read directly, because repository status can lag a deleted Secret). Repositories without a
     KEK are not gated on `EncryptionKeyReady`: no Job depends on a key Secret for them (same
     rule as transport sidecar injection).
   - Exception: a **safety** repository whose controller has not reported a `Ready` condition yet
     passes (status and KEK Secret checks are both deferred). The existing usage-lock gate
     keeps the restore in `Validating`, with `status.message` "waiting for the safety backup
     repository to report its status and usage lock", until the repository reports, and every
     `Validating` pass re-runs all checks. Once the usage lock is seen, the safety repository is
     checked once more without the exception before quiescing, because the repository's first
     status write can carry the lock together with `Ready=False`. This preserves today's
     "create repository and restore together" flow. The source repository keeps today's strict behavior.
   Any failure ends the restore `Failed` before `Quiescing`: `databaseMutationStarted=false`,
   no maintenance annotation, replica counts unchanged, no Job created. Retryable API errors
   (429/5xx) are retried instead of failing the restore.
5. **Dependency deleted after quiesce.** In `SafetyBackup` or `PreparingSource` with
   `databaseMutationStarted=false`, a 404 from the Kubernetes API (repository, backup or Job
   deleted while the restore runs) resumes the target and ends the restore `Failed`, instead of
   retrying forever with Kanidm at zero replicas. If the phase Job is still running, or a pod
   labelled `job-name=<job>` has not reached `Succeeded`/`Failed` (a deleted Job's pod can still
   be terminating), it may hold the PVC, so the controller keeps retrying until it ends. After
   the mutation boundary the restore keeps
   today's fail-closed retry behavior.

## Acceptance criteria

- `job_state_from` returns `Failed` for a Job with `Failed=True`/`DeadlineExceeded` and
  `failed=0` (regression test fails on v0.24.1 code with `Running`).
- Restore Job specs carry the deadlines and primary placement (unit test on the placement
  helper; e2e `restore_jobs_are_owned_by_restore` asserts restore, verify and source check Jobs).
- e2e `restore_missing_safety_repository_fails_before_quiesce` and
  `restore_missing_kek_secret_fails_before_quiesce` end `Failed` with the target untouched.
- `dependency_gone_before_mutation` is true only for a 404 in `SafetyBackup`/`PreparingSource`
  before the mutation boundary (unit tests in `controller.rs`).

## Design

Pure helpers in `legacy.rs`: `job_state_from(&Job)`, `restore_job_scheduling(&Kanidm)`,
`repository_not_ready_reason`, `kek_secret_problem`; one async `validate_restore_repository`
(GET repository, GET KEK Secret) used from `validate()` and `validate_backup_ref()`.
`controller.rs` `reconcile_apply` wraps the phase dispatch with the pre-mutation 404 handling
(`dependency_gone_before_mutation`, then `resume_before_mutation` and `Failed`).

## Interfaces and data

No CRD, status, RBAC or Helm change. The operator already has `get` on Secrets and
KanidmBackupRepositories. Failure messages for the source repository gain a `source ` prefix.

## Security and privacy

The KEK Secret is read only to check that the key entry exists; its value is not logged or
copied.

## Kubernetes, upgrade, and compatibility considerations

- Jobs already created by an older operator keep their spec (the controller never recreates an
  existing Job); restores in flight across the upgrade keep the old behavior for that Job.
- `activeDeadlineSeconds` and the `Failed` condition are GA Job API on every supported version.

## Operational behavior and rollback

A restore that previously hung now fails within 30 minutes. Rollback is a downgrade; no data or
schema migration is involved.

## Concerns and unresolved questions

- Very large databases on slow storage could exceed 30 minutes; the failure is safe before the
  mutation boundary and recoverable with force-release after it. A configurable deadline needs a
  CRD change and is deferred.
