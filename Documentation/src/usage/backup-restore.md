# Backup and Restore

Kaniop uses Kanidm's native logical backup format. The operator configures the online backup scheduler on exactly one primary node and stores local artifacts under `/data/backups` on the Kanidm PVC.

```yaml
spec:
  backup:
    schedule: "0 2 * * *"
    versions: 7
```

Local backups require PVC-backed storage and one `replicaGroup` with `primaryNode: true`. Kaniop intentionally does not claim PITR semantics or a globally atomic point-in-time cut across replicated writable nodes.

## Restore

Restore is an explicit destructive operation represented by `KanidmRestore`. Obtain the target UID with `kubectl get kanidm <name> -o jsonpath='{.metadata.uid}'`, select an existing backup basename from `/data/backups`, and use the same pinned Kanidm image as the target. `latest` and untagged images are rejected.

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmRestore
metadata:
  name: my-idm-restore
spec:
  targetRef:
    name: my-idm
    uid: <kanidm-uid>
  source:
    local:
      fileName: backup.json.gz
  restoreImage: kanidm/server:1.10.0
```

The controller validates the request, marks the target in maintenance, scales all Kanidm pods down, runs `kanidmd database restore`, verifies the database offline, discards stale secondary PVC data, starts the restored primary, rebuilds replicas through normal Kanidm replication, and only then resumes ordinary Kaniop reconciliation. A failure after database mutation is fail-closed: the restore remains `Failed` and the target remains marked as restoring.

Restoring a historical database is followed by GitOps reconciliation. Declaratively managed Kaniop resources can therefore be recreated or changed after recovery.

## S3 and infrastructure snapshots

Kaniop does not implement a separate S3 uploader. Native S3-compatible shipping remains deferred until Kanidm exposes its supported upstream interface. CSI/Velero snapshots can be used as an independent disaster-recovery layer, but they are not represented as equivalent to a Kanidm-native logical backup.
