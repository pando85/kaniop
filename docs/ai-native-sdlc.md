# AI-native software development at Kaniop

This document describes Kaniop's Git-first implementation of an AI-native
software development lifecycle. It follows the architecture in Anthropic's
[AI-Native SDLC Playbook](https://academy.claude.com/courses/ai-native-sdlc-playbook),
but uses OpenCode's native `AGENTS.md`, skills, agents, and permission formats.
It does not require Claude or Claude-specific files.

The objective is not to maximize generated code. It is to make intent,
decisions, verification, and improvement durable enough that humans can govern
faster agent-assisted delivery without lowering engineering standards.

## Design principles

1. **Git is the initial system of record.** Markdown and JSON are sufficient
   until their volume or query needs justify a database or observability tool.
2. **Humans own intent, risk, and approval.** Agents may research, plan,
   implement, test, and review; they may not approve or merge their own work.
3. **Deterministic evidence beats model opinion.** Prefer tests, generated-file
   checks, schemas, permissions, and CI over instructions or LLM judges.
4. **Store decisions and evidence, not hidden reasoning.** Do not commit full
   transcripts or chain of thought.
5. **Change the smallest control that fixes the observed failure.** Repeated
   repository context belongs in `AGENTS.md`; detailed reusable guidance in a
   skill; hard requirements in permissions, tests, or CI; regressions in evals.

These principles adapt the playbook's guidance on
[durable intent](https://academy.claude.com/courses/ai-native-sdlc-playbook/capture-intent),
[plan mode](https://academy.claude.com/courses/ai-native-sdlc-playbook/plan-mode),
[feedback loops](https://academy.claude.com/courses/ai-native-sdlc-playbook/give-claude-a-feedback-loop),
[continuous evals](https://academy.claude.com/courses/ai-native-sdlc-playbook/continuous-evals-in-ci),
and [AI-assisted review](https://academy.claude.com/courses/ai-native-sdlc-playbook/ai-in-the-pr-review-loop).

## Repository architecture

| Concern | Canonical location | Format | Retention |
|---|---|---|---|
| Always-loaded repository rules | `AGENTS.md` | Markdown | Current state |
| Task-specific institutional knowledge | `.opencode/skills/*/SKILL.md` | Markdown + frontmatter | Current state and Git history |
| Scoped specialist roles | `.opencode/agents/*.md` | Markdown + frontmatter | Current state and Git history |
| Tool guardrails | `opencode.json` | JSON | Current state and Git history |
| Intent, design, implementation plan | `intent/<work-item>/` | Markdown | Life of the repository |
| Review criteria | `REVIEW.md` | Markdown | Current state and Git history |
| Regression task corpus | `evals/cases/<case>/` | Markdown + JSON | Life of the repository |
| Observable run evidence | `evals/results/<run>/` | JSON | Keep useful baselines and candidates |
| Improvement decisions | `evals/retrospectives/` | Markdown | Life of the repository |

This mapping follows OpenCode's official formats for
[repository rules](https://opencode.ai/docs/rules/),
[skills](https://opencode.ai/docs/skills/),
[custom agents](https://opencode.ai/docs/agents/), and
[permissions](https://opencode.ai/docs/permissions/).

## Change lifecycle and gates

| Stage | Durable output | Agent responsibility | Human gate |
|---|---|---|---|
| Capture | `intent.md` | Clarify the problem, outcome, constraints, and unknowns | Accept the intended outcome |
| Specify | `spec.md` | Propose behavior, design, compatibility, risks, and acceptance criteria | Accept product and technical design |
| Plan | `plan.md` | Inspect the repository; identify files, sequence, tests, and rollback | Accept implementation approach |
| Implement | Code, tests, generated artifacts | Make the smallest correct change and maintain the plan | None until review |
| Verify | Command results and verifier report | Run feedback loops and independent verification | Assess residual risk |
| Review | PR using `REVIEW.md` | Find concrete correctness and policy failures | Approve or reject |
| Learn | Eval case, skill/rule/check change, or explicit no-action decision | Turn evidence into a proposed system improvement | Approve framework change |

Use all three intent artifacts for ambiguous, cross-component, security,
migration, compatibility, destructive, or difficult-to-rollback changes. A
small, well-specified fix can rely on its issue and PR. The artifact chain is a
loop: discoveries during implementation may update the spec or plan, with the
change made visible to reviewers.

## Feedback and review workflow

For a bug, first add or identify a check that fails for the expected reason.
Then implement, run the narrow check, and expand to the relevant repository
checks. An agent must not modify the acceptance test merely to make its output
pass. The independent `verifier` agent receives the issue, durable artifacts,
diff, and command evidence in a fresh context and reports findings without
editing the solution.

Every substantial PR should therefore contain:

- links to the issue and durable artifacts, when used;
- the implementation summary and any plan deviation;
- exact commands run and their outcomes;
- the verifier's Critical, Important, and Nit findings;
- remaining risks and the human decisions required.

Review is performed in the focused passes defined by `REVIEW.md`: intent and
scope, correctness, Kubernetes/API safety, and repository contract. Automated
review removes mechanical work but does not replace code-owner approval or
branch protection. This retains the separation of duties recommended in the
playbook's [PR review loop](https://academy.claude.com/courses/ai-native-sdlc-playbook/ai-in-the-pr-review-loop).

## What to measure

Metrics answer two different questions and must not be mixed:

- **Product quality:** is Kaniop correct, safe, operable, and maintainable?
- **Agent-system quality:** does the combination of model, OpenCode harness,
  `AGENTS.md`, skills, agents, tools, and permissions produce acceptable work?

Start with the following small scorecard. Each metric is generated from
committed eval results, CI, and PR evidence rather than self-reported by an
agent.

| Metric | Calculation | Evidence source | Interpretation |
|---|---|---|---|
| Eval pass rate | passed cases / executed cases | `evals/results/` | End-to-end task success for a fixed corpus |
| First-pass success | cases passing on attempt 1 / executed cases | `outcome.attempts` | Rework required before acceptance |
| Critical/Important finding rate | findings by severity / executed cases | result evidence and PR review | Residual risk reaching review |
| Regression rate | baseline passes that fail for candidate / baseline passes | paired result sets | Whether an instruction or harness change makes known work worse |
| Skill-selection accuracy | expected skills loaded / expected skills | case metadata and result evidence | Whether institutional knowledge is activated |
| Verification completion | required commands passed / required commands | result command evidence | Whether the feedback loop was actually closed |
| Median duration | median `duration_seconds` for comparable cases | result outcomes | Delivery latency, never a quality substitute |
| Median token use | median input + output tokens | result outcomes | Cost/efficiency, interpreted only beside quality |
| Escaped agent defect rate | agent-attributable defects found after merge / agent-assisted PRs | linked incidents/issues and PR labels | Production effectiveness; define attribution conservatively |

Do not create a composite “agent quality” score. It hides trade-offs. Compare a
candidate with its merge-base using the same repository snapshot, tasks, model,
tools, permissions, and budget. Repeat nondeterministic cases enough to expose
variance. A faster or cheaper candidate is not better if correctness regresses.

## How evaluation data is generated

1. Add a case from a real accepted change, escaped defect, incident, or material
   reviewer correction. Preserve the task and expected observable behavior, not
   the historical implementation.
2. Define deterministic commands, required properties, forbidden shortcuts,
   and expected skills in `case.json`.
3. Run the case in an isolated worktree at a fixed repository commit.
4. Record harness, model, configuration commit, attempts, duration, token usage,
   loaded skills, command outcomes, and review findings in a result JSON.
5. For a framework change, execute both the merge-base and candidate
   configurations and review the delta before merging.
6. Add any newly discovered failure mode to the corpus so it cannot silently
   recur.

Generate the baseline/candidate scorecard from those result files with
`make agent-eval-summary BASELINE=<directory> CANDIDATE=<directory>`.

The initial repository change validates the framework and eval metadata but
does not execute a model in CI. Headless execution should be added only after
the team chooses the production OpenCode invocation, credential boundary,
budget, isolation policy, and initial historical corpus. This avoids building a
dashboard around untrustworthy or non-comparable data.

## Retrospectives and continuous improvement

Run a lightweight monthly retrospective, plus an immediate retrospective after
an agent-caused incident. Generate the scorecard before the meeting and inspect
individual regressions; averages alone are insufficient.

For each repeated or significant failure, classify the control gap:

| Observed problem | Preferred change |
|---|---|
| Task intent was ambiguous | Improve issue or `intent.md` template |
| Repo fact is needed almost every session | Tighten concise `AGENTS.md` |
| Detailed reusable workflow was missing | Add or revise a skill |
| Agent selected the wrong role or lacked independence | Revise agent definition or handoff |
| A prohibited action was possible | Add an OpenCode permission, test, CI rule, or branch policy |
| A known failure recurred | Add or strengthen an eval case |
| Review repeatedly finds the same category | Improve `REVIEW.md` or deterministic verification |
| Evidence shows no systemic pattern | Record a deliberate no-action decision |

Every retrospective ends with owners and measurable follow-ups. Framework
changes are normal PRs: state the hypothesis, show baseline/candidate evidence,
identify regressions, and define rollback. Avoid growing `AGENTS.md` after every
mistake; excessive always-loaded context can reduce adherence and makes changes
harder to evaluate.

## Adoption plan

1. **Foundation:** merge the file structure, templates, permissions, review
   policy, and deterministic validation introduced with this document.
2. **Corpus:** convert 5–10 representative historical issues and reviewer
   corrections into eval cases.
3. **Manual baseline:** run those cases with the actual OpenCode setup and
   commit comparable results.
4. **Headless runner:** automate isolated runs only after credentials, budget,
   timeouts, and cleanup are agreed.
5. **CI comparison:** require baseline/candidate evidence for material changes
   to `AGENTS.md`, skills, agents, permissions, model, or harness.
6. **Production loop:** convert meaningful incidents and repeated review
   findings into new intent or eval artifacts; automate remediation only behind
   explicit risk-based approval gates.

Git files are sufficient at this stage. Move result events to an external store
only when corpus size, concurrent runs, retention, or cross-repository analysis
makes Git review impractical. Even then, keep the case definitions, schemas,
policy, and retrospective decisions versioned in this repository.

## Further sources

- Anthropic, [AI-Native SDLC Playbook](https://academy.claude.com/courses/ai-native-sdlc-playbook)
- Anthropic, [Skills as institutional knowledge](https://academy.claude.com/courses/ai-native-sdlc-playbook/skills-as-institutional-knowledge)
- Anthropic, [Hooks as approval gates](https://academy.claude.com/courses/ai-native-sdlc-playbook/hooks-as-approval-gates)
- Anthropic, [CI/CD integration and deployment](https://academy.claude.com/courses/ai-native-sdlc-playbook/ci-cd-integration-and-deployment)
- Anthropic, [Closing the loop on metrics](https://academy.claude.com/courses/ai-native-sdlc-playbook/closing-the-loop-on-metrics)
- OpenCode, [Rules](https://opencode.ai/docs/rules/)
- OpenCode, [Skills](https://opencode.ai/docs/skills/)
- OpenCode, [Agents](https://opencode.ai/docs/agents/)
- OpenCode, [Permissions](https://opencode.ai/docs/permissions/)
