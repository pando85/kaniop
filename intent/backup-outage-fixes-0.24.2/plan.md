# Implementation plan: backup outage fixes for 0.24.2 (B1, B2, B5)

Derived from: `spec.md` at d84b421f

## Repository findings

- `cmd/data-mover/src/commands/transport.rs` `run()`:
  - `create_bucket(...).map_err(...)?` maps every error to `Retryable` and exits.
  - `backfill_existing_backups(...).await?` exits once `list_manifest_keys` has used up its
    `max_retries` (3, so about 14 s).
  - `wait_for_watch_dir` returns `Err(Retryable)` on shutdown, which gives a non-zero exit on
    SIGTERM.
- `cmd/webhook/src/handlers.rs` and `libs/backup/src/controller/schedule.rs` each have their own
  copy of `validate_cron_schedule`: `Schedule::from_str(expr)`, falling back to
  `from_str("0 " + expr)` for any field count, with no upcoming-date check.
- Kanidm v1.11.2 `server/core/src/interval.rs` `start_online_backup`: with exactly 5 fields it
  inserts `0` at the front and pushes `*` at the end, then parses with `cron` 0.17.0 and errors
  if `upcoming(Utc).next()` is None.
- `schedule.rs` returns `Err(MissingData("localVersions must be at least 2"))` before it patches
  status.
- `libs/operator/src/kanidm/reconcile/transport.rs` `resolve_backup_config` renders
  `schedule.spec.schedule` and `local_versions` unchecked.
- The CRD CEL rule in `libs/backup-core/src/crd.rs` is `>= 1`.
- The webhook rules match `kanidmbackupschedules` only, not `/status`.

## Files and components

| File | Change |
|---|---|
| `libs/backup-core/src/schedule.rs` (new), `lib.rs`, `Cargo.toml` | Validators and table tests; add the `cron` dependency |
| `libs/backup-core/src/crd.rs` | CEL `>= 2`; describe the `schedule` and `localVersions` rules |
| `cmd/webhook/src/handlers.rs`, `Cargo.toml` | Pure `validate_schedule_spec` using the shared validators; drop `cron`, add `kaniop-backup-core` |
| `libs/backup/src/controller/schedule.rs`, `Cargo.toml` | `spec_validation_error` gives `InvalidSchedule` or `InvalidSpec` and the existing status patch path; drop `cron` |
| `libs/operator/src/kanidm/reconcile/transport.rs` | `resolve_backup_config` returns None for invalid specs |
| `libs/operator/src/kanidm/reconcile/statefulset.rs` | Correct the readiness comment |
| `cmd/data-mover/src/commands/transport.rs`, `Cargo.toml` | `retry_startup_step` and `StartupStep`; shutdown exits 0; tokio `test-util` for dev only |
| `tests/e2e/test/kanidm/backup.rs`, `backup_transport.rs` | Never-firing cron and `localVersions: 1` rejected; invalid credentials keep the primary Ready |
| `charts/kaniop/crds/crds.yaml`, `examples/` | Regenerated with `make crdgen` and `make examples` |
| `Documentation/src/usage/backup-restore.md` | localVersions minimum, schedule validation, sidecar startup retries |
| `evals/cases/transport-startup-s3-error-readiness`, `evals/cases/backup-schedule-never-fires` | Eval cases |

## Ordered implementation steps

1. Add the regression tests (webhook, schedule controller, operator `resolve_backup_config`) and
   confirm they fail on d84b421f for the expected reason.
2. Add the backup-core validators, then wire them into the webhook, the controller and the
   operator.
3. Raise the CEL minimum, then run `make crdgen` and `make examples`.
4. Add the transport retry helper and its unit tests, and fix the statefulset comment.
5. Add the e2e cases, docs, intent artifacts and evals.
6. Run `make lint`, `make test` and `make agent-framework-check`. Open the PR and let CI run e2e.

## Dependencies and coordination

- Another change edits `libs/operator/src/kanidm/restore/` (B3/B4). This plan touches no file
  there.
- Later fixes B12, B14 and B16 touch `schedule.rs`, `handlers.rs` and the CRD CEL. Rebase them on
  this change and regenerate the CRDs.

## Risks and blast radius

- Webhook UPDATEs of existing invalid Schedules are denied. They were already broken; delete is
  allowed.
- A sidecar with bad credentials now runs indefinitely without uploading. Before, it took the
  pod down. Its logs show the cause.
- The validator depends on the `cron` crate behaving like Kanidm's. Both use `cron` 0.17.

## Alternatives rejected

- **Gate rendering on Schedule `Ready` status:** adds up to 300 s of lag and restart flaps, and a
  stale status could gate the wrong thing. A pure check is deterministic.
- **Keep the sidecar running but idle on any error:** a transient error would then need a Pod
  restart to recover. Retrying recovers on its own once credentials, permissions or the network
  are fixed. Only `InvalidInput` idles, because only a new Pod spec can fix it.
- **Exit on `InvalidInput`:** the first draft did this. Independent review showed that the
  webhook admits a scheme-less endpoint with `insecure: true`, which the data-mover rejects as
  `InvalidInput`. Exiting would reproduce the B1 outage, so the sidecar idles instead.
- **Add a readiness probe to opt the sidecar out of readiness:** a native sidecar that is not
  running still gates `ContainersReady`, and a probe cannot change that.

## Verification

- Reproduced before the fix on d84b421f:
  - `cargo test -p kaniop-backup -- never_firing` failed at schedule.rs `0 0 31 2 *` assert.
  - `cargo test -p kaniop-operator -- never_firing local_version` failed
    `never_firing_schedule_is_not_rendered` and `single_local_version_is_not_rendered`.
  - `cargo test -p kaniop-webhook -- never_firing` failed at the `0 0 31 2 *` assert.
- After the fix: `make lint`, `make test` and `make agent-framework-check`. The results are
  recorded in the PR.
- e2e in CI, shard `kanidm-backup`:
  - `backup_schedule_invalid_cron_rejected_by_webhook` (now includes the never-firing case)
  - `backup_schedule_single_local_version_rejected`
  - `backup_transport_invalid_credentials_keep_primary_ready`

## Rollback

Revert the PR, or downgrade to v0.24.1. There is no data migration.

## Deviations

- The B1 retry logic has no failing-first unit test, because `run()` cannot be unit-tested
  without S3. The live findings F3B.19 and F4A.8 are the reproduction. The new e2e
  `backup_transport_invalid_credentials_keep_primary_ready` fails on v0.24.1: the sidecar's
  `restartCount` rises above 0.
- The task said to exit only for genuinely invalid input. The implementation goes further and
  never exits for configuration at all: on `InvalidInput` it logs
  `transport configuration is invalid` and idles until shutdown. Reason: such input can be
  admitted (see "Alternatives rejected"), and exiting crashloops the native sidecar. The idle
  path reuses the non-primary idle loop (`idle_until_shutdown`).
- Independent review also corrected the spec on status-subresource CEL evaluation (ratcheting
  is needed for the `InvalidSpec` patch on pre-existing `localVersions: 1` objects). It also
  added documentation of cron 0.17's day-of-week numbering and of year-bounded schedules.
