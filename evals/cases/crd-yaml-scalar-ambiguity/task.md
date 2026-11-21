# Task

PR #1203 adds `Kanidm.spec.mailSender.logLevel`, a typed enum with the values `trace`, `debug`,
`info`, `warn`, `error` and `off`, rendered as `RUST_LOG` on the mail-sender container. Locally
`make lint` and `make test` pass, and `make crdgen` produces the expected schema. On CI the CRD
Upgrade E2E workflow fails in both of its jobs:

`Helm CRD Upgrade E2E`:

```text
[2026-11-21T00:01:38Z] Verifying that the crdApply hook applied the current CRDs
--- /tmp/tmp.8rptrsuMTf/expected-kanidms.kaniop.rs.json
+++ /tmp/tmp.8rptrsuMTf/actual-kanidms.kaniop.rs.json
@@ -4599,7 +4599,7 @@
                         "info",
                         "warn",
                         "error",
-                        false,
+                        "off",
                         null
                       ],
                       "nullable": true,
[2026-11-21T00:01:38Z] FATAL: CRD kanidms.kaniop.rs spec does not match charts/kaniop/crds/crds.yaml
```

`Argo CD CRD Upgrade E2E` fails on the same installation with `Application kaniop did not become
Healthy+Synced within 600s`, and `kanidms.kaniop.rs` is the resource reported as `OutOfSync`.

Explain why the file `make crdgen` writes and the CRD that reaches the cluster disagree, and fix
it. Every other job on the branch is green.
