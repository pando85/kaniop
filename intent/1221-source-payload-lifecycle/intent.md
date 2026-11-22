# Intent: prevent plaintext restore payload exposure

- Owner: Kaniop maintainers
- Status: draft
- Work item: https://github.com/pando85/kaniop/issues/1221

## Problem

A remote restore downloads and, if applicable, decrypts a complete Kanidm database export into `/data/source-payload.json.gz` on the primary database PVC. File creation defaults to 0666 masked by umask, commonly 0644. The file survives the restore and PV retention. This exposes credentials and secret material despite client-side encryption at rest in S3.

## Proposed outcome

The plaintext payload has least-privilege permissions while in use and is absent after a completed restore. Failures and restarts must not leave an indefinitely exposed copy. A cleanup failure must not turn an otherwise successful restore into an identity-service outage.

## Affected users and systems

All remote restores using `KanidmBackup`, including client-side encrypted repositories, `kaniop-data-mover` and `KanidmRestore` controller/Jobs.

## Constraints and non-goals

- Do not remove source bytes before a successful `kanidmd database restore`.
- Do not claim that unlinking securely erases bytes from copy-on-write/snapshot volumes.
- Do not assume all supported Kanidm images contain a shell.
- Preserve CRD compatibility and keep local restores unchanged.
- Permissions hardening is independently safe to ship; lifecycle cleanup requires design review.

## Success measures

- Mode 0600 for newly downloaded payloads and overwritten historical files.
- Completed remote restores leave no source payload on the PVC.
- Failures before the mutation boundary clean up safely and restart Kanidm promptly.
- Cleanup Job failures are observable and recoverable without sacrificing identity availability.

## Open questions

- How should post-verify cleanup be scheduled when the primary PVC is node-local and the primary pod is serving?
- Which status/condition exposes failed cleanup without misrepresenting database restore success?
- What cleanup guarantees are possible after process SIGKILL or node loss?
