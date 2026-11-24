# Implementation plan: cross-repository backup ID collisions

Derived from: `spec.md` in this change.

## Repository findings

- `libs/backup/src/controller/discovery.rs`: `build_backup_cr` uses `kb-<id8>`, and the 409 branch increments `reconciled_count` without GET.
- `reconcile_discovered_backups` lists existing records by repository label and skips already matching manifest keys.
- `libs/backup-core` is consumed by backup and operator crates, so it is the correct place for shared naming used later by #1226.

## Files and components

- `libs/backup-core/src/naming.rs` and `lib.rs`: deterministic naming and unit tests.
- `libs/backup-core/Cargo.toml`: workspace sha2 dependency.
- `libs/backup/src/controller/discovery.rs`: verify 409 identities and allocate alternate records.
- e2e backup test: two repositories, same backup ID, independent records and retention.

## Ordered implementation steps

1. Add and test shared naming functions.
2. On legacy 409, GET and compare the full immutable spec.
3. On a mismatch, try a deterministic repository-scoped name. Verify a second 409 too.
4. Test using a mocked Kubernetes API for all paths, including races and mismatches.
5. Run Rustfmt, Clippy, unit/e2e CI and an independent review.
6. Add a two-repository integration drill before marking ready for merge.

## Risks and blast radius

Erroneous collision handling can orphan backups, hide them from GC, or break existing references. No automatic catalogue migration or S3 deletion is introduced.

## Alternatives rejected

- Always renaming existing records: breaks references and may trigger unwanted GC.
- Treat any HTTP 409 as success: repeats data-safety bug.
- Random suffix: breaks idempotency under retries.
- Mutating existing immutable catalog records: risks linking the wrong payload.

## Verification

- `make lint && make test`
- Mock API tests for name collision and idempotence.
- e2e `kanidm-backup` shard with two repositories and same backup ID.
- Review all fallback error paths, including deletion races and malformed existing objects.

## Rollback

Revert controller and naming changes; no CRD migration. Preserve existing alternate CRs until a compatible version is deployed.

## Deviations

The Kubernetes+MinIO two-repository e2e remains a required pre-merge gate; initial PR is deliberately Draft.
