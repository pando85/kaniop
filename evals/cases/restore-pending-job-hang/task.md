# Task

During a restore drill on kaniop 0.24.1, two `KanidmRestore` objects left the identity service
in a bad state:

1. The safety backup repository uses client-side encryption, but its KEK Secret had been
   deleted. The restore passed `Validating`, scaled Kanidm to zero replicas, and the safety
   backup Job pod then sat in `CreateContainerConfigError: secret "<kek>" not found`. The
   restore stayed in `SafetyBackup` with Kanidm at 0 replicas until someone deleted it.
2. A restore whose `safetyBackup.repositoryRef` named a repository that does not exist stayed in
   `Validating` forever with an empty `status.message`, logged a 404 every 5 seconds, and made
   every other restore of the same Kanidm fail with "another active restore targets this
   Kanidm".

Reviewing the code also showed that restore Jobs ignore the primary replica group's
`nodeSelector`, `tolerations` and node affinity, have no `activeDeadlineSeconds`, and that the
controller only looks at the Job's `failed`/`succeeded` counters. A Job pod that is never
scheduled therefore hangs the restore forever; once `databaseMutationStarted` is true there is no
way out, because `restore.kaniop.rs/force-release` only works on a `Failed` restore.

Fix the restore controller for a 0.24.2 patch release so that a restore Job that cannot start or
finish ends the restore in a terminal state within a bounded time, restore Jobs can be scheduled
wherever the primary Kanidm pod can, and a missing or unusable repository or KEK fails the
restore before the target is scaled down. No CRD change. Add regression tests that fail on the
0.24.1 code.
