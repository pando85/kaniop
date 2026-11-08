# Task

`CRD Upgrade E2E` failed on the `Helm CRD Upgrade E2E` job while installing kind.
Four earlier commits on this branch had already tried to fix the same job by
adding curl retries and a second download URL:

```text
Attempt 1 to install kind...
curl: (22) The requested URL returned error: 504
curl: (22) The requested URL returned error: 504
curl: (22) The requested URL returned error: 504
curl: (22) The requested URL returned error: 504
...
GitHub releases failed, trying kind.sigs.k8s.io fallback...
curl: (22) The requested URL returned error: 504
Failed to install kind after all attempts
##[error]Process completed with exit code 1.
```

The same step is duplicated in the Argo CD upgrade job and in the `e2e` job of
`rust.yml`, which also installs kind and kubectl for an `ubuntu-24.04-arm`
matrix leg and helm from `get.helm.sh` as a tarball. The Argo CD job additionally
installs Argo CD with `kubectl apply -f https://raw.githubusercontent.com/...`.

After those jobs were fixed, the `Helm` workflow failed on the same branch for a
different reason, in `make -C charts lint`:

```text
helm unittests not found. Installing...
Fetching latest helm-unittest release...
curl: (56) Connection died, tried 5 times before giving up
curl: (22) The requested URL returned error: 503
curl: (22) The requested URL returned error: 503
ERROR: Failed to fetch latest helm-unittest version
make: *** [Makefile:15: dependencies] Error 1
```

Make these jobs stop failing on transient release-hosting problems. Keep the
pinned versions under renovate control, and do not weaken what the upgrade tests
verify.
