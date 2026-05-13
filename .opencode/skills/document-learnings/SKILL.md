---
name: document-learnings
description: Guidelines for documenting reusable patterns discovered during work. Use when completing tasks that reveal non-obvious workflows or project conventions.
---

## Purpose

Capture reusable knowledge so future sessions avoid repeated debugging and rediscovery.

## When to Document

- After discovering a required workflow with multiple mandatory steps
- After fixing a non-obvious bug or uncovering a subtle constraint
- After establishing a new pattern or convention
- After the user corrects the model's approach — this reveals the user's preferred way of working
- When the user asks to document learnings

## When NOT to Document

- Obvious or standard practices
- One-off debugging that won't recur
- Information already well-covered in existing docs

## Where to Put Documentation

| Scope | Location |
|-------|----------|
| Project-specific workflows | `.opencode/skills/<name>/SKILL.md` |
| Short reference needed every invocation | `AGENTS.md` |

## Skill File Format

```
---
name: skill-name
description: Short description. Use when ...
---

## Purpose
What this skill covers.

## Instructions
### 1. Step One
Specific, actionable steps with code examples.

## Common Pitfalls
| Pitfall | Fix |
|---------|-----|
| ... | ... |
```

## Tips

- Keep skill descriptions actionable
- Include code examples and common pitfalls tables
- Commit skills to the repo so the whole team benefits
- Pay special attention when the user corrects or refines the model's output — that feedback encodes the user's standards and preferences for this project
- Document the "why" behind conventions, not just the "what" — e.g., why a checklist exists, what goes wrong if a step is skipped
