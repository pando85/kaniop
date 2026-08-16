---
name: grafana-dashboard
description: Improve and validate the Kaniop Grafana dashboard against repository metrics and the grigri live cluster. Use when changing dashboard panels, PromQL, variables, units, legends, or layout in charts/kaniop/files/dashboards/kaniop.json.
---

# Kaniop Grafana Dashboard

Use this workflow for every dashboard change. Do not finish until both Grafana MCP query validation and Passless-managed visual validation succeed.

## Source of truth

- Dashboard: `charts/kaniop/files/dashboards/kaniop.json`
- Metrics: `libs/operator/src/metrics.rs`, `libs/k8s-util/src/metrics.rs`
- Provisioning: `charts/kaniop/templates/dashboard-configmap.yaml`
- Helm tests: `charts/kaniop/tests/dashboard-configmap_test.yaml`
- Live Grafana: `grafana.grigri.cloud`
- Live datasource: discover it with `grafana-grigri_list_datasources`; never assume a UID

## Orchestration

Delegate independent discovery to subagents:

1. An `explore` agent inventories metric definitions, label sets, dashboard queries, and tests.
2. A second agent reviews the proposed PromQL and panel design.
3. Keep edits, MCP validation, and final browser acceptance in the orchestrator.

## Workflow

1. Inventory metric names and labels from Rust before editing.
2. Discover live Kaniop metrics with `grafana-grigri_list_prometheus_metric_names` using `^kaniop_.*`.
3. Confirm labels and values with `grafana-grigri_list_prometheus_label_names` and `grafana-grigri_list_prometheus_label_values`.
4. Compare repository metrics with live metrics. Do not add a panel that has neither live data nor an explicit reason to document an expected absent series.
5. Edit the provisioned dashboard. Preserve its stable `uid`; omit the Grafana database-level `id`.
6. Validate every changed PromQL expression with `grafana-grigri_query_prometheus` over a representative live time range.
7. Upload a development copy through `grafana-grigri_update_dashboard`:
   - remove `id`
   - use UID `kaniop-dev`
   - set title to `Kaniop — DEV VALIDATION`
   - set `overwrite: true`
   - leave the development copy in place for later iterations
8. Retrieve the uploaded copy with `grafana-grigri_get_dashboard_summary` and `grafana-grigri_get_dashboard_panel_queries`; verify panel count, datasource, and exact expressions.
9. Run `helm unittest charts/kaniop`, `make lint`, and `make test`.
10. Use the `passless-agent` skill and its required profile checks. Launch the Passless-managed browser, connect through Playwright MCP, log in to `grafana.grigri.cloud`, open `/d/kaniop-dev`, and inspect the rendered dashboard. Never expose credentials, cookies, CDP data, or tokens.
11. Verify that panels load, variables populate, legends are readable, units are correct, no query errors appear, and absent data is explainable by cluster activity rather than an invalid query.

## PromQL rules

- Counters: use `rate(...[$interval])` for throughput and `increase(...[$interval])` for interval totals.
- Histograms: use `histogram_quantile(q, sum by (<labels>, le) (rate(..._bucket[$interval])))`.
- Reconcile metrics: preserve the `controller` filter and grouping where relevant.
- Kubernetes API request metrics: the application emits `endpoint`, but Prometheus may rename it to `exported_endpoint` when the scrape target already has an `endpoint` label. Discover the live label before choosing it.
- Avoid division by zero in mean calculations when the denominator can be absent.
- Use explicit legend formats for multi-label series.

## Acceptance checklist

- Dashboard JSON parses.
- Every metric and label is confirmed in code.
- Changed queries execute successfully through `grafana-grigri`.
- The dev dashboard exists and its stored queries match the repository.
- Helm tests, lint, and tests pass.
- Passless-managed browser verification succeeds against the real grigri cluster.
- The repository dashboard keeps its production UID and contains no environment credentials.
