# Task

PR #1112 added `credentialBootstrapDelivery` with `default: event` to the
`KanidmPersonAccount` CRD. Since then, every Helm upgrade or Argo CD sync of an
installation with person resources fails in the chart's CRD migration hook:

```text
structural verification failed: 0 missing restorations, 4 checksum mismatches
```

The `CRD Migration E2E` workflow, which exercises the upgrade path, has not run on master
since 2026-10-24, so nothing reported the regression. Find out why the upgrade test did not
catch the change, and make the CRD upgrade e2e run whenever a change can alter the CRDs or
the chart that applies them. The test must prove that an upgrade from the previous published
release applies the current CRDs and that existing and new resources still reconcile.
