# Task

During a live review of Kaniop v0.24.1 backups, a `KanidmBackupRepository` was given a writer
credential without `s3:ListBucket`. A second repository was given credentials that MinIO rejects.
In both cases the `data-mover-transport` sidecar of the Kanidm primary exited with code 1 at
startup:

```text
ERROR list objects failed after retries ... 403 AccessDenied
```

The sidecar is a native sidecar (an init container with `restartPolicy: Always`). It went into
`Init:CrashLoopBackOff`, and the primary pod became `Ready=False` (`ContainersNotReady`) even
though the `kanidm` container itself was running and ready. The pod was removed from the Service
EndpointSlice. With a single-replica Kanidm this is a full IdM outage caused only by a backup
credential problem. The Schedule stayed `Ready=True` and the repository stayed `Ready=True`.
The same thing happens if S3, TLS or DNS is unavailable when the primary pod starts. A comment in
the StatefulSet builder says the sidecar is deliberately kept out of Pod readiness.

Make sure that S3, network and credential problems in the transport sidecar can never take the
Kanidm pod out of service. The sidecar should keep running and retry with a capped backoff,
honoring shutdown, and log clearly why it is not uploading. Genuinely invalid configuration
must not be retried forever. Fix the misleading comment. Add unit tests that inject failures
without a real S3, and an e2e test showing that a primary with invalid repository credentials
stays Ready.
