# Task

A live review of Kaniop v0.24.1 backups found two Schedule values that Kaniop accepts but that
break backups.

1. A `KanidmBackupSchedule` with `schedule: "0 0 31 2 *"` is admitted by the webhook, and the
   schedule controller reports `Ready=True Configured`. Kanidm 1.11.2 then refuses to start the
   primary:

   ```text
   Online backup schedule error: '0 0 0 31 2 * *' will not match any date.
   Failed to start server core!
   ```

   The primary crashloops. `schedule` is immutable, so the only recovery is to delete the
   Schedule. Kaniop's validator also disagrees with Kanidm about field counts: Kaniop retries
   any expression with a `0 ` prefix.

2. A Schedule with `localVersions: 1` is admitted by the CRD and the webhook. The schedule
   controller logs `localVersions must be at least 2` and returns an error before it writes any
   status, so there is no condition and no event. The Kanidm StatefulSet is still rendered with
   `KANIOP_BACKUP_VERSIONS=1`. Kanidm then deletes each backup file at the next tick, before the
   transport sidecar's minimum file age, and nothing is ever uploaded.

Make Kaniop validate the backup schedule exactly as Kanidm will interpret it, and reject
`localVersions` below 2. Apply the same rules at admission, in the schedule controller's status,
and when the operator renders the Kanidm StatefulSet. Objects admitted by an older version must
stop reaching Kanidm. Add regression tests, regenerate generated artifacts, and update the user
documentation.
