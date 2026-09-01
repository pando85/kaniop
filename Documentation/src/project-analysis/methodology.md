# Project Analysis Methodology

This document defines a reproducible process for analysing Kaniop's history and current project health. Reports should prefer primary evidence, preserve uncertainty, and separate measured facts from interpretation.

## Analysis contract

Every report must record:

- the analysis date and time in UTC;
- the Git ref or commit SHA used for repository content;
- the latest Kaniop release/tag considered;
- the source and query used for mutable GitHub metrics;
- any data that could not be collected directly.

For each claim, distinguish three classes:

1. **Measured**: directly obtained from repository files or APIs.
2. **Derived**: calculated from measured values, for example a percentage or release interval.
3. **Interpreted**: a maturity or project-health conclusion based on the evidence.

Do not turn an interpretation into a fact just because it is repeated in later reports.

## Source precedence

Use sources in this order whenever they disagree:

1. repository state at the exact Git ref being analysed;
2. GitHub REST/API metadata for repository activity;
3. Kaniop release notes, issues, pull requests, and commit history;
4. upstream Kanidm documentation and release metadata;
5. upstream Operator Framework documentation for capability-level definitions;
6. third-party indexes or historical snapshots only when primary data is unavailable.

If a primary source is available, do not substitute a third-party estimate.

## Establish the time boundary

Start by recording the current repository state:

```bash
git rev-parse HEAD
git describe --tags --always

gh api repos/pando85/kaniop \
  --jq '{created_at,updated_at,pushed_at,stargazers_count,forks_count,subscribers_count,open_issues_count}'
```

Historical reports should use tags or immutable commit SHAs rather than the moving `master` branch.

## Kanidm compatibility

### Effective SDK version

The effective Kanidm SDK version is the version resolved in `Cargo.lock`, not the semver requirement written in `Cargo.toml`.

For example, a manifest dependency such as:

```toml
kanidm_client = "1.10.0"
```

is normally a compatible Cargo requirement and can resolve to a later `1.x` release. Kaniop also intentionally reads the resolved `kanidm_client` version from `Cargo.lock` at build time for its runtime version compatibility logic.

Therefore, for every historical point:

```bash
git show <ref>:Cargo.lock
```

and extract the package block for `kanidm_client`.

A useful shell helper is:

```bash
git show "$REF:Cargo.lock" | awk '
  $0 == "name = \"kanidm_client\"" { found=1; next }
  found && /^version = / { print; exit }
'
```

Record at least these four values separately:

- **manifest requirement**: the requirement in `Cargo.toml`;
- **resolved SDK version**: `kanidm_client` in `Cargo.lock`;
- **default/tested server version**: what e2e tests derive and deploy;
- **explicit upgrade path**: the source and destination versions exercised by upgrade e2e tests.

Do not collapse these into one ambiguous "supported Kanidm version" value.

### Upgrade support

Inspect:

- `libs/operator/build.rs`;
- `libs/operator/src/version.rs`;
- Kanidm reconciliation/version status code;
- `tests/e2e/test/kanidm/upgrade.rs`;
- upstream Kanidm upgrade policy.

Report what is **tested**, what is **accepted by Kaniop's guard**, and what upstream Kanidm **supports** as separate concepts.

## Release history

Use Git tags/releases as the event source. Record:

- first beta;
- first non-beta release;
- minor release dates;
- patch-release bursts;
- major capability milestones.

Useful derived metrics include:

- days from repository creation to first non-beta release;
- mean and median time between minor releases;
- releases per 90 days;
- immediate patch ratio after feature releases;
- time from upstream Kanidm release to Kaniop adoption.

Release velocity is not automatically maturity. A burst of releases can indicate either productive iteration or stabilization after regressions; inspect the content.

## Operator capability maturity

Use the Operator Framework capability model as a common vocabulary:

1. Basic Install
2. Seamless Upgrades
3. Full Lifecycle
4. Deep Insights
5. Auto Pilot

Track two different assessments:

- **feature coverage**: capabilities implemented in code;
- **production-qualified cumulative level**: the highest cumulative level whose required behavior is considered production-supported.

This distinction matters. For example, observability features can resemble Level IV while a Level III backup/restore subsystem is still explicitly experimental. In that case, do not simply label the operator "Level IV".

Evidence should come from implementation, e2e tests, user-facing documentation, and current support statements.

## Testing and operational-safety maturity

Track testing as a first-class project capability rather than only counting test files.

Look for:

- e2e coverage existing before or alongside feature releases;
- N-1 upgrade tests;
- replication validation before and after upgrades;
- migration idempotency and failure injection;
- preservation of identity UUIDs and user data;
- Helm and GitOps upgrade paths;
- backup/restore recovery tests;
- e2e shard count and architecture matrix;
- CI changes made to keep the suite scalable.

A growing test infrastructure can be a stronger maturity signal than lines of code or stars.

## Community contribution

Do not equate "contributor" with "person who authored a merged PR".

Measure at least these contribution classes separately:

| Class | Examples |
|---|---|
| Implementation | Human-authored PRs and commits |
| Production knowledge | Reproductions, logs, metrics, scale data, failure timelines |
| Design input | Issues exposing missing abstractions or lifecycle requirements |
| Integration knowledge | cert-manager, Cilium, Gateway API, Argo CD, Flux, PSA, runtime classes |
| Validation | Testing fixes, confirming upgrades, failure reproduction |
| AI/agent implementation | Forkline, Claude, Codex or other explicitly attributable agents |
| Maintenance automation | Renovate and release/dependency bots |

### Issue counts

Use `is:issue` so pull requests are not mixed into the result:

```bash
gh api -X GET search/issues \
  -f q='repo:pando85/kaniop is:issue'

gh api -X GET search/issues \
  -f q='repo:pando85/kaniop is:issue author:pando85'

gh api -X GET search/issues \
  -f q='repo:pando85/kaniop is:issue -author:pando85 -author:app/renovate'
```

Counts alone are insufficient. Inspect issue bodies and discussions for evidence of real deployment knowledge. A report containing a topology, version matrix, logs, latency distribution, failure reproduction, or validated workaround may be a substantial contribution even if the reporter writes no Rust.

Do not use `author_association` alone to decide whether an issue is external or valuable.

### Production-knowledge conversion

For important externally reported issues, link the report to the implementing PR/commit and measure:

- time from issue opening to first maintainer response;
- time to implementation;
- time to release;
- whether the reporter validated the fix;
- whether the report changed architecture, defaults, tests, or only fixed a local bug.

This metric captures how effectively Kaniop turns operational experience into product capability.

## Pull-request attribution

Raw PR totals can be badly misleading when dependency and AI automation are active.

Always split merged PRs into categories such as:

- maintainer;
- external human;
- dependency bot;
- explicit AI/agent bot;
- other automation;
- mixed/co-authored work.

Example total query:

```bash
gh api -X GET search/issues \
  -f q='repo:pando85/kaniop is:pr is:merged'
```

Use author identities, `performed_via_github_app`, commit authors/co-authors, and PR descriptions as evidence. Document the exact attribution rule used for a report.

### AI attribution

Prefer conservative, explicitly observable metrics:

- PRs authored by a known agent bot;
- commits with an explicit AI co-author trailer;
- PRs or issues performed via a named GitHub App;
- repository instructions specifically created for AI coding agents.

Do **not** claim that a percentage of source code is AI-generated unless there is a reliable provenance system that supports that claim. Explicit agent-authored PR share is a defensible lower bound; actual AI-assisted development may be larger.

## Stargazers

Use GitHub's stargazer API with timestamps. Do not reconstruct star history from search-engine snippets or third-party ranking sites when direct data is available.

```bash
gh api --paginate \
  -H 'Accept: application/vnd.github.star+json' \
  repos/pando85/kaniop/stargazers
```

Each event includes `starred_at`. Store or aggregate these timestamps to derive:

- cumulative stars by day/month;
- new stars per month;
- 30/90-day star velocity;
- time to 10, 25, 50, 100, ... stars;
- stars at each major release;
- changes in velocity around important milestones.

If timestamped stargazer data cannot be accessed during a report run, mark the historical series as unavailable. Never silently replace it with approximate third-party values.

Also record forks and actual subscribers (`subscribers_count`) separately from the repository's legacy `watchers_count`, which mirrors stars.

## Architecture growth

Compare workspace structure at meaningful historical refs rather than using lines of code alone.

Useful measures include:

- Cargo workspace member count;
- number and type of controllers/CRDs;
- operational subsystems added, such as migration, backup, data movement, or leader election;
- number of e2e shards;
- architecture/platform matrix;
- number of generated/user-facing APIs.

Interpret whether growth adds domain breadth or operational depth.

## Historical phases

After collecting the evidence, group development into a small number of phases. Phase boundaries should correspond to changes in the project's engineering problem, for example:

- initial declarative resource management;
- lifecycle and credential management;
- Kubernetes ecosystem integration;
- upgrade/migration safety;
- disaster recovery and operational hardening.

The phases are interpretation. Keep the underlying dates and evidence visible so another reviewer can disagree with the narrative.

## Recommended KPI set

The living report should normally include:

- current Kaniop version and repository age;
- resolved Kanidm SDK and tested upgrade path;
- Kanidm adoption lag;
- Operator Framework feature coverage and production-qualified level;
- release cadence;
- e2e architecture/shard coverage;
- total merged PRs and attribution breakdown;
- external human implementation PRs;
- external issues and production-derived reports;
- production-report-to-fix latency for representative cases;
- explicit AI/agent PR share;
- stars, forks, subscribers, and timestamped star velocity;
- breaking CRD/API migrations and whether they have migration e2e coverage;
- backup/restore production gates;
- major current risks.

## Snapshot policy

A dated snapshot is immutable evidence. After it is merged:

- do not update its metrics merely because the project changed;
- fix factual errors only when necessary;
- add a visible `Correction` note describing what changed and why;
- put new measurements in `current.md` and the next dated snapshot.

The living report may evolve continuously, but every update should retain an `Updated` date and Git ref so the values remain auditable.
