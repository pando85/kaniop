# Task

The new `KanidmRadiusServer` controller (`libs/radius/`, PR #1120) passes `make test`, but the
live `radius` e2e shard (`make e2e && make e2e-test-shard SHARD=radius`) shows two failures that no
unit test reported.

1. **Deletion hangs.** Two `KanidmRadiusServer` resources share a namespace. Deleting one of them
   never finishes: the finalizer keeps waiting for consumers of its tokens and config Secrets, and
   the `AwaitingConsumers` warning names pods that belong to the other server. The other server
   rotated its token recently, so its pods carry a different `kaniop.rs/radius-token-id` than the
   one being deleted.
2. **UDP requests are lost during a rollout.** While the e2e rotation test replaces the token and
   rolls the Deployment (`maxUnavailable: 0`), a probe sending `Access-Request` packets over UDP
   through the Service sees 6 failures in 119 requests. Authentication works before and after the
   rollout. Pods are not crashing and every replacement pod is ready before an old one is removed.

Fix both in the controller so the e2e shard passes. A mocked unit test is not enough evidence for
either fix: each must be explained by the behaviour of real pods, real Services and real
termination, and the regression coverage must say which layer proves it (unit test for what can be
decided from inputs, e2e for what needs a cluster). Do not weaken the probe, the e2e assertions or
the deletion wait to make the shard pass.
