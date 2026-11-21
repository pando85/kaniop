# Intent: Live review of every Kaniop backup and restore path

- Owner: maintainer
- Status: accepted (plan v3, approved 2026-11-21)
- Work item: no GitHub issue; maintainer-driven test campaign. Findings become issues,
  separate fix PRs, and eval cases.
- Related: [intent/1000-backup-restore-hardening](../1000-backup-restore-hardening/intent.md)

## Problem

Backup and restore are covered by unit, integration, and Kind e2e tests, but have never been
exercised end to end on a real multi-node cluster with node-local storage, a real S3 server
with TLS, KMS, Object Lock and split credentials, and a production Kanidm whose data matters.
A static review of the code and docs found suspected defects that the existing tests do not
cover (see `spec.md`, "Static-review findings to confirm live"). Some of them only show up
with real scheduling, real storage classes, or real S3 behavior.

While preparing the campaign (Phase 0), an escaped release defect was found: kaniop 0.23.0
release binaries die with SIGILL (exit 132) on CPUs without AVX. This blocks the campaign on
the target cluster and is itself an outcome of the review.

## Proposed outcome

- Every backup and restore option Kaniop supports is run once, live, on the homelab cluster
  `grigri`: backup, mutate, restore, verify.
- Each suspected defect from the static review is confirmed or refuted with evidence.
- Real-cluster fault cases (node drain during upload, S3 server restart mid-upload, PV leak on
  a `Retain` storage class, bad credentials, alert firing) are observed and recorded.
- A normal restore of production Kanidm is rehearsed through GitOps, with a safety backup and a
  ZFS snapshot as rollback.
- Findings are converted into GitHub issues, separate fix PRs, `evals/cases/` entries, and
  runbook updates in the homelab GitOps repo.

## Affected users and systems

- Kaniop users relying on backup and restore: findings drive fixes and docs corrections.
- The homelab production Kanidm (`kanidm/kanidm` on `grigri`): one planned restore through
  GitOps; otherwise read-only.
- Kaniop release pipeline: the SIGILL defect requires a baseline CPU target and a CI check
  (fixed in a separate PR, not here).

## Constraints and non-goals

- This change is a plan only. It adds no code and records no results.
- Never write to the production namespace with kubectl; production changes go only through the
  homelab GitOps repo, with owner approval per push.
- Never point a test repository at the production bucket.
- Never run the repository e2e harness against `grigri`.
- Code fixes for anything found are separate changes with their own PRs.
- Non-goals (already covered by e2e): concurrent restores, delete mid-restore, force-release,
  finalizer deletion, `keepLast` retention. `workloadIdentity` gets a static review only.

## Success measures

- Every row of the option matrix in `spec.md` has a recorded pass or fail with evidence.
- Every static-review finding is marked confirmed or refuted.
- Production Kanidm is healthy after Phase 1: both replicas up, replication working, logins and
  OAuth2 apps working, safety backup uploaded.
- No test objects, PVs, Jobs, or Secrets are left behind after Phase 5.
- Every confirmed defect has an issue; significant escaped defects (including SIGILL) have an
  eval case.

## Open questions

- Whether `timeZone` and `jitterSeconds` deserve a live run, or static confirmation is enough
  (currently: static plus one cheap run).
- Whether the MinIO-compatible image (`pgsty/silo`) supports a static KMS key; this is checked
  in Phase 2 before the KMS rows run.
