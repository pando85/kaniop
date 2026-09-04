# Agent system

Kaniop follows an artifact-driven, AI-native development workflow inspired by
Anthropic's AI-Native SDLC playbook and implemented with OpenCode conventions.

## Control layers

| Layer | Location | Purpose |
|---|---|---|
| Repository context | `AGENTS.md` | Short instructions loaded every session |
| Institutional knowledge | `.opencode/skills/` | Detailed, task-triggered workflows |
| Delegation | `.opencode/agents/` | Scoped researcher and verifier roles |
| Deterministic guardrails | `opencode.json`, tests, and CI | Enforced boundaries |
| Intent and decisions | `intent/<work-item>/` | Durable intent, spec, and plan |
| Review policy | `REVIEW.md` | Consistent automated and human review |
| Harness evaluation | `evals/` | Regression cases and configuration results |

Instructions and skills are advisory. OpenCode permissions, tests, CI, branch
protection, and human approvals enforce properties that must always hold.

## Lifecycle

1. Capture non-trivial work in an issue and, when warranted, an
   `intent/<work-item>/intent.md`.
2. Produce and approve `spec.md`.
3. Use OpenCode's Plan agent to inspect the repository and commit `plan.md`.
4. Implement in an isolated branch or worktree.
5. Run the feedback loop until deterministic verification succeeds.
6. Use the `verifier` subagent in a fresh context and review against
   `REVIEW.md`.
7. Open a PR. The authoring agent may address findings but cannot approve or
   merge its own change.
8. Convert escaped defects and valuable corrections into eval cases.

Start manually. Automate an artifact transition only after its inputs, output,
gate, and failure behavior are stable.
