# Backup System Hardening

Status: **Proposed**

Related:

- [ADR 0001 — Production Kanidm backup and restore](../adr/0001-production-kanidm-backup-and-restore.md)
- [Original backup plan](./production-kanidm-backup-and-restore.md)
- [Transport plan](./backup-transport.md)

## Summary

Hardening pass over the backup/restore feature prompted by a live-cluster incident: the
transport sidecar crash-loops with `watch directory does not exist or is not a directory`
on non-primary pods (and on fresh PVCs before the first backup). Investigation revealed a
set of correctness, data-leak, and validation gaps. This plan fixes the transport startup
ordering, closes the S3 object leak with proper deletion integrity, implements the
`encryption` CRD field (currently silently ignored), makes restore job volume sizes
configurable, validates cron schedules at admission, and fills the e2e coverage gaps.

## Gaps being addressed

| # | Gap | Root cause | Evidence |
|---|-----|-----------|----------|
| 1 | Sidecar crash-loops on non-primary pods | `check_primary_gate()` runs after watch-dir check and S3 init | `transport.rs:36-40` exits(2) before gate at `transport.rs:218-233`; observed on `kanidm-default-1` in grigri |
| 2 | Sidecar crash-loops on fresh PVCs | `/data/backups` created lazily by kanidmd at first backup; sidecar mounts data PVC read-only, no initContainer mkdir | `transport.rs:36-40`, `statefulset.rs:954-960` |
| 3 | S3 payload leak on retention/delete | deletion Job deletes only the manifest key; payload objects under `payload/` are orphaned | `backup.rs:839` (`keys_to_delete = vec![manifest_key]`) |
| 4 | Deletion not durable | `KanidmBackup` has no finalizer; CR vanishes from etcd before/during deletion Job; failures orphan everything | `crd.rs:430-445`, `backup.rs:489-506` |
| 5 | `encryption` spec field is a no-op | mode/keyId flow into operation docs and manifest but data-mover never acts on them; restore upload hardcodes `None` | `upload_shared.rs:235-239`, `restore.rs:2585-2586` |
| 6 | Restore emptyDir sizes hardcoded 10Gi | breaks restores on large databases | `restore.rs:2299` (shared), `restore.rs:2507` (staging) |
| 7 | Cron `schedule` never validated | garbage strings pass admission and are rendered into `[online_backup]` | webhook `handlers.rs:294-298` only checks non-empty |
| 8 | `imageDigest` always `None` in transport op doc | not propagated | `transport.rs:183` |
| 9 | e2e gaps | retention deletion, invalid cron, encryption, non-primary idle — untested e2e | `tests/e2e/test/kanidm/backup.rs` |

## Phase 1 — Transport startup ordering (gaps 1, 2, 8)

The transport plan already committed to "other pods in the primary replica group run an
idle sidecar"; the implementation breaks that promise by checking the watch dir and
building the S3 client before the primary gate.

**Changes** (`cmd/data-mover/src/commands/transport.rs`):

1. Move `check_primary_gate()` to the first statement of `run()` — before the watch-dir
   check and before S3 client creation. Non-primary pods must do **nothing at all**: no
   dir check, no bucket client, one info log, sleep loop.
2. Replace the fatal `watch_dir.is_dir()` check with a tolerant wait: poll until the
   directory appears (log once at info, re-log at most once per interval at debug), then
   enter the existing backfill + poll loop. Handles fresh PVCs and restores where kanidmd
   hasn't written a backup yet. Never exit(2) for a missing dir.
3. Propagate `image_digest` into the transport operation document from the kanidmd
   container image resolution already available in `build_transport_sidecar()`
   (`statefulset.rs:921-990`).

**Notes:**

- No CRD changes. No topology changes — StatefulSets are homogeneous and the
  runtime self-gate is the documented design.
- Keep S3 client construction lazy (after the gate) so non-primary pods never hold
  credentials-derived state.
- Live-cluster remediation: next reconcile that injects the sidecar rolls the StatefulSet;
  already-injected sidecars pick up fix on pod restart.

**Tests:**

- Unit: gate-first ordering (non-primary returns Ok(idle) without touching dir or S3).
- Unit: missing dir → waits, then proceeds when dir appears (tempdir + spawn thread).
- e2e (`backup_transport.rs`): assert sidecar container on pod-1 (non-primary) reaches
  Ready and has zero restarts after rollout.

## Phase 2 — Deletion integrity (gaps 3, 4)

**Payload enumeration.** The operator pod does not speak S3 — the data-mover Job does.
Extend `delete-plan` (`cmd/data-mover/src/commands/delete_plan.rs`) to accept a backup
prefix mode: given `{prefix}/.../backups/{id}/`, list all objects beneath it
(manifest + payload) and delete them. `libs/backup/src/controller/backup.rs:839` then
passes the backup prefix instead of only the manifest key. Deletion remains explicit and
confined: `paths.rs` `contains_key` confinement already guarantees the prefix stays inside
the repository's tenant/cluster namespace — keep that guard and unit-test it for the new
prefix mode.

**Finalizer.** Add finalizer `kanidmbackups.kaniop.rs/finalizer` to `KanidmBackup`:

- Attach on first reconcile (any phase with SSOT CR present).
- On `deletion_timestamp`: run the existing Deleting phase (deletion Job); remove the
  finalizer only after the Job reports success **and** discovery confirms zero remaining
  keys under the backup prefix (defensive re-list via the same Job result document).
- Deletion Job failure → keep finalizer, exponential backoff retry (existing
  `backoff_reconciler!`), surface `DeletionDeferred`-style condition with the error.
- Orphan-prevention invariant: a `KanidmBackup` CR never leaves etcd while objects may
  still exist in S3.

**Docs:** CRD needs regeneration (`make crdgen`) and finalizer behavior documented in
`Documentation/src/usage/backup-restore.md`.

**Tests:**

- Unit: delete-plan prefix expansion, confinement rejection for foreign prefixes.
- Unit: finalizer add/remove transitions, backoff on Job failure.
- e2e (`backup.rs`): create schedule with `keepLast: 2`, force >2 backups, assert old CRs
  deleted **and** MinIO bucket has no residual objects under their prefixes
  (closes gap 9 retention item).
- e2e: `kubectl delete kanidmbackup` blocks until objects are gone.

## Phase 3 — Encryption (gap 5)

**Decision: support all modes, default to none.** When `spec.encryption` is absent, no
encryption occurs (no SSE headers, no client-side transform) — this is the documented
default and current effective behavior. Supported modes, designed to compose (client-side
and SSE may both be active; the manifest records each independently):

| `mode` | Mechanism | Key custody |
|--------|-----------|-------------|
| _absent_ | none | — |
| `providerManaged` | SSE-S3 (`x-amz-server-side-encryption: AES256`) | provider |
| `providerKms` + `keyId` | SSE-KMS (`aws:kms` + key-id header) | provider KMS |
| `clientSide` + `keyRef` | client-side envelope (below) | user Secret / KMS |

**Threat model honesty**: SSE is transparent at the API — anyone with valid S3 read
credentials gets plaintext. Only `clientSide` protects against leaked storage credentials
or a hostile/compelled provider. Kanidm backups contain password hashes and TOTP seeds, so
client-side is the mode that matches the sensitivity of the data.

### 3a — Server-side encryption headers

Mapping per table above. Changes:

1. **CRD CEL** (`libs/backup-core/src/crd.rs:140-156`): `keyId` required iff
   `mode == providerKms`; forbidden otherwise; `keyRef` required iff `mode == clientSide`.
   Replacing the "server-side encryption" doc comment with per-mode semantics.
   Regenerate CRDs and examples (`make crdgen`, `make examples`).
2. **Header plumbing** (`cmd/data-mover/src/s3.rs`): rust-s3 0.37 must send SSE headers
   on every object PUT and — critically for SSE-KMS — on `initiate_multipart_upload`
   (S3 ignores per-part SSE headers). Verify whether 0.37 exposes
   `initiate_multipart_upload_with_headers`; if not, implement the initiate call via
   rust-s3's `Request`/`Command` primitives locally rather than forking the multipart
   state machine.
3. **Wire consumers**: upload (`upload_shared.rs`), transport, and restore safety-backup
   upload (`restore.rs:2585-2586` currently hardcodes `encryption_mode: None`) all read
   the encryption fields already present in their operation documents.
4. **Download**: no changes for SSE-S3/KMS (server decrypts transparently).

### 3b — Client-side envelope encryption

Multipart-compatible by aligning crypto chunks 1:1 with multipart parts:

- **Envelope scheme**: random 256-bit DEK per backup; DEK wrapped by a KEK; wrapped DEK,
  nonce salt, chunk size, and algorithm recorded in the manifest (extend
  `ManifestEncryption` additively — schema already versioned; unknown optional fields must
  not break older readers, verify serde behavior).
- **AEAD per part**: AES-256-GCM; each multipart part independently sealed; nonces derived
  as `salt || part_index` — safe because the DEK is unique per backup (no global nonce
  state, no reuse). Per-part integrity plus existing `payloadSha256` for whole-file
  verification.
- **Data path**: encrypt each part buffer before `put_multipart_chunk` / `put_object`;
  download decrypts part-by-part with the same constant-memory streaming properties as
  today. Single-part path uses one chunk.
- **KEK source**: `spec.encryption.keyRef` as `SecretRef` projected via the existing
  `auth.rs` env/volume pattern into data-mover Jobs and the transport sidecar. Later
  (deferred): KMS-wrapped DEKs via workload identity — same wrap interface, KMS sees only
  32-byte payloads.
- **Restore/download integration**: decrypt failures fail closed at `PreparingSource`
  with a clear condition/reason (wrong/missing KEK); safety-backup upload encrypts with
  the same fields (fixes the `restore.rs:2585` hardcode for this mode too).
- **Rotation** (documented, `rekey` subcommand deferred): re-wrap = rewrite wrapped-DEK
  blobs in manifests, no data re-upload. Document that KEK loss = unrecoverable backups.
- **Deps**: `aes-gcm` (RustCrypto, pure Rust, small tree) in `[workspace.dependencies]`.
  Rejected: `age` (single-stream, kills multipart), AWS Encryption SDK (provider-locked).
- New dep: `aes-gcm`; randomness from `rand` (already in tree — verify).

### 3c — Validation & immutability (both modes)

- Webhook: reject removal of `encryption` or changing `mode`/`keyId`/`keyRef` while any
  `KanidmBackup` references the repository (same immutability-after-use pattern as
  bucket/prefix, `backup_validator.rs`).
- CEL (3a.1) covers spec-internal consistency; webhook covers cross-resource history.

**Tests:**

- Unit: header construction per SSE mode; multipart initiate carries headers; CEL
  fixtures; webhook immutability cases; AEAD roundtrip (encrypt/decrypt per part,
  tamper detection), nonce derivation uniqueness, manifest add-on serde compatibility.
- e2e: `clientSide` roundtrip — schedule backup with `clientSide` + Secret KEK, assert
  MinIO object bytes differ from plaintext and payload decrypts via restore roundtrip;
  restore with wrong KEK fails closed at `PreparingSource` with the documented condition.
- e2e (SSE): MinIO SSE requires KES; if unavailable in the in-cluster fixture, detect and
  skip the HEAD assertion, keeping upload/download roundtrip assertions (transparent to
  the server) — pure header correctness stays unit-tested.

## Phase 4 — Configurable restore job volumes (gap 6)

Follow the existing operator configuration pattern (AGENTS.md): clap arg in
`cmd/operator/src/main.rs` → `OnceLock` in `libs/operator/src/controller/mod.rs` → used by
the restore reconciler.

- One flag: `--backup-job-volume-size` (env `BACKUP_JOB_VOLUME_SIZE`), default `10Gi`,
  parsed as `k8s-openapi::apimachinery::pkg::api::resource::Quantity`; invalid values fail
  fast at startup.
- Applied to both the safety-backup shared volume (`restore.rs:2299`) and the source
  staging volume (`restore.rs:2507`). These volumes serve equivalent roles (spill space
  for one database copy), so a single size is deliberate; per-restore overrides are a
  non-goal.
- Document in `Documentation/src/usage/backup-restore.md` and the Helm chart (env pass-
  through value if the chart exposes operator extra env; otherwise document `--set` args).

**Tests:** unit (flag parsing, value reaches both volume builders), chart unittest if a
value is added.

## Phase 5 — Cron schedule validation (gap 7)

- Validate `spec.schedule` syntax in the webhook (`cmd/webhook/src/handlers.rs:294-298`)
  and defensively in the schedule controller. Use `saffron` (the parser Kanidm itself
  uses for `[online_backup]`) for semantic parity — an expression we accept but kanidmd
  rejects is the failure mode being eliminated. Verify the exact crate/version Kanidm
  uses at implementation time before adding the dependency to `[workspace.dependencies]`.
- Rejection is non-breaking: previously-accepted garbage strings were rendered into
  kanidmd config and failed there anyway.

**Tests:** webhook unit fixtures (valid standard + non-standard forms, empty, garbage);
e2e asserting admission denial for `"not-a-cron"` and `"60 * * * *"`.

## Phase 6 — Coverage & docs (gap 9 + bookkeeping)

- e2e additions: retention deletion (Phase 2), invalid cron (Phase 5), encryption
  roundtrip (Phase 3), non-primary idle (Phase 1), corrupt/truncated source backup →
  restore fails cleanly at `PreparingSource`/`Validating` without touching the database.
- Update `check-e2e-shards` filters since `kanidm-data` will grow.
- After Phases 1–5 land and soak, revisit the "experimental" label on transport
  (`TransportExperimental` condition, docs warnings) — specifically whether crash-loop
  elimination + deletion integrity is enough to drop the "never appears stuck" caveats.
- Verify all 8 PrometheusRule alerts fire for the new failure modes (deletion Job
  failure, finalizer-stuck) — extend `KaniopBackupGCDeferred` coverage if needed.

## Out of scope

- KMS-wrapped DEKs for `clientSide` via workload identity — deferred; KEK via Secret only.
- KEK rotation tooling (`rekey` subcommand) — documented mechanism, implementation deferred.
- SSE-C (customer-provided keys) — not implemented.
- Per-pod topology changes (single-replica primary StatefulSet) — rejected: breaks the
  replica-group abstraction; the runtime self-gate is the documented design.
- Dynamic primary failover detection — unchanged per transport plan non-goals.
- Workload-identity e2e against real IRSA — unit/integration level only for now.

## Sequencing

Phases 1 and 2 are independent and highest priority (live crash-loop, active data leak).
Phase 3 is the largest and can start in parallel once its CEL/webhook surface is agreed.
Phases 4 and 5 are small and slot anywhere. Phase 6 spreads across the others.
