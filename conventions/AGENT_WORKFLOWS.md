# Agent workflows

## Discovery and sources of truth

- `AGENTS.md` routes repository rules; nested `AGENTS.md` files apply to their subtrees. Their sibling `CLAUDE.md` files import `@AGENTS.md` so both clients receive the same rules.
- `skills/<name>/SKILL.md` owns each workflow. `.agents/skills/<name>/SKILL.md` exposes it to Codex; `.claude/skills/<name>/SKILL.md` exposes it to Claude Code. Loaders keep matching names and descriptions and link to the canonical file. Do not copy workflow bodies into loaders.
- Keep UI metadata in canonical `skills/<name>/agents/openai.yaml` and an identical copy beside the Codex loader. Keep automatic skill selection enabled unless explicitly requested otherwise.
- `.codex/agents/<role>.toml`, `.claude/agents/<role>.md`, and `.cursor/agents/<role>.md` expose the roles below. Keep their names, descriptions, and shared role reference aligned. Codex and Claude Code inherit the session model. Cursor pins the parent chat to `grok-4.7[reasoning_effort=high,context=500k,fast=false]`. `explorer` is `composer-2.5[fast=false]`. `worker`, `reviewer`, and `async-safety-reviewer` are `grok-4.7[reasoning_effort=medium,context=256k,fast=false]`. Launch a Cursor role with `subagent_type` and no `model` argument. Read-only Cursor roles set `readonly: true`. Do not encode dated prices or model rankings.
- Antigravity (the `agy` CLI and the desktop app) reads root `AGENTS.md` and `.agents/skills/`, but not nested `AGENTS.md` files below its start folder, `.claude/rules/` or `.claude/agents/`. `cargo agent-check sync-antigravity` generates their Antigravity form: `.agents/agents/<role>.md` (each role's description, prompt and tools), `.agents/rules/folder-*.md` (glob rules that inline each nested `AGENTS.md`) and `.agents/rules/claude-*.md` (glob rules that repeat each `.claude/rules/` file). Never edit them by hand; `cargo agent-check` reports them when stale. Roles run on their own Gemini models; see Antigravity below.
- Use `skills/foxy-agent-maintenance/SKILL.md` when changing this setup. Run `cargo agent-check` from the repository root to check its structure without launching Foxy.

## Delegation

Delegate independent work when separate context or parallel investigation improves the result. Keep small, sequential, or overlapping tasks in the main agent. A search and a short file read do not need a child agent. Follow session limits; reuse an existing agent for a related follow-up and avoid nested delegation by default.

Start children with a self-contained brief rather than the full conversation when the client supports it. Include:

```text
Role and goal: one concrete question or bounded change.
Scope: allowed files; write ownership or read-only; relevant diff/base revision.
Context: relevant nested AGENTS.md, convention sections, symbols, and invariants.
Contract: observable outcome and exclusions; behavior or test expectations that must stay fixed.
Evidence: file:line plus failure input, callers, or command results as appropriate.
Validation: checks allowed here; checks reserved for the main agent.
Stop: return once answered; report a blocker rather than expanding scope.
```

Workers share the checkout. Assign non-overlapping files and preserve existing user changes. Schema, dependencies, lockfiles, shared module wiring, formatting across crates, and mutable runtime state stay with the main agent. Child agents do not run workspace builds, testkit cases, the GUI, Steam, or network integrations unless their brief explicitly assigns an authorized isolated check.

Ownership is explicit: naming another worker or branch is context, not permission to edit, import, commit or integrate its work. Ask the main coordinator to resolve overlap; never annex a peer's files or extend a brief unilaterally. The coordinator alone changes assignments and integrates returned work.

Main-agent handoff: inspect the combined diff, verify consequential findings against source, resolve contradictions, and run validation once after integration. Distinguish passed checks, checks not run, and existing failures. A successful narrow test does not establish workspace or runtime coverage.

## Review scope and stopping

For consequential Rust changes, prefer a fresh-context reviewer given the contract, exact changed files or diff, relevant callers and evidence limits. Keep the implementer's proposed conclusion out of the initial brief. In a dirty checkout, HEAD alone may include earlier milestones and miss untracked files; identify this task's actual scope. Reviewers read assigned files and callers, without treating adjacent unfinished work as part of the change.

Use one focused independent review when it adds value. Add an async-safety or second review for a different unresolved risk, such as native handle lifetime, persistent recovery or an unsafe boundary; do not prescribe reviewer counts or models. Ordinary small edits need no agent fleet. Compilation and safe Rust do not prove event order, serialized contracts, host assumptions or the absence of missing behavior.

Validate each finding against the actual path before fixing it. Report the trigger, violated contract and consequence; separate a demonstrated defect from a hypothesis or missing evidence. After fixes, review the changed path and affected callers. Repeat a whole review only for a new cross-cutting change or unresolved concern. Once acceptance, applicable checks and material findings are resolved, stop rather than seeking repeated clean verdicts.

For test or acceptance changes use [test oracle review](../skills/foxy-test-oracle-review/SKILL.md); for multi-step delivery use [milestone execution](../skills/foxy-milestone-execution/SKILL.md). Promote a repeated failure into the narrowest existing workflow when it will change a future decision, not into another universal checklist.

## Roles

| Role | Scope | Result |
| --- | --- | --- |
| `explorer` | Read-only source mapping for one question | Relevant callers, state flow, invariants, and file:line evidence |
| `worker` | Implementation in explicitly owned files | Focused changes, relevant tests, and integration needs |
| `reviewer` | Read-only review of a specified diff | Concrete defects ordered by severity, with input and consequence |
| `async-safety-reviewer` | Read-only review of async/task and shared-state changes | Blocking, cancellation, lock, queue, and ownership failures |

### explorer

Read the relevant instruction files and use `rg` to find the symbol and nearest `mod.rs`. Trace the code path only far enough to answer the brief. Include callers and tests when they affect the answer. Do not edit, build, launch, or infer behavior from names alone. Mark missing evidence explicitly.

### worker

Read the applicable instructions and `skills/foxy-rust-change/SKILL.md`. Edit only the assigned files. Add focused tests for changed pure behavior, preserving repository-instance identity, game-space ownership, and existing public contracts as applicable. Report required edits outside ownership instead of making them. Leave shared validation and module/dependency integration to the main agent unless explicitly assigned.

### reviewer

Start from the assigned diff and inspect relevant callers, contracts, tests, and nested instructions. Prioritize observable bugs, data loss, unsafe filesystem/network behavior, UI/CLI contract mismatches, and missing regression coverage. Check docs/examples when behavior changes. Give each finding a file:line, triggering input or scenario, and concrete consequence. Skip speculative style advice. State when no defects are found and what was not exercised. Do not edit or run builds.

For Rust boundaries, check that types preserve external semantics such as serialized numbers, absent versus empty values and error categories. Trace paired effects through both durable state and the active UI/owner projection. Check when context, environment and working paths are captured across waits; memory safety alone cannot establish those contracts. Use the async-safety role for a separate unresolved concurrency/lifetime question rather than duplicating the same review.

### async-safety-reviewer

Load [core conventions](CORE_CONVENTIONS.md) and `src/core/AGENTS.md`; add [sync guardrails](SYNC_ALGO_CONVENTION.md) for sync changes and [UI conventions](UI_CONVENTIONS.md) for egui task dispatch. Trace ownership across each changed `.await`, task spawn, cancellation branch, and result delivery.

- Identify blocking I/O, hashing, parsing, and long loops on the async executor or UI thread. Use existing bounded workers. `spawn_blocking` work keeps running once started even when its awaiting future is cancelled, so review cooperative cancellation and cleanup inside the operation.
- Check guard lifetime and lock ordering. A short synchronous mutex can be appropriate when released before `.await`; an async mutex does not fix a long critical section or nested lock cycle.
- Check bounded queues, concurrency gates, backpressure, timeout behavior, and handling of task errors. A timeout that drops a future does not roll back filesystem or database writes.
- Trace shutdown, partial downloads, patch fallback, and task completions after a game-space switch. Verify the operation's space/context stays attached to its result and stale results cannot mutate the active space.
- Respect the database process claim and Turso seam. Verify transaction/gate ownership and `(remote_url, local_path)` scoping; never open runtime databases to inspect a review.

Report concrete failure scenarios with file:line evidence. Do not edit or run builds or live cases.

## Antigravity

Pick the main session's model and thinking level in the app's model selector or with `agy --model <id>`: `gemini-4-hydrogen-high` for routine work, `gemini-4-argon-high` for hard, long-horizon work. Each role runs on its own model and level, listed in `ROLE_MODELS` in `tools/agent-check/src/antigravity.rs`:

| Role | Gemini model |
| --- | --- |
| `explorer` | `gemini-3.8-flash-medium` |
| `worker` | `gemini-4-hydrogen-medium` |
| `reviewer` | `gemini-4-hydrogen-high` |
| `async-safety-reviewer` | `gemini-4-hydrogen-high` |

To delegate, the main agent runs the role in the terminal, from the app or the CLI, and reads the printed answer:

```text
cargo agent-check role --list
cargo agent-check role explorer "<brief>"
cargo agent-check role reviewer --brief-file <path>
cargo agent-check role reviewer --argon --brief-file <path>
```

`--argon` runs the role on `gemini-4-argon-high` for one hard review. `cargo agent-check models` reports whether these pins are still offered; [agent model check](../skills/foxy-agent-model-check/SKILL.md) decides, conservatively, whether one changes. The delegation contract above applies unchanged. Antigravity's built-in subagent tool is not used: a subagent file cannot name a model or a thinking level, only a coarse class that Google maps on its servers. A role started as `agy --agent <role> --model <id>` runs exactly that model and level, so the generated role files set `subagent: false`.

A headless session cannot ask for permission. The launcher passes session-only rules with `toolPermission: request-review`, so an action outside them is refused even when the owner runs Antigravity without prompts:

- every role reads the checkout; `worker` also writes it and runs commands;
- read-only roles may run read-only `git` and `rg`, also when a global RTK hook rewrote them to `rtk <command>`;
- every role is denied git commands that change history or the branch, `cargo run`, `cargo prerelease`, running a built `Foxy` and writes to `.git`;
- when RTK is installed, its config and data folders are readable and writable so it works inside the role's sandbox.

A refused action is listed after the answer and can end the role's turn early. Antigravity loads `.agents/` only in a folder it trusts, and it trusts folders by exact path; a parent folder does not cover the folders inside it.

## Structural validation

`cargo agent-check` is a read-only Rust helper in `tools/agent-check/`, with its own workspace and lockfile. It dynamically checks all canonical skills and their references: metadata, discovery parity, loader targets, catalog coverage, UI metadata, role pairs, instruction imports, local Markdown file links, text rules, and whether the generated Antigravity files match their sources and every role has a Gemini model. It does not prove skill quality, enforce shell permissions, validate every prose path or link anchor, or exercise Foxy.

Claude roles omit edit tools for read-only work, but shell access still permits writes. Codex custom-agent instructions also inherit session permissions. These definitions express task boundaries; they do not create a separate sandbox or authorize external actions.

## Primary references

- [Bun's Rust rewrite](https://bun.com/blog/bun-in-rust) and [GitHub's Rust runtime migration lessons](https://github.blog/ai-and-ml/migrating-the-github-copilot-runtime-to-rust-using-copilot/#h-lessons-learned) are case studies informing independent review, preserved behavioral evidence and attention to iteration cost. Apply these ideas proportionately to Foxy's incremental work and existing authorization boundaries.
- [Codex skill discovery](https://learn.chatgpt.com/docs/build-skills) and [custom subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents).
- [Claude Code skills](https://code.claude.com/docs/en/skills) and [custom subagents](https://code.claude.com/docs/en/sub-agents).
- [Cursor subagents](https://cursor.com/docs/subagents) and [Grok 4.7](https://cursor.com/docs/models/grok-4-7).
- Antigravity [models](https://antigravity.google/docs/models/), [custom subagents](https://antigravity.google/docs/subagents/), [rules](https://antigravity.google/docs/rules), [permissions](https://antigravity.google/docs/permissions/) and [CLI](https://antigravity.google/docs/cli/reference).
- [Tokio shared-state guidance](https://tokio.rs/tokio/tutorial/shared-state) and [blocking-task cancellation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).
