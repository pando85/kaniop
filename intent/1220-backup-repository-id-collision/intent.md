# Intent: catalog all copies of backups across repositories

- Owner: Kaniop maintainers
- Status: draft
- Work item: https://github.com/pando85/kaniop/issues/1220

## Problem

When two repositories contain the same backup ID, the discovery reconciler tries to create the same Kubernetes name `kb-<id8>` twice. On a 409 it reports the second copy as reconciled without checking whether the existing backup points at the same repository or manifest. The second copy remains uncatalogued, cannot be restored via `KanidmBackup`, and is invisible to normal retention and garbage collection.

## Proposed outcome

A backup is counted as reconciled only if the existing Kubernetes resource identifies the same immutable backup specification. Copies in different repositories receive distinct, stable names. Existing references and names remain unchanged.

## Affected users and systems

Backup discovery, multi-repository backfill, backup catalog, retention/GC, and safety-backup cataloging (#1226).

## Constraints and non-goals

- Never rename existing `KanidmBackup` resources.
- Never silently overwrite an existing record with another repository's data.
- Keep output names deterministic under retries and concurrent scans.
- No CRD migration. A fallback-name collision must remain visible as an error, not success.

## Success measures

- Two repository copies with the same backup ID can both be catalogued.
- A 409 for a matching spec is idempotent and counted as reconciled.
- A 409 for a differing spec triggers a repository-scoped name, then verifies that one too.
- Neither a same-name mismatch nor a failed GET is counted as reconciled.
