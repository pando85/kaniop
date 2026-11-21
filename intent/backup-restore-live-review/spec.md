# Specification: Live review of every Kaniop backup and restore path

Derived from: [intent.md](intent.md) in the same change. This is a test campaign
specification: it defines what is tested, under which guardrails, and what counts as a pass.
It records no results.

## Environment

- Cluster: homelab `grigri`, k3s 1.35, 3 nodes, node-local ZFS via openebs-zfspv. Only the
  kubectl context `grigri` is used.
- GitOps: the homelab GitOps repo, Argo CD auto-sync (prune and selfHeal) from `master`.
  A Kaniop upgrade is a chart version bump, commit, and push. The chart hook `kaniop-crd-apply`
  applies CRDs; no manual CRD apply.
- Production Kanidm `kanidm/kanidm`: `kanidm/server:1.11.2`, 2 replicas (`kanidm-default-0` on
  node `grigri`, `kanidm-default-1` on node `prusik`), PVC 2Gi openebs-zfspv (`Retain`),
  namespace selectors `{}` (reads identity CRs from every namespace), external Gateway.
- Production backups: `KanidmBackupRepository` `kanidm` pointing at the homelab MinIO, bucket
  `kaniop`, prefix `kanidm`, no encryption. `KanidmBackupSchedule` daily at 03:00 UTC.
  16 `KanidmBackup` objects at planning time.
- The homelab restore template `system/kanidm/templates/kanidm-restore.yaml` (in the homelab
  GitOps repo) supports only `source.local` and has no `safetyBackup`. It must be extended
  before a production restore.

## Static-review findings to confirm live

| # | Suspected defect | Evidence |
|---|---|---|
| F1 | Local restore reads `/data/<fileName>`, but scheduled backups are written to `/data/backups`. | `libs/operator/src/kanidm/restore/legacy.rs:59` (`BACKUP_PATH`), `libs/operator/src/kanidm/reconcile/statefulset.rs:51` (`online_backup.path`) |
| F2 | `timeZone`, `jitterSeconds`, `maxUploadBytesPerSecond`, `maxDownloadBytesPerSecond`, `maxConcurrentParts` are accepted but unused. | static review |
| F3 | `localVersions: 1` is accepted by the CRD but rejected by the controller. | `libs/backup/src/controller/schedule.rs:336` |
| F4 | Docs say encryption can be added later; the webhook rejects that once the repository is in use. | docs vs webhook |
| F5 | Operator Jobs do not inherit `nodeSelector`, `tolerations`, `imagePullSecrets`; an unschedulable Job keeps the cluster at 0 replicas. | static review |
| F6 | Local restore skips the backup version-compatibility check (remote only). | `libs/operator/src/kanidm/restore/legacy.rs:2868` (`validate_backup_compatibility`) |
| F7 | Repository `Ready` validates spec only (intentional). Check where bad credentials surface. | static review |

## Escaped defect: SIGILL in 0.23.0 release binaries

Found during Phase 0; blocks the campaign on `grigri`.

- kaniop 0.23.0 release binaries die with SIGILL (exit 132) on CPUs without AVX (node `grigri`,
  Intel Atom C2000).
- Cause: AWS-LC C code is compiled with AVX by the floating release build environment
  (`ubuntu-latest` plus Rust stable: GCC 15.2 and rustc 1.100, against GCC 13.3 and rustc 1.99
  for 0.21.1).
- CRDs are not affected: all 10 match 0.23.0 and are `Established`.
- Required fix (separate PR): baseline CPU target for C and Rust, pinned runner and toolchain,
  a CI check that runs every shipped binary under `qemu-x86_64 -cpu Westmere`, release 0.23.1,
  and an eval case under `evals/cases/`.

## Guardrails

- Never write with kubectl in namespace `kanidm`. Production changes only through the homelab
  GitOps repo, with owner approval for each push.
- Never point a test repository at the production bucket. The `KanidmBackup` finalizer deletes
  S3 data, and webhook prefix uniqueness is checked per namespace only.
- A pre-apply check rejects any test manifest that references `namespace: kanidm` or a
  `kanidmRef` named `kanidm`. The test Kanidm is never named `kanidm`.
- Never run the repository e2e harness against `grigri`; it restarts the production operator.
- All test objects live in namespace `default` and carry the label `kaniop-review=backup`.
  Alert noise from `default` is accepted (or silenced for the campaign).
- Production data used for disaster-recovery rehearsal is read-only: credentials are read from
  the Secret, never printed, and never copied into `default`.

## Test environment (namespace `default`)

### `minio-test`

- Image `pgsty/silo:RELEASE.2026-10-26T00-00-00Z` (same as homelab production and
  `tests/e2e/scripts/setup-minio.sh`; includes `mc`).
- Names prefixed `minio-test-` (the e2e cleanup script deletes `minio*` names).
- StatefulSet, 1 replica, PVC on `fast-ci-zfspv` (about 10Gi),
  `server /data --certs-dir /certs --console-address :9001`.
- KMS: static `MINIO_KMS_SECRET_KEY=kaniop-test-key:<base64 32 bytes>` from a Secret. SSE-S3
  (AES256) and SSE-KMS with key id `kaniop-test-key`; any other key id is the negative test.
  Do not enable `MINIO_KMS_AUTO_ENCRYPTION` or `mc encrypt set`: they would hide whether Kaniop
  sends the encryption headers. Verify with `mc stat` (`X-Amz-Server-Side-Encryption`) and
  `mc admin kms key status`.
- TLS: cert-manager `Certificate` `minio-test-tls` from ClusterIssuer `internal`, DNS names
  short, `.default.svc`, and `.svc.cluster.local`. Mount `tls.crt` as `public.crt`, `tls.key`
  as `private.key`, `ca.crt` as `CAs/ca.crt`. CA ConfigMap `minio-test-ca` (key `ca.crt`) in the
  repository's namespace for `caBundleRef`. `caBundleRef` sets `SSL_CERT_FILE`, which replaces
  the system roots.
- Repository settings: `forcePathStyle: true`, `region: us-east-1`.
- Buckets: `kaniop-test`, and `kaniop-test-lock` created with Object Lock (default GOVERNANCE,
  1 day). Never COMPLIANCE.
- Users and policies (no `BypassGovernanceRetention`):
  - writer: `GetBucketLocation`; `PutObject`, `GetObject`, `AbortMultipartUpload`,
    `ListMultipartUploadParts`
  - reader: `ListBucket`, `GetBucketLocation`; `GetObject`
  - deleter: `ListBucket`, `GetBucketLocation`; `GetObject`, `GetObjectVersion`,
    `DeleteObject`, `DeleteObjectVersion`
- Known risks: rust-s3 bulk delete without `Content-MD5` (confirm on the lock bucket); static-key
  KMS support in Silo is unverified.

### `minio-test-http`

Second small instance, no certificates, `emptyDir`, for `insecure: true`.

### `kanidm-test`

- `kanidm/server:1.11.2`, 2 replicas, explicit `storageClassName: fast-ci-zfspv`, explicit
  `primaryNode`, no Gateway, namespace selectors limited to `default`, `securityContext` 389,
  `KANIDM_DB_FS_TYPE=zfs`.
- Seed data through CRs (`kanidmRef: kanidm-test`) and through the CLI. Restore is proven on the
  CLI-created (non-CR) data; CR objects only prove convergence.
- The first sidecar injection restarts the pods. Budget about 10 to 15 minutes per backup.

### Disaster-recovery target

Same domain as production Kanidm, `kanidm/server:1.11.2`, internal or self-signed certificate,
no Gateway (avoids external-dns changes). Production backups are copied read-only into
`minio-test` and catalogued and restored only there.

## Option matrix

Every row: backup, mutate, restore, verify. Each row has its own repository and prefix. Only
one schedule per Kanidm at a time. `minAge` and `keepLast` are immutable, so set them (small
`minAge`) at creation.

| Row | Option | Acceptance criteria |
|---|---|---|
| M1 | No encryption, HTTPS with `caBundleRef` | Backup uploaded over TLS trusted only through the CA ConfigMap; restore returns CLI-created data. |
| M2 | `clientSide` encryption with KEK | Object content is ciphertext; restore with the same KEK returns the data. |
| M3 | `clientSide` with custom `keyRef.key` | Same as M2 using a non-default Secret key (0.21.1 required the key `encryption-key`). |
| M4 | `providerManaged` (SSE-S3) | `mc stat` shows `X-Amz-Server-Side-Encryption: AES256` on every object; restore succeeds. |
| M5 | `providerKms` with `keyId` | `mc stat` shows SSE-KMS with `kaniop-test-key`; a wrong `keyId` fails with a visible condition, not a silent loop. |
| M6 | Split writer, reader, deleter credentials | Each operation uses the least-privilege user; no operation needs more than its policy grants. |
| M7 | `insecure: true` over plain HTTP (`minio-test-http`) | Backup and restore succeed over HTTP. |
| M8 | Object Lock bucket plus retention delete | Retention delete of a locked object is deferred and visible (condition, metric, alert), then succeeds after the lock expires. |
| R1 | Local restore by `fileName` | Restore reads the file the schedule actually wrote (confirms or refutes F1). |
| R2 | Cross-UID disaster recovery | A backup from one Kanidm restores into a different Kanidm and data matches. |
| R3 | Safety backup skip (break-glass) | Restore proceeds without a safety backup only when explicitly requested. |
| R4 | Restore 1 replica to 3 replicas | All replicas converge and replicate after restore. |
| - | `workloadIdentity` | Static review only. |

## Suspected-defect and fault cases (Phase 4)

| Case | Acceptance criteria |
|---|---|
| Bad credentials | Record where the failure surfaces (repository, schedule conditions, `KaniopBackupStale`) (F7). |
| `localVersions: 1` with a 1 to 2 minute cron | Record CRD vs controller behavior and interaction with the 120 s upload delay (F3). |
| 6-field cron | Record whether accepted, rejected, or misinterpreted. |
| `timeZone` and `jitterSeconds` | Confirm ignored (F2); static plus one cheap run. |
| Missing safety repository | Restore fails early with a clear condition, no deadlock. |
| Cross-version local restore | Record whether the version check runs for local restores (F6). |
| Node drain during upload | Upload fails or resumes cleanly; no stuck Job or lock. |
| `minio-test` restart mid-upload | Same as node drain. |
| PV leak on `Retain` class | Record whether operator Jobs leave PVs behind. |
| Unschedulable Job | Record whether the cluster stays at 0 replicas (F5). |
| Alert firing | Backup alerts fire for the induced failures. |
| Retention pruning, suspend toggle, delete schedule or repository (last) | Pruning matches policy; suspend stops runs; deletes leave no orphans, or orphans are recorded. |

## Production restore (Phase 1) acceptance criteria

- Homelab restore template extended with `backupRef` and `safetyBackup.repositoryRef`.
- Restore from the latest daily backup. Manual changes since 03:00 UTC are accepted as lost;
  CR-managed objects reconcile back.
- After restore: both replicas running, replication healthy, logins work, OAuth2 apps work,
  admin password rotated, safety backup uploaded.
- Restore disabled again in git.

## Security and privacy

- Production S3 credentials are never printed, logged, or copied into `default`.
- Test MinIO keys and users are throwaway and live only in `default`.
- No COMPLIANCE-mode Object Lock (cannot be removed).

## Concerns and unresolved questions

- Operator-created objects do not carry the `kaniop-review=backup` label; cleanup must find them
  by owner and name.
- rust-s3 bulk delete without `Content-MD5` may fail on the Object Lock bucket.
