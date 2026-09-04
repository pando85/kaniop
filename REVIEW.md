# Review policy

Run focused passes and rank every finding as Critical, Important, or Nit.
Automated review informs the human code owner; it never approves its own change.

## Pass 1: intent and scope

- The change implements the issue and any committed `intent.md`/`spec.md`.
- The diff contains no unrelated cleanup or speculative refactoring.
- Material deviations from `plan.md` are documented.

## Pass 2: correctness

- Reconciliation remains idempotent and safe under retries.
- Deletion, cancellation, transient API failure, empty-state, and version-skew
  behavior are considered where relevant.
- Tests prove the behavior rather than mirroring implementation details.
- Bug-fix tests fail for the expected reason before the fix.

## Pass 3: Kubernetes and API safety

- Ownership, finalizers, status conditions, watches, and update semantics remain
  correct.
- Destructive recovery is restricted to errors that are known to require it.
- CRD/API compatibility and generated artifacts are handled deliberately.
- Logs, events, metrics, and errors do not expose credentials or sensitive data.

## Pass 4: repository contract

- Matching skills under `.opencode/skills/` were applied.
- `make lint`, `make test`, and relevant specialized checks were run.
- CRDs and examples were regenerated from source rather than hand-edited.
- User-facing behavior has documentation and representative examples.

## Severity

- **Critical:** credible data loss, security exposure, invalid upgrade path, or
  broadly destructive behavior.
- **Important:** incorrect behavior, missing required verification, material
  regression, or policy violation.
- **Nit:** local improvement that does not affect correctness or risk.

Report no more than five nits. Skip generated files and formatting already
enforced by CI. Every finding must identify a concrete failure mode and evidence.
