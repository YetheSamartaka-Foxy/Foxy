---
name: foxy-agent-model-check
description: "Check whether the Gemini and Cursor models pinned for Foxy's agent roles are still offered or have newer same-family successors, with cargo agent-check models, and change a pin only on primary evidence. Use when a provider announces or retires a model, a pinned role model fails, or the owner asks whether the agent model setup is current. Conservative: the default outcome is no change."
---

# Foxy agent model check

The role model pins are a cost and behaviour contract: `ROLE_MODELS` in `tools/agent-check/src/antigravity.rs`, the Cursor `model` lines in `.cursor/agents/` with their checker constants in `tools/agent-check/src/roles.rs`, and the tables in [agent workflows](../../conventions/AGENT_WORKFLOWS.md). Codex and Claude Code inherit the session model and pin nothing. This workflow keeps the pins current without churn: reporting is the job, a change is the exception.

## 1. Report

Run `cargo agent-check models` from the repository root. It never edits a file and never calls a model; it compares the pins with the list `agy models` prints for the signed-in account.

- `missing`: a pinned Gemini model is no longer offered. Act on it (steps 2 and 3).
- `candidate`: a newer model of a pinned family, or a new Gemini generation, is listed. Research only (step 2); a candidate alone is no reason to change.
- `unchecked`: Cursor has no local model list, so check its models page only when the owner asks or a Cursor role fails. An `unchecked` Antigravity line usually means `agy` is signed out or not installed.
- No `missing` or `candidate`: report the pins and stop. Do not research further.

## 2. Evidence (only for missing or candidate)

- Primary sources only: the provider's model or changelog page, its CLI's own model list, a staff answer. Blogs, forums and leaderboards are leads, never evidence.
- A candidate qualifies for a pin only when all of these hold: generally available (not preview, experimental or beta); listed for the owner's plan by `agy models`; the same role class as the pin it would replace (the light model for `explorer`, the judgment model for `worker`, `reviewer` and `async-safety-reviewer`); and no higher price class, unless the owner accepts the difference.
- For a missing pin, take the provider's named successor in the same class; if none is named, the nearest same-class model in the list. Never fill a gap with a pricier class.

## 3. Change (rare)

- A forced replacement (the pinned model is gone) proceeds as one focused commit. Any other change waits for the owner's yes.
- Never on this workflow's initiative: raise a thinking level, move a role to a higher class, adopt a preview model, change the main-session recommendation (the owner's choice), switch on benchmark claims, or change a pin "to be safe".
- Gemini 4 Carbon, Claude Fable and Codex `gpt-6-astra` have limited usage and are owner's-choice models. Never move a pin, role or recommendation to one; excluded families are never candidate pins. The main session stays Hydrogen at high for routine work and Neon at high or xhigh for hard work unless the owner chooses otherwise.
- Antigravity: edit `ROLE_MODELS` and the matching table in [agent workflows](../../conventions/AGENT_WORKFLOWS.md). Cursor: edit the `.cursor/agents/` model lines, the constants in `tools/agent-check/src/roles.rs` and the Cursor sentence in that document, following [agent maintenance](../foxy-agent-maintenance/SKILL.md).
- Prove a new Antigravity pin with one bounded run, `cargo agent-check role explorer "<one-line question>"`, and confirm the model in that session's transcript.
- Validate with `cargo agent-check models`, `cargo agent-check` and `git diff --check`; checker code changes also run the commands in `tools/AGENTS.md`.

## 4. Report back

The pin table, each finding with its evidence, and either "no change" or the exact change with its source.
