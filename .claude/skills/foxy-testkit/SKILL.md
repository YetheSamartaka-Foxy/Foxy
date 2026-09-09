---
name: foxy-testkit
description: "Run Foxy's developer-machine test kit: UX click-through cases through the agent GUI driver, and downloader/checker/database performance cases with an append-only ledger, baselines, regression verdicts, and WAL versus MVCC comparison."
argument-hint: "[case name, suite filter, or 'perf'/'ux']"
---

# Foxy test kit

This Claude Code project skill is a loader for the repo-maintained Agent Skill at `skills/foxy-testkit/SKILL.md`.

Before acting, read the canonical skill:

```powershell
Get-Content -Raw skills\foxy-testkit\SKILL.md
```

Follow its instructions exactly. Use this project skill as `/foxy-testkit` in Claude Code. Keep this file small; update the canonical skill in `skills/foxy-testkit/` when the test kit workflow changes.

## Quick reference (full details in the canonical skill)

The kit is the `foxy-testkit` binary (`cargo build -p foxy-testkit`); there is no PowerShell or Python in it.

- **One case:** `foxy-testkit run --case .\testkit\cases\<id>.json --no-build`
- **Suite:** `foxy-testkit suite --filter "ux-*" --no-build` or `--tag database`
- **Validate only:** add `--validate-only` (no build, no launch, no disk writes to the target)
- **Database variants:** `--database-mode wal|mvcc`, then `foxy-testkit compare --case-id <id>`
- **Kit changes:** verify with `foxy-testkit replay --all` before running a case
- **Artifacts:** `testkit/runs/<case-id>/<run-id>/`; ledger rows in `testkit/ledger/<case-id>.jsonl`
- **Local origin:** `foxy-testkit mirror --upstream <url> --output .\testkit\origin\data\<name> --force`
- **Close other Foxy processes first**; one process owns a game space database.
- **Never** compare debug with release, GUI with CLI, or WAL with MVCC outside `foxy-testkit compare`.
