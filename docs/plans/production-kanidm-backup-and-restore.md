# Production Kanidm backup and restore implementation plan

Implements [ADR-0001: Production Kanidm backup and restore orchestration](../adr/0001-production-kanidm-backup-and-restore.md).

Related issue: [#434](https://github.com/pando85/kaniop/issues/434)

## Goal

Implement the ADR in dependency order and deliver a production-supported
restore-first path before enabling scheduled online transport.

The production milestone must prove that, given a committed compatible backup
in S3-compatible storage, Kaniop can:

1. validate the source and exact Kanidm image before downtime;
2. quiesce the complete Kanidm topology;
3. create, upload and verify an offline safety backup of the current primary;
4. download and verify the selected restore source;
5. cross the persisted database mutation boundary only after both artifacts are
   safe;
6. restore and verify the database offline;
7. start one authoritative primary and rebuild secondaries;
8. recover deterministically after operator restarts and injected failures.

Scheduled online upload is a later experimental phase. It becomes
production-supported only when a documented Kanidm completion contract exists.

## Non-goals

The plan does not implement:

- incremental backup or point-in-time recovery;
- copying an active SQLite database;
- CSI snapshots or blue/green restore;
- cross-namespace restore;
- automatic cross-cluster restore;
- GCS, Azure Blob or NFS repositories;
- client-side envelope encryption;
- automated periodic deep restore testing;
- a `kubectl kaniop` plugin.

## Current implementation baseline

The current tree already contains:

- `Kanidm.spec.backup` with schedule and local versions;
- primary-only rendering of `[online_backup]` into Kanidm configuration;
- local backup storage under `/data/backups`;
- the `KanidmRestore` CRD and controller;
- target UID, basename and pinned-image validation;
- restore maintenance annotation and identity reconciler write gates;
- quiesce, Pod termination and VolumeAttachment waiting;
- restore and `database verify` Jobs;
- persisted `database_mutation_started` state;
- fail-closed finalizer behavior;
- primary-first replicated recovery;
- basic restore metrics and unit tests.

Known baseline gaps are the first work package, not prerequisites to ignore.

## Delivery phases

| Phase | Outcome | Production gate |
|---|---|---|
| 0. Baseline correction | Generated APIs, local restore E2E and operational alerts are trustworthy | Required |
| 1. Contracts | Repository, Schedule, Backup, extended Restore and manifest v1 compile and install | Required |
| 2. S3 data mover | Secure, idempotent payload/manifest transfer works against MinIO | Required |
| 3. Catalog | Reader discovery reconstructs `KanidmBackup` resources | Required |
| 4. Safety backup | Quiesced offline backup commits remotely before mutation | Required |
| 5. Remote restore | Remote download, restore, verify and primary-first rebuild work | Required |
| 6. Retention and GC | Deterministic deletion uses a separate deleter identity | Required |
| 7. Hardening | Failure matrix, AWS S3, security review and runbooks pass | GA restore-first |
| 8. Online transport | Schedule and sidecar work behind an experimental feature gate | Experimental |
| 9. Online support | Upstream completion contract is enforced and tested | Future production gate |

## API contracts

All new resources are namespaced and start at `kaniop.rs/v1alpha1`. The existing
`KanidmRestore` remains at its current served version and gains backward-
compatible fields. If a breaking Restore schema change is unavoidable, add a
new served version and conversion rather than introducing a lower API version.

### `KanidmBackupRepository`

Purpose: declare one S3-compatible repository, credentials by logical role,
server-side encryption and transport limits.

```yaml
apiVersion: kaniop.rs/v1alpha1
kind: KanidmBackupRepository
metadata:
  name: offsite
  namespace: identity-prod
spec:
  s3:
    bucket: corp-kaniop-backups
    prefix: prod
    region: eu-west-1
    endpoint: https://s3.eu-west-1.amazonaws.com
    forcePathStyle: false
    caBundleRef: null
  authentication:
    writer:
      workloadIdentity: {}
    reader:
      workloadIdentity: {}
    deleter:
      workloadIdentity: {}
  encryption:
    mode: providerKms
    keyId: alias/kaniop-backups
  limits:
    maxUploadBytesPerSecond: 52428800
    maxDownloadBytesPerSecond: 104857600
    maxConcurrentParts: 4
    safetyBackupMinRetention: 720h
status:
  observedGeneration: 1
  lastProbeTime: null
  capabilities: null
  conditions: []
```

Validation:

- bucket and prefix are non-empty and normalized;
- endpoint is HTTPS when supplied;
- no `insecureSkipVerify` field exists;
- custom trust uses a same-namespace ConfigMap `caBundleRef`;
- exactly one supported authentication mode is selected for each role;
- credential values cannot be inline;
- Secret and ConfigMap references are same-namespace;
- transfer limits and part counts are bounded;
- `safetyBackupMinRetention` has a safe minimum;
- bucket, prefix and endpoint become immutable after first use;
- credential references and transport limits remain mutable for rotation and
  tuning;
- `providerKms` explicitly means provider-side SSE-KMS, not client-side
  encryption.

Prefix ownership:

- one Repository owns each `(endpoint, bucket, prefix)` tuple;
- the webhook rejects duplicates visible in the namespace;
- documentation states that reuse from another namespace, cluster or credential
  set is unsupported and cannot be fully detected by Kubernetes admission;
- the capability probe writes and reads only a random test key under a reserved
  probe prefix and removes it using the appropriate identity.

Conditions:

- `Ready`;
- `CredentialsValid`;
- `CapabilitiesVerified`;
- `EncryptionConfigured`.

### `KanidmBackupSchedule`

Purpose: own the single Kanidm online backup cron, local versions and remote
retention policy.

```yaml
apiVersion: kaniop.rs/v1alpha1
kind: KanidmBackupSchedule
metadata:
  name: corp-idm-standard
  namespace: identity-prod
spec:
  kanidmRef:
    name: corp-idm
  repositoryRef:
    name: offsite
  schedule: "3 */6 * * *"
  timeZone: UTC
  suspend: false
  concurrencyPolicy: Forbid
  jitterSeconds: 300
  localVersions: 7
  retention:
    keepLast: 8
    daily: 7
    weekly: 4
    monthly: 12
    minAge: 24h
status:
  observedGeneration: 1
  lastDiscoveredBackupRef: null
  lastSuccessfulBackupTime: null
  conditions: []
```

Validation and mutability:

- one active Schedule per `Kanidm` in a namespace;
- admission uses a webhook for cross-resource uniqueness because CEL cannot list
  other resources;
- cron and timezone are parsed at admission and reconciliation;
- `kanidmRef`, `repositoryRef`, schedule and retention policy become immutable
  after the first committed backup;
- `suspend` remains mutable;
- automated restore suspension is represented by a controller-owned status
  Condition or target maintenance state, not by mutating user intent in
  `spec.suspend`;
- `localVersions` must be at least two and documentation explains that it must
  cover upload/retry latency;
- `concurrencyPolicy` is initially `Forbid`; do not expose `Allow` or `Replace`
  without executable semantics.

Conditions:

- `Ready`;
- `Suspended`;
- `RestoreInProgress`;
- `UpstreamCompletionContractAvailable`;
- `LastBackupSucceeded`.

Before online transport is supported, a Schedule is either configuration-only
or experimental. It must not claim a committed remote backup merely because the
Kanidm stanza was rendered.

### `KanidmBackup`

Purpose: immutable evidence and catalog metadata for one committed manifest.
It is not a generic online "backup now" request.

```yaml
apiVersion: kaniop.rs/v1alpha1
kind: KanidmBackup
metadata:
  name: corp-idm-019c7c76
  namespace: identity-prod
spec:
  backupId: 019c7c76-f423-7a12-8f41-2bea7588a303
  kanidmRef:
    name: corp-idm
    uid: 9e630aed-3a61-4418-b711-e6030fb67b51
  repositoryRef:
    name: offsite
  manifestKey: v1/tenants/a81c/clusters/9e630aed/backups/019c7c76/manifest.json
status:
  phase: Ready
  consistency: kanidm-offline
  reason: restore-safety
  kanidmVersion: 1.10.4
  imageDigest: sha256:abc
  sizeBytes: 18432791
  payloadSha256: 9c8e...
  createdAt: "2026-08-18T02:03:41Z"
  conditions: []
```

Rules:

- `spec` is immutable;
- logical identity is `(repository UID, backupId)`;
- `backupId` and `manifestKey` are unique within a Repository;
- controller-created labels include Kanidm UID, Repository UID, consistency and
  reason without exposing sensitive metadata;
- deletion of the CR does not delete S3 objects;
- ownerReferences to Schedule or Restore are forbidden because catalog deletion
  must not cascade to durable data;
- a finalizer is used only when needed to serialize explicit GC state, never to
  turn ordinary CR deletion into implicit remote deletion.

Discovered backup phases:

```text
Discovering -> Ready -> Deleting -> Deleted
                   \-> Invalid
```

Orchestrated safety backup phases:

```text
Pending -> Generating -> Uploading -> Committing -> Verifying -> Ready
                                                            \-> Failed
```

Conditions:

- `Generated`;
- `Uploaded`;
- `Committed`;
- `IntegrityVerified`;
- `DeletionDeferred`.

### `KanidmRestore`

Extend the existing source union and status without removing local restore
compatibility immediately:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmRestore
metadata:
  name: restore-20260818
  namespace: identity-prod
spec:
  targetRef:
    name: corp-idm
    uid: 9e630aed-3a61-4418-b711-e6030fb67b51
  source:
    backupRef:
      name: corp-idm-019c7c76
  restoreImage: kanidm/server@sha256:abc
  safetyBackup:
    repositoryRef:
      name: offsite
    skip: false
status:
  phase: Completed
  safetyBackupRef: corp-idm-safety-019c7d01
  databaseMutationStarted: true
  conditions: []
```

Rules:

- `spec` remains immutable;
- `source.local.fileName` remains available during migration;
- remote production restores require `source.backupRef`;
- target, source and Repository are same-namespace;
- target UID and manifest Kanidm UID must match;
- domain must match;
- restore image must be immutable and match the manifest's Kanidm version and
  approved digest;
- safety backup defaults to required;
- `skip=true` requires non-empty break-glass reason and approver annotations;
- admission validates shape, while repository reads and image availability stay
  in controller preflight.

Additional status:

- original replica counts;
- names of safety, restore and verify Jobs;
- safety backup ID/ref and manifest key;
- source staging PVC/volume information;
- per-phase timestamps;
- `databaseMutationStarted`;
- observed target UID and generation.

Do not store an unbounded audit log in status.

## Manifest v1

### Object layout

```text
v1/
  tenants/<namespace-uid>/
    clusters/<kanidm-uid>/
      backups/<backup-id>/
        payload/kanidm.backup.json[.gz]
        manifest.json
      staging/<backup-id>/
        multipart-state
```

### Required schema

```json
{
  "apiVersion": "backup.kaniop.rs/v1alpha1",
  "kind": "KanidmBackupManifest",
  "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
  "createdAt": "2026-08-18T02:03:41Z",
  "source": {
    "namespaceUid": "a81c...",
    "kanidmName": "corp-idm",
    "kanidmUid": "9e630aed-3a61-4418-b711-e6030fb67b51",
    "domain": "idm.example.es",
    "kanidmVersion": "1.10.4",
    "imageDigest": "sha256:abc"
  },
  "backup": {
    "mode": "full",
    "consistency": "kanidm-offline",
    "reason": "restore-safety"
  },
  "payload": {
    "key": "v1/tenants/a81c/clusters/9e630aed/backups/019c7c76/payload/kanidm.backup.json.gz",
    "sizeBytes": 18432791,
    "sha256": "9c8e..."
  },
  "encryption": {
    "transport": "tls",
    "atRest": "provider-kms",
    "keyId": "alias/kaniop-backups"
  },
  "compatibility": {
    "sameKanidmVersionRequired": true,
    "minimumManifestReader": "0.13.0"
  }
}
```

Implementation rules:

- define manifest types in a library shared by operator and data mover;
- deny unknown fields where safe for the alpha schema, but preserve an explicit
  compatibility strategy before beta;
- bound string lengths, object sizes and collection lengths before allocation;
- validate object keys remain under the Repository/Kanidm/backup prefix;
- never deserialize or rewrite the Kanidm payload;
- canonical serialization is required if manifest signing is added later.

### Version compatibility

- the operator lists every manifest API version it supports;
- newer operators continue reading declared older versions;
- an unknown newer version becomes `Invalid` with
  `ManifestVersionTooNew`, never `Ready`;
- discovery does not rewrite old manifests;
- additive optional fields stay within a version;
- breaking schema changes add another manifest API version;
- `minimumManifestReader` is advisory and does not replace parser support.

### Commit protocol

1. Generate a random immutable backup ID.
2. Write the complete local backup to staging.
3. Calculate local SHA-256 and size.
4. Start or resume multipart upload to the immutable payload key.
5. Complete the multipart upload or abort it on terminal failure.
6. Verify payload size and available provider checksum metadata by exact key.
7. Upload the immutable manifest with conditional create semantics.
8. Read or `HEAD` the manifest by exact key.
9. Return success only after step 8.

Invariants:

- no manifest means no committed backup;
- retries use the same backup ID;
- an existing different object at either key is a hard conflict;
- no S3 rename/copy-delete is used as a commit primitive;
- object listing is not part of commit confirmation;
- incomplete multipart uploads are aborted and also covered by bucket lifecycle
  as defense in depth.

## Component design

### Shared backup library

Add a workspace library for:

- manifest schema and version dispatch;
- repository path construction and confinement;
- checksum streaming;
- S3 configuration and capability model;
- transfer result/error types;
- retention selection shared by controller tests and GC planning.

Keep cloud SDK dependencies out of the core operator libraries where practical.

### Data mover binary

Add one minimal binary image with subcommands such as:

```text
kaniop-data-mover upload
kaniop-data-mover download
kaniop-data-mover probe
kaniop-data-mover delete-plan
kaniop-data-mover watch-online   # experimental
```

The binary:

- has no Kubernetes client;
- receives a bounded operation document through a projected ConfigMap or file;
- supports workload identity and Secret-mounted credentials;
- streams payloads instead of loading them into memory;
- implements multipart limits, retries, timeouts and bandwidth throttling;
- emits structured logs without secrets or signed URLs;
- writes a small result document to a shared termination/result volume;
- uses distinct exit codes for retryable, invalid-input, integrity and
  authorization failures.

Do not make the operator parse human log lines as protocol. Result documents
have a versioned schema and bounded size.

### Repository controller

Responsibilities:

- validate referenced Secrets/ConfigMaps exist without reading secrets into
  status;
- run a probe Job using the relevant role identities;
- record supported capabilities and probe timestamp;
- re-probe after credential reference or endpoint generation changes;
- use backoff on transient failures;
- never silently fall back to a weaker credential or encryption mode.

Probe operations must not require bucket-wide list or delete permissions. Use a
reserved random key under the configured prefix and clean it with the deleter
identity.

### Catalog discovery controller

Responsibilities:

- run one discovery loop per ready Repository under leader election;
- list only keys ending in `/manifest.json` below the Repository prefix;
- bound each scan and paginate;
- fetch and validate candidate manifests;
- verify payload existence/metadata by exact key;
- create/reconcile `KanidmBackup` deterministically;
- preserve invalid diagnostics without repeatedly emitting Events;
- expose scan age, duration and failure metrics.

For large repositories, persist a continuation cursor or shard discovery by
Kanidm UID. Do not list payloads.

### Safety backup Job

The Restore controller performs quiesce. The Job:

- mounts only the primary PVC and required config;
- uses the exact target Kanidm image for the backup command;
- writes to an isolated staging volume with an explicit size limit;
- runs `kanidmd database backup` while no Kanidm server mounts the database;
- streams the resulting file through the data mover;
- commits the manifest and writes a result document;
- has `automountServiceAccountToken: false` and `backoffLimit: 0`;
- has deterministic naming from Restore UID and attempt generation.

If combining the Kanidm command and uploader in one Pod requires separate
containers, coordinate them through files and completion markers created by the
offline Job itself. This marker is trustworthy because Kaniop controls both
processes and the server is stopped.

### Restore source preparation Job

Before the mutation boundary:

- download manifest and payload to staging;
- verify exact key confinement, size and SHA-256;
- verify manifest identity and version again in the Job;
- make the verified payload available to the restore Job without another remote
  dependency;
- fail without touching the database PVC.

Prefer a separate staging PVC sized from manifest metadata for large backups.
`emptyDir` may be allowed only below a documented size threshold.

### Restore and verify Jobs

Preserve the existing security and deterministic behavior. Extend the Job input
to use the verified staged payload.

The restore Job is the first component allowed to mutate the primary PVC. The
controller persists `databaseMutationStarted=true` before creating it.

The verify Job runs after restore and before any Kanidm server starts.

### GC controller and Job

The controller computes a deletion plan from catalog state. The plan contains
only exact manifest, payload and staging keys.

The deleter Job:

- receives the immutable plan;
- has only delete/head permissions;
- deletes manifest first;
- deletes payload and staging afterward;
- returns per-key results;
- does not list or make retention decisions;
- has no Kubernetes token.

## Restore reconciliation

### State machine

```text
Pending
  -> Validating
  -> Quiescing
  -> SafetyBackup
  -> PreparingSource
  -> RestoringPrimary
  -> Verifying
  -> RebuildingReplicas
  -> Resuming
  -> Completed
```

Every phase is idempotent and derived from persisted status plus deterministic
child resources.

### Validating

Before downtime:

- target exists and UID matches;
- no other active Restore targets the same Kanidm;
- target uses persistent PVC storage and has exactly one primary group;
- source Backup is `Ready` and not deleting;
- Repository is `Ready`;
- manifest parses and matches source Backup status;
- namespace UID, Kanidm UID and domain match policy;
- payload exists and metadata is consistent;
- restore image is immutable, available and compatible;
- staging capacity can hold the source and safety backup;
- required Secrets, service accounts and CAs exist;
- original replica counts are persisted.

Do not quiesce if any preflight check fails.

### Quiescing

- set the existing restore/maintenance annotation;
- expose `RestoreInProgress` on the Schedule without mutating user
  `spec.suspend`;
- stop new experimental online work;
- if an online upload is already in flight, wait for it to finish up to a
  bounded timeout;
- do not promise sidecar cancellation because the sidecar has no Kubernetes API
  control channel;
- after timeout, proceed only if the local file is no longer being generated;
  an incomplete remote upload remains uncommitted and is cleaned as staging;
- scale all groups to zero;
- wait for Pods to terminate and VolumeAttachments to detach.

### SafetyBackup

- create a `KanidmBackup` with reason `restore-safety` and deterministic backup
  ID recorded in Restore status;
- create the offline safety backup Job;
- reconcile Job and result document;
- read the committed manifest directly by exact key using reader credentials;
- verify payload metadata/checksum evidence;
- mark the Backup `Ready` and persist `safetyBackupRef`;
- on failure, delete transient Jobs/staging, restore original replica counts,
  remove maintenance and fail without mutation.

Break-glass skips only this phase and records the required Condition, Event,
metric and structured log.

### PreparingSource

- create the source preparation Job;
- download to staging;
- verify checksum and compatibility;
- persist staging identity and successful result;
- do not mount the primary database PVC read-write;
- on failure, resume the original service because mutation has not started.

### Mutation boundary and RestoringPrimary

Immediately before creating the restore Job:

- verify target remains quiesced;
- verify safety backup is Ready or authorized break-glass is persisted;
- verify source preparation result remains valid;
- patch `databaseMutationStarted=true` and confirm status write;
- create the deterministic restore Job.

If the status patch cannot be confirmed, do not create the Job.

A restore Job failure leaves Kanidm offline and keeps finalizer/maintenance
state. Automatic rollback to the safety backup is not attempted in the MVP.

### Verifying

Run `kanidmd database verify` offline. Failure leaves the target offline and
requires explicit recovery.

### RebuildingReplicas

- delete/reprovision stale secondary PVCs using the existing validated topology
  rules;
- start exactly one primary replica;
- wait for readiness and execute semantic smoke checks;
- start/rebuild secondaries in controlled order;
- wait for replication/topology readiness;
- never allow an old secondary volume to rejoin unchanged.

### Resuming

- restore desired replica counts;
- wait for availability;
- remove maintenance annotation;
- clear controller-owned Schedule suspension Condition;
- clean transient Jobs/config/staging according to retention;
- mark Completed and publish outcome/duration metrics.

### Deletion and finalizers

Before mutation, deleting a Restore cancels transient work and returns the
original service to its prior state.

After mutation, finalizer removal is refused until the database is verified and
service recovery is resolved. Document an explicit administrator recovery
procedure; do not add an unaudited force path.

## Break-glass implementation

Required fields:

```yaml
spec:
  safetyBackup:
    skip: true
metadata:
  annotations:
    backup.kaniop.rs/break-glass-reason: "The current PVC is unreadable"
    backup.kaniop.rs/break-glass-approved-by: "incident-commander@example.com"
```

Controls:

- CEL validates annotations when `skip=true` where metadata access is supported;
- validating webhook performs the complete check;
- document a dedicated ClusterRole for users allowed to create break-glass
  Restores or an admission policy bound to an authorized group;
- emit `SafetyBackupSkipped` Warning Event;
- set Condition reason `BreakGlassOverride`;
- increment `kaniop_restore_break_glass_total`;
- rely on Kubernetes audit for actor identity;
- never accept a generic `force` field.

## Retention and garbage collection

### Policy ownership

- Schedule owns retention of scheduled backups for its Kanidm/Repository pair;
- Repository owns `safetyBackupMinRetention` and any hard lower bounds;
- active Restore references protect their source and safety backup;
- provider Object Lock can extend but never shorten Kaniop retention.

### Selection algorithm

1. Select only committed, `Ready` backups for one Kanidm/Repository pair.
2. Exclude backups referenced by active Restores.
3. Exclude all backups younger than `minAge`.
4. Exclude safety backups younger than Repository safety retention.
5. Retain the newest `keepLast`.
6. Retain one deterministic representative per daily, weekly and monthly
   bucket using UTC boundaries for v1.
7. Mark every remaining backup as a deletion candidate.
8. Re-evaluate immediately before creating the delete plan.

Use the union of retention sets. A backup is deleted only when selected by none.

### Deletion protocol

1. Patch Backup phase to `Deleting` with a deletion plan ID.
2. Create a deleter Job with exact immutable keys.
3. Delete `manifest.json` first.
4. Delete payload and known staging objects.
5. On complete success, mark `Deleted` or remove the catalog after a bounded
   history period.
6. On Object Lock/retention denial before manifest deletion, set
   `DeletionDeferred` and leave the backup Ready.
7. If manifest deletion succeeds but payload deletion fails, keep a tombstone
   status and retry orphan cleanup; never recreate the manifest automatically.

A dry-run planner and property tests must prove protected backups are never
selected.

## Security implementation

### IAM capabilities

| Identity | Required operations | Forbidden operations |
|---|---|---|
| Writer | multipart create/upload/complete/abort, conditional PUT, HEAD own key | list broad prefix, overwrite, delete, read other payloads |
| Reader | LIST manifests, GET/HEAD manifest and payload | PUT, DELETE |
| Deleter | DELETE/HEAD exact planned keys | PUT, broad LIST, manifest creation |
| Restore reader | GET/HEAD selected manifest and payload | PUT, DELETE |

Provider limitations may require a slightly broader policy; document the exact
AWS and MinIO policies used in tests.

### Pod hardening

All mover and Job containers use, where compatible:

```yaml
securityContext:
  allowPrivilegeEscalation: false
  capabilities:
    drop: ["ALL"]
  readOnlyRootFilesystem: true
  runAsNonRoot: true
  seccompProfile:
    type: RuntimeDefault
```

Add explicit CPU, memory and ephemeral-storage requests/limits. Mount credentials
only into the container that needs them. Acknowledge that containers in one Pod
are not a hard security boundary.

### Secrets and logs

- never copy secret values into CR status, Events or operation documents;
- never log authorization headers, session tokens, signed URLs or payload data;
- redact SDK error fields known to include endpoints/query strings;
- test logs and Events for known fixture secrets;
- prevent debug mode from enabling HTTP body logging.

### Encryption

Initial modes:

- `providerManaged`: provider-side encryption such as SSE-S3;
- `providerKms`: provider-side SSE-KMS with a key ID;
- `none`: rejected for production profiles and allowed only if the API explicitly
  chooses to support development repositories.

Do not label provider-side encryption as client-side or place key material in the
manifest. Document KMS key deletion protection and recovery.

## Observability

Metrics:

```text
kaniop_backup_duration_seconds{phase,consistency}
kaniop_backup_bytes_total{repository}
kaniop_backup_failures_total{phase,reason}
kaniop_backup_last_success_timestamp{namespace,kanidm}
kaniop_backup_age_seconds{namespace,kanidm}
kaniop_backup_discovery_duration_seconds{repository}
kaniop_backup_discovery_failures_total{reason}
kaniop_backup_gc_deleted_total{repository}
kaniop_backup_gc_deferred_total{repository,reason}

kaniop_restore_duration_seconds{phase}
kaniop_restore_attempts_total
kaniop_restore_outcomes_total{result,phase}
kaniop_restore_safety_backup_duration_seconds
kaniop_restore_break_glass_total
```

Avoid backup ID, Restore name, object key and raw error text labels.

Prometheus alerts:

- Repository not Ready;
- discovery stale or repeatedly failing;
- effective backup age exceeds configured RPO;
- safety backup failure;
- restore failure;
- restore stuck after mutation boundary;
- break-glass used;
- GC deferred beyond threshold.

Dashboard panels prioritize last committed backup, backup age, repository health,
restore outcome/duration and GC deferral.

## Work packages and patch map

### Phase 0: baseline correction

Files and work:

- run `make crdgen` so `KanidmRestore` and `Kanidm.spec.backup` appear in
  `charts/kaniop/crds/crds.yaml`;
- run `make examples` and verify generated examples;
- add `kanidmrestore` to `make clean-e2e`;
- add Helm tests for Job, PVC and VolumeAttachment permissions;
- add restore Prometheus alerts;
- add local restore E2E tests under `tests/e2e/test/kanidm/`;
- test operator restart, stale UID, wrong image, corruption and replicated
  rebuild.

Exit criterion: the existing local feature is installed by the chart and its
safety claims are exercised end to end.

### Phase 1: contracts

Likely changes:

- add Repository, Schedule and Backup CRD modules under
  `libs/operator/src/kanidm/backup/` or a dedicated backup library;
- extend `KanidmRestoreSource` and status;
- add manifest and operation-result types;
- register CRDs in `cmd/crdgen`;
- add generated examples in `cmd/examples`;
- register controllers in both operator startup modes;
- add webhook validation and RBAC resources;
- update usage documentation.

Exit criterion: CRDs install, validation works and no controller performs remote
I/O yet.

### Phase 2: S3 data mover

Likely changes:

- add data mover crate and binary under `cmd/`;
- add shared manifest/repository crate under `libs/`;
- add workspace S3 dependencies centrally;
- add image build/publish targets;
- add Repository probe controller and Job templates;
- add MinIO component-test fixture;
- document AWS and MinIO IAM.

Exit criterion: probe, upload, commit, download and delete-plan tests pass with
network and process failures injected.

### Phase 3: catalog and discovery

Likely changes:

- add discovery reconciler and Repository indexes;
- create deterministic Backup names from Repository UID and backup ID;
- add pagination, rate limits and metrics;
- add invalid-manifest diagnostics;
- test reconstruction after catalog CR deletion and operator restart.

Exit criterion: committed manifests converge to correct `KanidmBackup` resources
without sidecar Kubernetes access.

### Phase 4: safety backup

Likely changes:

- add Restore phases and status fields;
- add offline backup Job builder;
- add safety upload result reconciliation;
- implement pre-boundary cleanup/resume;
- implement Repository safety retention;
- add break-glass admission, RBAC guidance, Conditions and metrics.

Exit criterion: every normal Restore has a verified remote safety backup before
`databaseMutationStarted` becomes true.

### Phase 5: remote restore

Likely changes:

- add remote source preflight;
- add staging storage and download Job;
- feed verified source to restore Job;
- extend same-version/digest checks;
- add health/smoke checks after primary start;
- harden replicated rebuild.

Exit criterion: full remote round trip recovers semantic Kanidm state and stale
secondaries cannot reintroduce post-recovery data.

### Phase 6: retention and GC

Likely changes:

- add retention planner and property tests;
- add deleter Job and result protocol;
- implement `Deleting`, `Deleted` and `DeletionDeferred`;
- add orphan/multipart cleanup;
- add GC metrics and alerts.

Exit criterion: retention is deterministic, restart-safe and never requires
writer credentials to delete.

### Phase 7: hardening

Work:

- complete chaos/failure matrix;
- run AWS S3 interoperability tests;
- benchmark 1 GiB and 10 GiB payloads;
- conduct security review and secret-leak tests;
- validate clean-cluster DR runbook;
- document image and KMS retention;
- test operator upgrades during every non-terminal phase.

Exit criterion: restore-first GA criteria pass.

### Phase 8: experimental online transport

Likely changes:

- make Schedule render the authoritative Kanidm cron;
- inject the sidecar only into the designated primary Pod;
- add a feature gate disabled by default;
- implement candidate detection and conservative stability checks;
- use normal payload/manifest commit;
- rely on discovery for Backup creation;
- handle failover, local rotation and sidecar restart.

Restore interaction:

- mark Schedule with controller-owned `RestoreInProgress` status;
- wait a bounded period for an upload already in flight;
- do not claim the operator can cancel the sidecar through Kubernetes;
- incomplete uploads remain uncommitted staging and are retried or collected.

Exit criterion: experimental tests pass, but documentation and Conditions still
state that upstream completion is not guaranteed.

### Phase 9: production online support

Required upstream gate:

- documented completion contract or native remote shipping;
- minimum Kanidm version encoded in validation;
- version-specific integration tests;
- proof that no manifest is committed before upstream completion;
- local rotation and primary failover tests;
- performance impact within the published support envelope.

Only after this phase can backup-age alerts derived from scheduled online
transport be treated as a production RPO signal.

## Test plan

### Unit tests

Cover:

- all CRD defaults, serialization and CEL schema;
- cross-resource webhook validation;
- one Schedule per Kanidm;
- Repository immutability and duplicate-prefix checks;
- manifest versions and bounded parsing;
- object-key confinement;
- commit state transitions and idempotent backup IDs;
- retention union, minAge and safety retention;
- mutation boundary;
- break-glass validation;
- deterministic child names;
- error classification and backoff;
- Prometheus label cardinality.

### Property tests

Prove:

- protected backups are never selected by retention;
- retries never create a second logical backup ID;
- path construction cannot escape a Repository prefix;
- a Backup cannot be Ready without a committed manifest and valid payload
  evidence;
- no pre-boundary failure leaves Kanidm intentionally scaled down;
- no post-boundary failure resumes unverified service.

### Component tests with MinIO

Cover:

- capability probe;
- conditional immutable PUT;
- multipart success, timeout, resume and abort;
- process death after payload and before manifest;
- process death after manifest and before controller status;
- exact-key read after write;
- delayed LIST discovery;
- invalid/oversized manifest;
- payload checksum mismatch;
- wrong encryption expectation;
- credential rotation and denial;
- private CA and invalid TLS;
- manifest-first deletion and partial GC;
- Object Lock/retention denial where supported.

### Controller integration tests

Cover:

- missing/deleted references;
- status conflicts and watch restarts;
- Repository re-probe;
- discovery pagination and duplicate manifests;
- Job result parsing;
- finalizer behavior;
- restore deletion before and after mutation;
- operator restart at every phase;
- Kubernetes API timeouts and transient 5xx;
- volume detach timeout;
- maintenance gate across every identity reconciler.

### Restore-first E2E

Primary scenario:

1. Create a PVC-backed replicated Kanidm.
2. Create representative Persons, Groups, OAuth2 clients and Service Accounts.
3. Produce/import a known remote backup and wait for catalog Ready.
4. Apply later creates, updates and deletions.
5. Create a remote `KanidmRestore`.
6. Assert complete quiesce and write gating.
7. Assert offline safety backup is remotely committed and cataloged.
8. Assert source is downloaded and verified before mutation.
9. Assert `databaseMutationStarted` is persisted before restore Job creation.
10. Assert restore and database verification succeed.
11. Assert exactly one primary starts first and passes smoke checks.
12. Assert secondary PVC state is rebuilt.
13. Assert recovery-point data semantics.
14. Assert reconciliation resumes.
15. Assert fixture credentials never appear in logs or Events.

Required failure scenarios:

- source missing or corrupt before downtime;
- source checksum mismatch before mutation;
- safety backup command failure;
- safety upload failure;
- operator crash after safety commit but before status;
- unauthorized and authorized break-glass;
- target UID mismatch;
- domain mismatch;
- wrong Kanidm version and digest;
- exact image unavailable;
- concurrent Restore;
- delete Restore in every phase;
- restore Job failure after mutation;
- verify Job failure;
- primary health failure;
- stale secondary with post-backup state;
- Repository credentials rotate during operation;
- GC attempts to select active source/safety backup.

### Experimental online tests

Against every supported Kanidm version:

- observe actual filename/write behavior;
- kill Kanidm during online generation;
- rotate local versions during upload;
- restart sidecar after payload and before manifest;
- restart operator before discovery;
- fail over the designated primary;
- verify no Kubernetes token in sidecar;
- verify heuristic mode always reports its experimental Condition.

These tests do not replace the upstream completion gate.

### Benchmarks

Measure separately:

- offline backup generation;
- upload and multipart overhead;
- source download;
- restore command;
- database verification;
- primary readiness and replica rebuild;
- maximum staging space;
- CPU/RSS and I/O;
- S3 request count and cost drivers.

Reference matrix:

- 100 MiB, 1 GiB and 10 GiB in required CI/nightly tiers;
- one and replicated topologies;
- MinIO and AWS S3;
- 1 Gbit/s, 100 Mbit/s and 20 Mbit/s shaping;
- low and high latency;
- throttled and unthrottled transfer.

Publish observed RTO envelopes instead of a universal promise.

## Documentation deliverables

Update:

- `docs/adr/0001-production-kanidm-backup-and-restore.md`;
- `docs/plans/production-kanidm-backup-and-restore.md`;
- `Documentation/src/usage/backup-restore.md`;
- troubleshooting with phase-specific recovery;
- Helm values and generated examples when configuration exists;
- a restore-first operations runbook;
- a clean-cluster DR runbook;
- AWS/MinIO IAM examples;
- KMS and image-retention guidance;
- experimental online limitations.

## Verification commands

For each implementation slice, run the applicable focused tests and always run:

```bash
make crdgen
make examples
make lint
make test
make book
helm unittest charts/kaniop
```

For storage/controller work also run MinIO component tests. For phases that
change observable recovery behavior, run the relevant Kind E2E suite. Never mix
integration and E2E Cargo feature flags.

Generated CRDs and examples must be committed when their source definitions
change. CI should fail on generated-file drift.

## Acceptance criteria

### Alpha

- all four APIs install and validate;
- manifest v1 is versioned and bounded;
- Repository probe and role separation work with MinIO;
- payload/manifest commit is idempotent;
- catalog discovery reconstructs Backup CRs;
- offline safety backup and single-node remote restore pass E2E.

### Beta: production restore-first

- replicated remote restore passes semantic E2E;
- every normal restore commits a safety backup before mutation;
- all pre-boundary failures resume the old service;
- all post-boundary failures keep unverified service offline;
- operator restart is tested at every phase;
- break-glass authorization and audit evidence are verified;
- retention and Object Lock deferral are tested;
- metrics, alerts and runbooks exist;
- AWS S3 interoperability passes.

### GA: production restore-first

- security review has no unresolved critical findings;
- chaos matrix passes;
- clean-cluster DR has been exercised;
- 1 GiB and 10 GiB support envelopes are published;
- KMS and exact-image retention are documented and tested operationally;
- operator upgrade during active backup/restore is safe;
- chart, CRDs, examples, documentation and E2E are release-gated.

### Production online-backup gate

- Kanidm provides a documented completion contract or supported native shipping;
- Kaniop requires the corresponding minimum Kanidm version;
- integration tests prove manifests are never committed early;
- primary failover and local retention races are tested;
- performance impact is measured and documented;
- the experimental feature gate and warning Condition can be retired.

## Rollout and migration

1. Correct and test the existing local backup/restore implementation without
   changing behavior for users that do not configure backup.
2. Install the three new CRDs additively with no sidecar injection.
3. Enable Repository, catalog, safety backup and remote restore behind an
   operator feature gate during alpha.
4. Preserve `source.local.fileName` for at least one deprecation window.
5. Do not automatically convert local files into committed remote backups.
6. Provide an explicit import workflow that records Kanidm version, image
   digest, source UID and checksum without modifying the payload.
7. Enable restore-first production support after beta/GA gates.
8. Introduce the online sidecar only when users explicitly enable the
   experimental gate.
9. Remove local-source restore only through normal API deprecation and storage
   version migration.

Existing `Kanidm` resources without a Schedule or Repository must render the
same workload as before. Installing new CRDs alone must not restart Kanidm Pods.

## Major risks

| Risk | Mitigation |
|---|---|
| Safety backup extends outage | Stream and throttle predictably; benchmark; retain narrow break-glass |
| In-place restore damages the only PVC | Remote safety backup and fail-closed boundary; future blue/green |
| Online file uploaded before complete | Experimental only until upstream completion contract |
| Data mover compromises writer credentials | No K8s token, prefix-scoped writer, no delete/overwrite, Pod threat model |
| Catalog is lost | Reconstruct from manifests with reader discovery |
| GC deletes a protected backup | Union retention, recheck, separate plan/job, property and E2E tests |
| S3-compatible semantics differ | Capability probe and explicit support matrix |
| KMS key or Kanidm image is lost | Independent retention and DR runbook |
| Manifest and payload are maliciously replaced | Immutable keys, split roles, Object Lock; signing for stronger threat model |
| Restore reintroduces newer secondary state | Primary-only seed and destructive secondary rebuild |
| Operator crashes during destructive phase | Persisted phases, deterministic Jobs and mutation boundary |
