# Specification: backup outage fixes for 0.24.2 (B1, B2, B5)

Derived from: `intent.md` at d84b421f

## Required behavior

### B1: the transport sidecar does not exit on transient or credential failures

- On the designated primary, the startup steps "create S3 client" and "startup backfill
  listing" are retried in-process until they succeed. Each retry is logged at WARN as
  `transport startup step failed; retrying ...` with the step name, the attempt number, the exit
  code the step would have used and the next delay.
- The backoff is exponential: it starts at 5 s, doubles, and is capped at 300 s.
- SIGTERM or SIGINT during a backoff, during a startup attempt, or while waiting for the watch
  directory, ends the process cleanly with exit code 0. Dropping an in-flight startup attempt is
  safe because the startup steps only read from S3.
- `ExitCode::InvalidInput` is not retried. That covers configuration that cannot become valid
  without a new Pod spec: an invalid repository path or manifests prefix, a missing endpoint or
  region, or an endpoint the S3 client rejects (no `http(s)://` scheme, or plain HTTP to a
  non-loopback host without `insecure`). The webhook admits some of these, for example a
  scheme-less endpoint with `insecure: true`. So on the primary the sidecar logs
  `transport configuration is invalid` and idles until shutdown, without uploading. It does not
  exit: exiting would crashloop the native sidecar and cause the same outage. A fixed
  configuration rolls the Pod anyway. Every other failure is retried, including `Retryable`,
  `Authorization` and missing credentials.
- Only an unreadable or wrong operation document still ends the process before startup. The
  operator renders that document, so this is an operator bug, not user configuration.
- The poll loop is unchanged. It already treated upload errors as transient.

### B2: one cron validator that matches Kanidm

- `kaniop_backup_core::schedule::validate_backup_cron_schedule(schedule)` applies Kanidm
  1.11.x's rewrite exactly. It splits on ASCII whitespace. With exactly 5 fields it prepends
  `0` and appends `*`. Any other field count is passed through unchanged. The fields are joined
  with single spaces.
- The result must parse with `cron` 0.17, the version Kanidm 1.11.2 uses, and
  `upcoming(Utc).next()` must be `Some`.
- Error messages show both the user's expression and the expression Kanidm evaluates. They say
  either "does not parse" or "never fires".
- Callers:
  - **webhook** (`validate_backup_schedule`): denies CREATE and UPDATE. The message contains
    `invalid cron schedule`, which the existing e2e checks for.
  - **schedule controller**: patches `Ready=False` with reason `InvalidSchedule` and requeues.
    This replaces the old local validator.
  - **`resolve_backup_config`**: returns `None`, so no `[online_backup]` block, no
    `KANIOP_BACKUP_*` variables and no transport sidecar are rendered.

### B5: `localVersions` must be at least 2

- The CRD CEL rule becomes `self.localVersions >= 2 && self.localVersions <= 3650`, with the
  message `localVersions must be between 2 and 3650`.
- `kaniop_backup_core::schedule::validate_local_versions` rejects values below
  `MIN_LOCAL_VERSIONS = 2`. Its message starts with `localVersions must be at least 2`.
- The webhook denies such values. The schedule controller patches `Ready=False` with reason
  `InvalidSpec` instead of returning `Err` before it writes status. `resolve_backup_config`
  returns `None`.

### Documentation correction

- The comment on the transport sidecar in `statefulset.rs` must say that a native sidecar that is
  not running does gate Pod readiness.

## Acceptance criteria

1. A unit test of the retry helper injects a failing closure and proves the following: retryable
   and authorization errors are retried until success with backoff 5, 10, 20, 20 s (cap 20 s in
   the test); `InvalidInput` returns after one attempt; and shutdown during a backoff or during an attempt
   returns `Shutdown`.
2. A table-driven unit test in backup-core covers 5, 6 and 7 fields, `@daily`, `@hourly` and
   `@weekly`, common crons, `0 0 31 2 *`, a 7-field expression with a past year, 5 fields plus a
   year, and syntax errors.
3. The webhook, schedule controller and operator tests for `0 0 31 2 *` and `localVersions: 1`
   fail on d84b421f and pass with the fix.
4. In e2e, the webhook rejects `0 0 31 2 *` and `localVersions: 1` is rejected at admission.
   CEL runs before the webhook, so the e2e exercises the CRD rule; unit tests cover the webhook.
   A primary with a sidecar on invalid credentials stays Ready with `restartCount == 0`.
5. `make lint`, `make test` and `make agent-framework-check` pass. The CRDs and examples are
   regenerated.

## Design

- The validators live in `kaniop-backup-core` because the operator crate cannot depend on
  `libs/backup`. `cron` moves from `kaniop-backup` and `kaniop-webhook` to `kaniop-backup-core`.
  It stays a workspace dependency.
- The retry helper is generic over the async closure (`FnMut() -> Future<Output = Result<T,
  i32>>`). That lets a test inject failures without S3. It returns
  `Result<StartupStep<T>, i32>`, where `StartupStep` is `Done(T)` or `Shutdown`.
- The listing's own short retry loop (`max_retries`, about 14 s) stays inside each attempt, so
  each outer attempt is one complete listing try.
- The validity check in `resolve_backup_config` is pure. It does not depend on Schedule status, so
  it adds no restart flaps and no status lag.

## Interfaces and data

- New public module `kaniop_backup_core::schedule`: `kanidm_cron_expression`,
  `validate_backup_cron_schedule`, `validate_local_versions` and `MIN_LOCAL_VERSIONS`.
- CRD: only the CEL rule, its message and the field descriptions change. There is no schema
  change.
- New condition reason `InvalidSpec` on `KanidmBackupSchedule` `Ready`. `InvalidSchedule`
  already existed.

## Security and privacy

- Retry logs carry the step name, the attempt number and the existing error text. No
  credentials are logged; `S3Error` messages do not include secrets.

## Kubernetes, upgrade, and compatibility considerations

- **Existing objects with `localVersions: 1`.** The CEL rule is attached to `spec`. With CRD
  validation ratcheting (Kubernetes 1.30+), metadata-only updates pass CEL. But the webhook
  validates the whole spec on every UPDATE, as it already did for the cron, so every update is
  rejected until `localVersions` is fixed. A fixing update (`localVersions >= 2`) passes. That
  object
  never produced a backup, and deleting it is always allowed: the Schedule has no finalizer.
  Status updates go through the status subresource, which the webhook does not intercept. The
  apiserver still evaluates the CRD's CEL rules over the whole object on status updates. With
  ratcheting (default on since Kubernetes 1.30) the unchanged spec passes, so the
  `Ready=False InvalidSpec` patch succeeds. On clusters without ratcheting (1.29, or with the
  feature gate off) that patch is rejected for a stored `localVersions: 1` object. The
  controller then returns an error, as v0.24.1 did, but `resolve_backup_config` still refuses
  to render the object, so the data-safety fix holds there too.
- **Existing never-firing schedules.** On upgrade the operator stops rendering them. The
  StatefulSet rolls back to a template without the backup block, and Kanidm can start again.
  A pod already stuck under OrderedReady still needs a manual delete (O7, out of scope).
- **Sidecar version skew.** Old sidecars keep exiting until the pod runs the new data-mover
  image, which ships in the same release.

## Operational behavior and rollback

- **Rollback:** revert the release. The behavior falls back to v0.24.1. Downgrading the CRD
  restores `>= 1`.
- **Visibility:** a sidecar retrying with bad credentials is visible only in its logs. This
  release does not change status or metrics (B17).

## Concerns and unresolved questions

- The sidecar retries forever with bad credentials, and idles with invalid configuration. That
  is intentional: losing backups must not take IdM down. B17 tracks making it visible.
- Validity depends on the current date, as it does in Kanidm. A 7-field schedule restricted to a
  year becomes invalid once that year has passed. The operator then stops rendering it, which
  rolls the StatefulSet. Kanidm would refuse to restart with it anyway.
- In `cron` 0.17, as in Kanidm, day of week runs 1-7 with Sunday as 1, so `0 2 * * 0` is
  rejected. This is documented, not changed.
