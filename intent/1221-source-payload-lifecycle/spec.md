# Specification: private and ephemeral restore source payload

Derived from: `intent.md` in this change.

## Required behavior

R1. Create decrypted output with owner-only (0600) permissions on Unix. If an older file already exists, tighten its mode before writing plaintext.

R2. Preserve the payload through source checksum verification and successful primary database restore. Delete it only after a successful database verification and once no Job needs it.

R3. On errors before database mutation, remove any partially downloaded payload and restore the original replica topology without waiting for a cleanup Job that cannot start.

R4. Controller/restarts and finalizer deletion must not bypass the cleanup protocol. Missing files are already cleaned; retries are idempotent.

R5. A cleanup failure must never make an otherwise serving Kanidm pod unavailable. Report persistent failure and offer an explicit recovery path.

## Acceptance criteria

- A new file and an existing historical 0644 file both end up 0600 before downloaded data is written.
- e2e remote restore reaches Completed and `test ! -e /data/source-payload.json.gz` on the primary PVC.
- Source-prep errors, mismatched manifest/checksum, Job failures, and controller restarts exercise cleanup.
- Other local restore/backup paths are unaffected.

## Design

**Stage A (this PR):** implement owner-only file creation in the data mover and independent regression tests. This reduces exposure but DOES NOT fulfill R2-R5.

**Stage B (pending independent review):** use an explicit restore-owned cleanup operation on the same PVC, preferably a bounded Job running the known data-mover image rather than a shell inside the Kanidm server image. Design Job scheduling (PV affinity, pod securityContext, resource limits), cleanup before completion, retry semantics and pre-mutation failure handling. Bring Kanidm back to service before waiting for any post-verify cleanup Job so that cleanup cannot create an IdM outage. Do not implement Stage B until failure semantics are agreed.

## Interfaces and data

No API changes in Stage A. File is internal; mode 0600; source download path remains `/data/source-payload.json.gz`.

## Security and privacy

Containing the data in a 0600 file does not protect against root, the owner UID, volume snapshots, stale historical copies, or compromise of a privileged pod. Removal is not cryptographic erasure.

## Kubernetes, upgrade, and compatibility considerations

Source-preparation and database restore Jobs inherit the target primary group's pod securityContext; verify identical UID/group permissions in e2e before merging Stage B.

## Operational behavior and rollback

Stage A can be rolled back without CRD changes, but doing so restores insecure file permissions for new downloads.

## Concerns and unresolved questions

Phase transitions and file cleanup must not recreate the backup/restore availability bugs fixed by #1212 and #1213.
