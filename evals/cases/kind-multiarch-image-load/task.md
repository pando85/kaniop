# Task

The CI Fix pipeline is red on this repository.

The `CRD Upgrade E2E` workflow fails in both the `Helm CRD Upgrade E2E` and
`Argo CD CRD Upgrade E2E` jobs while loading the webhook certificate image into the
Kind cluster:

```text
ERROR: failed to load image: command "docker exec --privileged -i kaniop-crd-upgrade-control-plane \
ctr --namespace=k8s.io images import --all-platforms --digests --snapshotter=overlayfs -" \
failed with error: exit status 1

Command Output: ctr: content digest sha256:0acfba2a51945193d54d2bc2683615053f406c247c6c6eafe253bdaf5b1b5d25: not found
```

It last passed on 2026-11-12, and nothing in the change under test touched the images.

The `Rust` workflow also fails in the e2e shard jobs on the `ubuntu-24.04-arm` runners
(`e2e kanidm-backup`, `e2e kanidm-restore`, `e2e oauth2`, ...), while the same shards pass on
`ubuntu-24.04`:

```text
Warning  BackOff  kubelet  Back-off restarting failed container minio in pod minio-...
exec /usr/bin/docker-entrypoint.sh: exec format error
make: *** [Makefile:440: e2e-test-shard] Error 2
```

Fix both so CI is green. The e2e matrix runs Kind clusters on x86_64 and aarch64 runners, and
`.github/workflows/crd-upgrade-e2e.yml` runs its jobs on `ubuntu-latest`.
