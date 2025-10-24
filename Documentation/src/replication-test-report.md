# Replication

## Overview

This document records observed facts from replication-related tests run against Kanidm clusters
managed by the Kaniop operator. The content is limited to the original test notes and their explicit
outcomes; no additional analysis or assumptions have been added.

## Admin passwords

When external replication nodes are configured and `automaticRefresh` is enabled,
`admin/idm_password` becomes invalid on replica nodes because it takes the value from the primary
node.

## Creation with invalid secret name

### Test and fix

- Deploy Kanidm with an external replication entry that references a non-existent secret and
  `optional: false`.
- Update the replication entry to reference a valid secret name.

### Expected

- After correcting the secret reference, the affected pod(s) should be recreated and the deployment
  should recover.

### Observed

- Fixing the secret reference did not automatically recreate the pod(s) or recover the deployment in
  the tested runs.
- Certificate creation failed during the recovery attempts.

### Relevant detail

- The StatefulSet pod management policy is `OrderedReady` by default, so a pod is not deleted until
  the next pod becomes `Ready`.
  [https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/#pod-management-policies](https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/#pod-management-policies)

## Create a cluster when admin secret already exists

If the admin secret already exists at cluster creation time, nodes did not become functional in the
tested flows and TLS secrets had to be recreated to restore operation.

## Admin passwords not valid (joining as a pull replica)

When a node joins an external Kanidm cluster as a pull replica, admin passwords on the joining node
stop working in the observed runs. Tests show the operator did not automatically reset those
passwords.

## Replica 0 recreation with primary node present

### Observed

- Recreating Replica 0 while the primary node is present caused Replica 0 to start and report
  `Ready`, but the admin password on that pod was unset and the node was unusable for admin
  operations.
- With a load balancer configured for session stickiness, clients pinned to Replica 0 could not
  perform admin actions.

### After manual intervention

- Manually recreating the admin password secret and the TLS secrets restored Replica 0’s
  functionality.
- In the observed recovery, replica data was wiped and then resynchronized from the primary.

## Replica 0 recreation without primary node present

### Observed

- Recreating Replica 0 while the primary node was unavailable required manual secret recreation for
  the pod to start.
- Logs showed errors indicating Replica 0 could not pull changes (could not synchronize).

### After manual intervention

- Manually recreating admin and TLS secrets allowed the pod to start, but synchronization errors
  remained in the logs and additional manual steps were required to recover data.

## Considerations

- Data on Replica 0 can be recovered by running a sync command on that replica.
- Admin passwords on other replicas will be lost and must be reset manually, or they will be reset
  when syncing and recreating the admin secret.

## Improvements

- Readiness probes should fail if the admin password is not set, because the node is not usable for
  administrative operations.
- Readiness probes should fail when the node is not synchronized with the cluster.
