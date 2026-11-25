---
name: release
description: Prepare and publish a new release. Use when the user asks to release, cut a release, or publish a new version.
---

## Purpose

Release a new version of Kaniop using the release script and CI pipeline.

## When to use

Use this skill when:
- The user asks to release a new version
- The user asks to cut a release or publish
- The user asks to tag a new version

## Prerequisites

Before releasing, verify:

1. **Working tree is clean** — no uncommitted changes
2. **You are on `master`** — releases only happen from master
3. **Local master is up to date with origin/master**
4. **No commits ahead of origin/master** (output of `git rev-list --count origin/master..HEAD` must be 0)
5. **Repository is not a shallow clone** — git-cliff needs full history for accurate changelogs

Check with:
```bash
git status --short
git rev-parse --abbrev-ref HEAD
git pull origin master
git rev-list --count origin/master..HEAD
git tag --sort=-creatordate | head -3
git log --oneline v<latest_tag>..HEAD
```

## Version Decision Guide

Use Semantic Versioning (MAJOR.MINOR.PATCH). Determine the bump type by analyzing commits since the last release.

### Major Version (X.0.0)

Bump MAJOR when:
- Breaking changes to CRD specs
- Breaking changes to Helm chart values
- Breaking changes to operator behavior requiring manual intervention
- Commit message contains `BREAKING CHANGE:` or `!` (e.g., `feat!: ...`)

### Minor Version (0.X.0)

Bump MINOR when:
- New features added (`feat:` commits)
- New CRD types or fields
- New Helm chart configuration options
- Backward-compatible enhancements
- **Kanidm upgraded to the next minor version** — Kaniop tracks Kanidm's minor version; a Kanidm minor bump always triggers a Kaniop minor release

### Patch Version (0.0.X)

Bump PATCH when:
- Bug fixes (`fix:` commits)
- Documentation updates (`docs:` commits)
- Internal refactoring (`refactor:` commits)
- Dependency updates (`chore(deps):` commits)

### Decision Process

1. Run: `git log v<CURRENT_VERSION>..HEAD --oneline`
2. Check commit messages for:
   - `!` or `BREAKING CHANGE:` -> MAJOR
   - `feat:` -> MINOR
   - `fix:`, `docs:`, `refactor:`, etc. -> PATCH
3. If multiple types, use the highest precedence (MAJOR > MINOR > PATCH)

## Release Process

### Step 1: Verify Clean State

Ensure you're on master with no uncommitted changes, up to date with origin, and no commits ahead:

```bash
git checkout master
git pull origin master
git status  # Should show "nothing to commit, working tree clean"
```

Verify no commits ahead of origin/master:

```bash
git rev-list --count origin/master..HEAD  # Should output 0
```

**Unshallow check** — shallow clones produce incomplete changelogs:

```bash
git rev-parse --is-shallow-repository
```

If this outputs `true`, unshallow the repo before proceeding:

```bash
git fetch --unshallow origin
git fetch --tags origin
```

### Step 2: Determine Version

1. Get current version:
   ```bash
   grep '^version =' Cargo.toml | head -1
   ```

2. Review commits since last release:
   ```bash
   git log v<CURRENT_VERSION>..HEAD --oneline
   ```

3. Decide on MAJOR, MINOR, or PATCH bump based on the Version Decision Guide above.

### Step 3: Create Release Branch

```bash
git checkout -b release/v<NEW_VERSION>
```

### Step 4: Update Version

Edit the root `Cargo.toml` and update the version in the `[workspace.package]` section:

```toml
version = "<NEW_VERSION>"
```

Then propagate the version across all workspace files:

```bash
make update-version
```

This automatically updates:
- All workspace dependency versions in root `Cargo.toml`
- `Cargo.lock`
- `charts/kaniop/Chart.yaml` (version, appVersion, image tags, annotations)
- Artifact Hub changes annotation (via git-cliff with `.ci/cliff-chart.toml`)

**Important**: After running `make update-version`, verify the `artifacthub.io/changes` annotation in `charts/kaniop/Chart.yaml` includes any chart-scoped commits (e.g., `feat(chart):`, `fix(chart):`). If it shows "No changes in the chart" but chart commits exist, see [Troubleshooting](#incorrect-artifacthubiochanges-annotation).

### Step 5: Update Changelog

Generate the changelog using git-cliff:

```bash
make update-changelog
```

This runs: `git cliff -t v<VERSION> -u -p CHANGELOG.md`

The changelog is generated from conventional commits and grouped by type (Added, Fixed, Documentation, Build, Refactor, Styling, Testing, Chore). Release commits are excluded.

### Step 6: Regenerate Examples

If CRD fields were changed in this release cycle:

```bash
make examples
```

### Step 7: Commit Changes

Stage and commit all changes:

```bash
git add .
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
git commit -m "release: Version $VERSION"
```

### Step 8: Push Branch and Create PR

```bash
git push -u origin release/v<NEW_VERSION>
```

Create a pull request to merge into master.

### Step 9: After Merge

After the PR is merged to master, tagging and releasing is done **automatically** by CI:

1. `auto-tag.yml` reads the new version from `CHANGELOG.md`
2. Creates a signed GPG tag `v<VERSION>`
3. Pushes the tag to GitHub

The tag then triggers:
- `docker_images.yml`: Builds/pushes multi-arch Docker images (amd64, arm64), creates GitHub Release
- `rust.yml`: Publishes crates to crates.io
- `helm.yml`: Publishes signed Helm chart to GHCR OCI registry

Monitor with:
```bash
gh run list --limit 5
```

**Note:** Do not manually create tags — CI handles this automatically.

## What NOT to Do

| Mistake | Why it's wrong | Fix |
|---------|---------------|-----|
| Manually editing CHANGELOG.md | git-cliff generates it from conventional commits | Use `make update-changelog` |
| Manually editing Chart.yaml version | `make update-version` handles all of it | Use the Makefile target |
| Creating git tags manually | `auto-tag.yml` creates signed tags automatically | Just push to master |
| Releasing from a feature branch | Changelog generation needs master commit IDs | Checkout master first |
| Releasing with dirty working tree | Script will fail or produce incomplete release | Commit or stash changes first |
| Skipping the unshallow check | Shallow clones produce incomplete changelogs | Always check and unshallow if needed |
| Forgetting `make update-version` after Cargo.toml edit | Version won't propagate to Chart.yaml, lock file, etc. | Always run `make update-version` |

## Troubleshooting

### "There are commits ahead of origin/master"
Merge or push them first before starting the release.

### Shallow clone detected
```bash
git fetch --unshallow origin
git fetch --tags origin
```

### git-cliff not installed
```bash
cargo install git-cliff
```

### Auto-tag workflow didn't trigger
Ensure:
- The CHANGELOG.md has a new version entry as the first `## [v...]` heading
- The tag doesn't already exist: `git tag -l | grep <version>`
- The `PAT` and `GPG_PRIVATE_KEY` secrets are configured in GitHub

### Version not propagating to Chart.yaml
Ensure `make update-version` was run after editing `Cargo.toml`. The version is read from the root `Cargo.toml` `[workspace.package]` section.

### Incorrect `artifacthub.io/changes` annotation
If the annotation shows "No changes in the chart for this Kaniop version" but chart commits exist:
1. The Makefile now fails on shallow clones — ensure `git rev-parse --is-shallow-repository` returns `false`
2. Verify tags are present: `git tag --sort=-creatordate | head -3`
3. If tags are missing: `git fetch --tags origin`
4. Re-run `make update-version` and verify the annotation includes chart commits
5. The Makefile will now show git-cliff errors instead of silently falling back

## Manual Image Rebuild (CI publish failure)

Use when a tag exists but its images never reached ghcr (e.g. `v0.25.0`: the
`docker_images.yml` run failed `.ci/check-x86-64-baseline.sh` with SIGILL, so
`ghcr.io/pando85/*:0.25.0` was never published). Diagnose first:

```bash
gh run list --workflow docker_images.yml --limit 5
gh run view <run-id> --log-failed | grep -E "FAIL|SIGILL|##\[error\]"
```

### Known CI gap (hypothesis, unverified)

ubuntu-26.04's GCC defaults to `-march=x86-64-v3`. The workflow sets
`CFLAGS_x86_64_unknown_linux_gnu=-march=x86-64`, which only cc-rs-style builds read;
cmake-based C dependencies read plain `CFLAGS`. If the baseline check keeps failing
in CI while local baseline builds pass, try adding plain `CFLAGS: -march=x86-64` to
the workflow env as well.

### Rebuild and publish from the tag

Image tags strip the `v` prefix (`v0.25.0` → `0.25.0`). Run from a clean checkout of
the tag; return to master afterwards.

```bash
git fetch --tags && git checkout v<VERSION>

# x86_64: baseline CFLAGS, exactly as CI sets them
env -u CARGO_TARGET_DIR CFLAGS_x86_64_unknown_linux_gnu=-march=x86-64 make release
.ci/check-x86-64-baseline.sh target-x86_64-unknown-linux-gnu/x86_64-unknown-linux-gnu/release

# arm64 (requires cross + qemu binfmt)
env -u CARGO_TARGET_DIR make release CARGO_TARGET=aarch64-unknown-linux-gnu

# images (amd64 local; arm64 via buildx --platform linux/arm64 --load)
env -u CARGO_TARGET_DIR CFLAGS_x86_64_unknown_linux_gnu=-march=x86-64 make images VERSION=<VERSION>
```

`env -u CARGO_TARGET_DIR` matters on hosts that export a global absolute
`CARGO_TARGET_DIR`: the Makefile's `?=` keeps the environment value, `cross` refuses
absolute target dirs, and the Dockerfile's `COPY ${CARGO_TARGET_DIR}/…` cannot reach
outside the build context. The same applies to `make push-images`.

Before pushing, verify the images contain exactly the tested binaries:

```bash
docker run --rm --entrypoint sha256sum ghcr.io/pando85/kaniop:<VERSION> /bin/kaniop /bin/kaniop-crd-migrator
sha256sum target-x86_64-unknown-linux-gnu/x86_64-unknown-linux-gnu/release/{kaniop,kaniop-crd-migrator}
docker run --rm --platform linux/arm64 ghcr.io/pando85/kaniop:<VERSION>-arm64 --version
```

Then either `make push-images VERSION=<VERSION>` (CI path, rebuilds and pushes by
digest) or push the verified per-arch images and assemble the manifest from their
pushed digests with `docker buildx imagetools create -t …:0.25.0 …@<amd64-digest>
…@<arm64-digest>`. Verify with `docker buildx imagetools inspect …:0.25.0` (both
platforms listed).

### Verification semantics

Release binaries legitimately **contain** AVX2/AVX512 instructions: AWS-LC and
OpenSSL assembly fast paths selected at runtime from CPUID. "Runs on any x86-64"
means the qemu baseline check passes, not that the disassembly is free of newer
ISAs. For extra confidence sweep CPU models:

```bash
for cpu in qemu64 Nehalem Haswell Skylake-Client Cascadelake-Server; do
  qemu-x86_64-static -cpu $cpu ./kaniop --version || echo "FAIL $cpu"
done
```

### Manual-rebuild pitfalls

| Pitfall | Fix |
|---------|-----|
| Global absolute `CARGO_TARGET_DIR` in the host environment | `env -u CARGO_TARGET_DIR make …` so the Makefile default applies |
| Host `~/.cargo/config.toml` sets `rustc-wrapper = "sccache"`; cross containers lack sccache | `CARGO_BUILD_RUSTC_WRAPPER=env` (transparent wrapper) and whitelist it via `[build.env] passthrough` in a `Cross.toml` referenced by `CROSS_CONFIG` (kept outside the repo); an empty value does not disable the config |
| Host cargo `jobs = 2` throttles release builds | Temporary `CARGO_BUILD_JOBS=<n>` override |
| ghcr push denied with a fine-grained PAT or the default `gh` token | ghcr push needs `write:packages`: `gh auth refresh -h github.com -s write:packages`, then `gh auth token \| docker login ghcr.io -u pando85 --password-stdin` |
| Deleting per-arch child versions after `imagetools create` | Don't — the index references the child manifests; leave `*-amd64`/`*-arm64` tags in place |

## Key Files

| File | Role |
|------|------|
| `Cargo.toml` | Root workspace version (single source of truth) |
| `cliff.toml` | git-cliff configuration for main changelog |
| `.ci/cliff-chart.toml` | git-cliff configuration for Artifact Hub chart changes |
| `CHANGELOG.md` | Generated changelog (auto-tag reads version from here) |
| `charts/kaniop/Chart.yaml` | Helm chart version, appVersion, image tags, annotations |
| `.github/workflows/auto-tag.yml` | Creates signed tag on push to master |
| `.github/workflows/docker_images.yml` | Builds/pushes Docker images and creates GitHub Release |
| `.github/workflows/rust.yml` | Publishes crates to crates.io on tag |
| `.github/workflows/helm.yml` | Publishes Helm chart to GHCR on tag |
| `Makefile` | `update-version`, `update-changelog`, and `release` targets |

## Checklist

- [ ] On master branch, clean working tree
- [ ] Pulled latest from origin/master
- [ ] No commits ahead of origin/master
- [ ] Repository is not a shallow clone (or has been unshallowed)
- [ ] Determined version bump type (MAJOR/MINOR/PATCH)
- [ ] Created release branch `release/v<VERSION>`
- [ ] Updated version in root `Cargo.toml`
- [ ] Ran `make update-version`
- [ ] Ran `make update-changelog`
- [ ] Ran `make examples` (if CRD fields changed)
- [ ] Committed with message `release: Version <VERSION>`
- [ ] Pushed and created PR
- [ ] After merge: CI automatically creates tag and release

## Quick Reference

| Step | Command |
|------|---------|
| Check current version | `grep '^version =' Cargo.toml \| head -1` |
| View recent commits | `git log v<CUR>..HEAD --oneline` |
| Check commits ahead | `git rev-list --count origin/master..HEAD` |
| Check shallow clone | `git rev-parse --is-shallow-repository` |
| Unshallow repo | `git fetch --unshallow origin && git fetch --tags origin` |
| Propagate version | `make update-version` |
| Update changelog | `make update-changelog` |
| Regenerate examples | `make examples` |
| Commit | `git commit -m "release: Version <VER>"` |
| Monitor CI | `gh run list --limit 5` |
