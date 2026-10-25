---
name: foxy-agent-maintenance
description: "Maintain Foxy's AGENTS.md and CLAUDE.md instructions, shared skills, Codex/Claude discovery loaders, agent roles, and Rust metadata checker. Use when changing the repository's agent setup."
---

# Foxy agent maintenance

Read `skills/AGENTS.md`, [agent workflows](../../conventions/AGENT_WORKFLOWS.md), and the instruction file governing the edited subtree. Maintain current, task-specific guidance; link existing conventions rather than repeating them.

- Put universal rules and routing in root `AGENTS.md`, subtree rules in nested `AGENTS.md`, domain details in `conventions/`, and reusable procedures in `skills/<name>/SKILL.md`. Keep private plans and audit notes in `.local/`.
- When adding an `AGENTS.md`, add a sibling `CLAUDE.md` importing `@AGENTS.md`. Keep the root `CLAUDE.md` import intact. Read applicable nested instructions explicitly when the client has not loaded them.
- For a skill, use a lowercase hyphenated directory/name and YAML frontmatter with a precise `description`. Keep the body at the canonical path. Add matching `.agents/skills/<name>/SKILL.md` and `.claude/skills/<name>/SKILL.md` loaders that link to it, plus canonical `agents/openai.yaml` and its identical Codex-loader copy. List the workflow in `skills/README.md` and the relevant routers.
- For a role, add `.codex/agents/<role>.toml` with `name`, `description`, and `developer_instructions`; add `.claude/agents/<role>.md` with matching name and description. Both load the shared role in `conventions/AGENT_WORKFLOWS.md`. Keep permissions inherited, omit model overrides by default, and reserve writes for explicit worker ownership.
- Helpers belong in Rust under `tools/`, never in Python or shell scripts. Read `tools/AGENTS.md` before changing the checker. Do not add guard hooks, change user settings, or increase permissions as a side effect of writing instructions.
- Keep the catalog categorized and every canonical skill linked. The checker discovers canonical skills and references dynamically; a new workflow needs no hardcoded checker entry. Give specialized skills distinct triggers and reuse foundation workflows for shared mechanics.

From the repository root, run:

```text
cargo agent-check
git diff --check
```

For checker code changes, also run its standalone format, Clippy, and test commands from `tools/AGENTS.md`. Root application builds are unnecessary for instruction-only changes.

The checker validates structure, not agent judgment. Review the descriptions for accidental routing overlap, verify the procedure against actual code/commands, and exercise a realistic bounded task when the workflow materially changes. If using an independent agent for this, give it the request and minimum artifacts without the intended conclusion, and keep the evaluation read-only or isolated.
