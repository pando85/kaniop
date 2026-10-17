A production cluster running Kaniop v0.18.0 with a healthy `KanidmBackupSchedule` reports a frozen
discovery status even though discovery is succeeding every five minutes. `kubectl get
kanidmbackupschedule -o yaml` shows `status.discovery.lastScanTime` advancing every tick, but
`lastSuccessfulScanTime` is stuck six weeks in the past, `lastDiscoveredCount` is stale, the
`Discovered` condition is `False/Discovering` with an old `lastTransitionTime`, and a second
condition, `DiscoveryFailed`, is also `False` with a reason and status that disagree. Meanwhile
`kaniop_backup_discovery_last_scan_success_timestamp` advances every tick and backups are being
catalogued normally, so the persisted status contradicts the metrics. The operator is also creating
a discover Job every five minutes instead of once per stale window, which is five times the S3
`LIST` load the schedule should be paying.

Fix the operator so a discovery tick cannot clobber its own status: whatever a scan learned about
success, count, truncation and prune must persist in the same tick that reports it, and the
staleness gate must honour the most recent discovery signal even after a schedule has ever failed.
The fix must not narrow the condition set the single apply declares, must not introduce a second
field manager, must not change the `DiscoveryFailed` polarity or populate `lastError` (that is a
separate follow-up), and must not rename the `namespaceUid` wire field or move the `tenants/`
keyspace segment (also a separate follow-up). The documentation and the e2e assertions must state
the real cadence honestly — `T*(floor(S/T)+2)` where `T` is the scan interval and `S` is the stale
threshold, assuming the Job completes before the next tick — and must not claim
`max(scan_interval, stale_threshold)`. Add regression tests that would fail if a second apply or a
stale base were reintroduced, and do not accept a test that only asserts `.force == true` on params
built by the same expression as the production code.
