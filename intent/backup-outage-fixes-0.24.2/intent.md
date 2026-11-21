# Intent: A backup problem never takes Kanidm down or silently stops backups

- Owner: pando85
- Status: draft
- Work item: v0.24.1 live backup and restore review (campaign plan in #1207), bugs B1, B2 and B5;
  target patch release 0.24.2

## Problem

The v0.24.1 live review found three ways a backup configuration problem breaks Kanidm itself or
loses every backup without any signal.

- **B1, outage.** The `data-mover-transport` native sidecar exits when its startup steps fail:
  creating the S3 client, or the startup backfill listing of existing manifests. It exits on
  rejected credentials, on a missing `s3:ListBucket` permission, and when S3, TLS or DNS is
  unavailable while the primary pod starts. The sidecar then crashloops, and a native sidecar
  that is not running makes the pod `ContainersReady=False`. The Kanidm primary leaves its
  Service EndpointSlice. With one replica that is a full IdM outage caused only by a backup
  credential (findings F3B.19 and F4A.8: `403 AccessDenied` and `403 InvalidAccessKeyId`,
  `Init:CrashLoopBackOff`, EndpointSlice `ready=false`, Kanidm `1/2`). The Schedule and the
  repository stayed `Ready=True` the whole time. A code comment in `statefulset.rs` claims the
  sidecar does not affect pod readiness, which is false.
- **B2, outage.** A cron that parses but never fires, such as `0 0 31 2 *`, passes the webhook and
  the schedule controller, and the Schedule reports `Ready=True`. Kanidm 1.11.2 rewrites it to
  `0 0 0 31 2 * *`, finds no upcoming date and refuses to start: "Online backup schedule error:
  '0 0 0 31 2 * *' will not match any date" (finding F4A.3). The primary crashlooped for about 5
  minutes. Kaniop's validator also disagrees with Kanidm on field counts: it retries any
  expression with a `0 ` prefix, but Kanidm only rewrites exactly five fields. So five fields plus
  a year are accepted by Kaniop and rejected by Kanidm.
- **B5, data safety.** `localVersions: 1` is admitted by the CRD (`>= 1`) and by the webhook. The
  schedule controller returns an error before it writes any status, so no condition and no event
  appear. The operator still renders `KANIOP_BACKUP_VERSIONS=1`. Kanidm then deletes each backup
  file at the next tick, before the sidecar's 120 s minimum file age, and nothing is ever
  uploaded. In the live run 0 of 4 scheduled backups were uploaded (findings F4A.1 and F4B.3).

## Proposed outcome

- A problem with backup credentials, permissions or S3 reachability never affects Kanidm pod
  readiness. The sidecar keeps running and retries, and its logs say why.
- Kaniop never renders a backup schedule that Kanidm will refuse, or a local retention under
  which nothing can be uploaded. It rejects such values at admission. A Schedule admitted before
  this release reports `Ready=False` with a clear reason, and Kaniop leaves its configuration
  out of Kanidm.

## Affected users and systems

- Every Kanidm cluster with a `KanidmBackupSchedule`. The risk is highest with one replica.
- Components: data-mover `transport`, the admission webhook, the schedule controller, the Kanidm
  StatefulSet renderer and the `KanidmBackupSchedule` CRD.

## Constraints and non-goals

- Patch release: no new CRD fields, and no change to the transport upload protocol.
- Do not touch the restore Job code (`libs/operator/src/kanidm/restore/`); restore outages B3 and
  B4 are handled separately.
- Not in scope: surfacing sidecar failures in Schedule status or metrics (B17), timeZone and
  jitter handling (B16), minAge validation (B14), and re-uploads after a sidecar restart (B15).

## Success measures

- With a repository whose credentials S3 rejects, the primary pod stays Ready, and the sidecar has
  no restarts and logs that it is retrying (e2e
  `backup_transport_invalid_credentials_keep_primary_ready`).
- `0 0 31 2 *` and `localVersions: 1` are rejected at admission (e2e). Objects that already
  exist are reported in status and never rendered (unit tests).

## Open questions

- None blocking. Making sidecar failures visible in status is follow-up B17.
