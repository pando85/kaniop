# Specification: collision-safe backup discovery

Derived from: `intent.md` in this change.

## Required behavior

1. Continue to prefer legacy names `kb-<first 8 backup-id chars>`. This protects existing references.
2. If creating a legacy name returns 409, GET that record. Full immutable spec equality means it is the same record; count it as reconciled.
3. If it differs, derive `kb-<id8>-<six hex chars of SHA-256(repository name)>`, warn, and CREATE.
4. If the alternate name returns 409, GET and verify full immutable spec equality. Treat mismatched or unreadable records as errors, never as reconciled.
5. Keep the naming helper in `kaniop-backup-core` for future reuse by the restore safety-record fix (#1226).

## Acceptance criteria

- Deterministic names, stable original names, and distinct names for repositories.
- Mock API tests for alternate creation, matching legacy record, and mismatched alternate record (fail closed).
- Integration test involving two repositories and the same backup ID (to be added before merge).
- Retention/GC and restore work independently for both catalog records.

## Security and privacy

This changes catalog identity, not S3 storage or the content of existing objects. A failed collision resolution must be observable and retried, not silently ignored. No S3 deletion is permitted by this change.

## Kubernetes, upgrade, and compatibility considerations

No schema changes. Existing legacy names survive; alternate names stay within Kubernetes name length constraints and are deterministic across operator restarts. If a conflicting record is removed between CREATE 409 and GET, the scan fails safely and retries later.

## Operational behavior and rollback

Rollback leaves alternate catalog CRs intact. Older discovery versions may report a 409, but must not delete either copy. Backups remain stored in S3.

## Concerns and unresolved questions

Six hex characters give only 24 bits of repo hash. Exact fallback-name collisions are detected, rather than treating unrelated records as equivalent. Consider a longer fallback suffix only if evidence shows this is inadequate.
