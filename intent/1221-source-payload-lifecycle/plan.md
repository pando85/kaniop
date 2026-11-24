# Implementation plan: restore source payload safety

Derived from: `spec.md` in this change.

## Repository findings

- `cmd/data-mover/src/commands/download.rs`: `tokio::fs::File::create` uses permissive default mode on the output.
- `libs/operator/src/kanidm/restore/legacy.rs`: source preparation writes to the primary PVC, and database restore reads that same file.
- `libs/operator/src/kanidm/restore/controller.rs`: post-verification success routes to rebuilding/resuming. Restore Jobs are separate and only share the PVC; a successful source-prep Job cannot remove the file immediately.
- `tests/e2e/test/kanidm/restore.rs`: restore shard has realistic PVC-backed tests and explicit non-root security contexts.

## Files and components

Stage A: only `cmd/data-mover/src/commands/download.rs` plus this intent documentation.
Stage B: restore controller, Job construction, relevant e2e tests, user-facing runbook.

## Ordered implementation steps

1. Implement owner-only file open and permission correction for existing files.
2. Add deterministic unit tests covering creation and replacement.
3. Run Rustfmt/Clippy/unit tests and CI.
4. Design post-verify cleanup and pre-mutation failure cleanup, with idempotent reconciliation and availability as a hard constraint; obtain independent review.
5. Implement Stage B on a separate PR, add e2e success/failure/restart tests, then close #1221.

## Dependencies and coordination

Stage B must account for native PV node affinity and any restore scheduling protections in PR #1212.

## Risks and blast radius

A 0600 file is unreadable to a restore Job running under a different UID. Validate inherited pod securityContexts and do not merge with untested UID assumptions. A cleanup Job can contend for a local PVC or become unschedulable; it must not hold the identity service down.

## Alternatives rejected

- Run `rm` inside `kanidm/server`: availability of /bin/sh is unverified.
- Delete the file immediately after source-prep: primary database restore still needs it.
- Reuse a single default-umask file open: recreates the original exposure.
- Assume deletion from PVC equals secure erasure: unsupported on modern storage.

## Verification

- `make lint`
- `make test`
- Run focused data-mover unit test for output permissions.
- e2e `kanidm-restore` with non-root UID/GID; explicitly verify that the restore command can read the file.
- For Stage B, assert file absence after Completed plus failure/restart semantics.

## Rollback

Revert the Stage A PR if a run-as UID mismatch emerges; no migration/CRD change.

## Deviations

Stage B intentionally remains unimplemented and unapproved; do not close #1221 from this PR.
