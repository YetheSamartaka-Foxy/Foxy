# Agent workflows

## Discovery and sources of truth

- `AGENTS.md` routes repository rules; nested `AGENTS.md` files apply to their subtrees. Their sibling `CLAUDE.md` files import `@AGENTS.md` so both clients receive the same rules.
- `skills/<name>/SKILL.md` owns each workflow. `.agents/skills/<name>/SKILL.md` exposes it to Codex; `.claude/skills/<name>/SKILL.md` exposes it to Claude Code. Loaders keep matching names and descriptions and link to the canonical file. Do not copy workflow bodies into loaders.
- Keep UI metadata in canonical `skills/<name>/agents/openai.yaml` and an identical copy beside the Codex loader. Keep automatic skill selection enabled unless explicitly requested otherwise.
- `.codex/agents/<role>.toml` and `.claude/agents/<role>.md` expose the roles below. Keep their names, descriptions, and shared role reference aligned. Leave model and reasoning settings inherited unless the user requests a specific choice; do not encode dated prices or model rankings.
- Use `skills/foxy-agent-maintenance/SKILL.md` when changing this setup. Run `cargo agent-check` from the repository root to check its structure without launching Foxy.

## Delegation

Delegate independent work when separate context or parallel investigation improves the result. Keep small, sequential, or overlapping tasks in the main agent. A search and a short file read do not need a child agent. Follow session limits; reuse an existing agent for a related follow-up and avoid nested delegation by default.

Start children with a self-contained brief rather than the full conversation when the client supports it. Include:

```text
Role and goal: one concrete question or bounded change.
Scope: allowed files; write ownership or read-only; relevant diff/base revision.
Context: relevant nested AGENTS.md, convention sections, symbols, and invariants.
Evidence: file:line plus failure input, callers, or command results as appropriate.
Validation: checks allowed here; checks reserved for the main agent.
Stop: return once answered; report a blocker rather than expanding scope.
```

Workers share the checkout. Assign non-overlapping files and preserve existing user changes. Schema, dependencies, lockfiles, shared module wiring, formatting across crates, and mutable runtime state stay with the main agent. Child agents do not run workspace builds, testkit cases, the GUI, Steam, or network integrations unless their brief explicitly assigns an authorized isolated check.

Main-agent handoff: inspect the combined diff, verify consequential findings against source, resolve contradictions, and run validation once after integration. Distinguish passed checks, checks not run, and existing failures. A successful narrow test does not establish workspace or runtime coverage.

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

### async-safety-reviewer

Load [core conventions](CORE_CONVENTIONS.md) and `src/core/AGENTS.md`; add [sync guardrails](SYNC_ALGO_CONVENTION.md) for sync changes and [UI conventions](UI_CONVENTIONS.md) for egui task dispatch. Trace ownership across each changed `.await`, task spawn, cancellation branch, and result delivery.

- Identify blocking I/O, hashing, parsing, and long loops on the async executor or UI thread. Use existing bounded workers. `spawn_blocking` work keeps running once started even when its awaiting future is cancelled, so review cooperative cancellation and cleanup inside the operation.
- Check guard lifetime and lock ordering. A short synchronous mutex can be appropriate when released before `.await`; an async mutex does not fix a long critical section or nested lock cycle.
- Check bounded queues, concurrency gates, backpressure, timeout behavior, and handling of task errors. A timeout that drops a future does not roll back filesystem or database writes.
- Trace shutdown, partial downloads, patch fallback, and task completions after a game-space switch. Verify the operation's space/context stays attached to its result and stale results cannot mutate the active space.
- Respect the database process claim and Turso seam. Verify transaction/gate ownership and `(remote_url, local_path)` scoping; never open runtime databases to inspect a review.

Report concrete failure scenarios with file:line evidence. Do not edit or run builds or live cases.

## Structural validation

`cargo agent-check` is a read-only Rust helper in `tools/agent-check/`, with its own workspace and lockfile. It dynamically checks all canonical skills and their references: metadata, discovery parity, loader targets, catalog coverage, UI metadata, role pairs, instruction imports, local Markdown file links, and text rules. It does not prove skill quality, enforce shell permissions, validate every prose path or link anchor, or exercise Foxy.

Claude roles omit edit tools for read-only work, but shell access still permits writes. Codex custom-agent instructions also inherit session permissions. These definitions express task boundaries; they do not create a separate sandbox or authorize external actions.

## Primary references

- [Codex skill discovery](https://learn.chatgpt.com/docs/build-skills) and [custom subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents).
- [Claude Code skills](https://code.claude.com/docs/en/skills) and [custom subagents](https://code.claude.com/docs/en/sub-agents).
- [Tokio shared-state guidance](https://tokio.rs/tokio/tutorial/shared-state) and [blocking-task cancellation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).
