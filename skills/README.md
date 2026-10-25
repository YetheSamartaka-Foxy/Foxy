# Foxy agent skills

The maintained workflows live here. Codex discovers thin loaders in `.agents/skills/`; Claude Code discovers equivalent loaders in `.claude/skills/`. Both read the same canonical instructions. No global installation or symlink setup is required for repository work.

| Skill | Use for |
| --- | --- |
| [foxy-rust-change](foxy-rust-change/SKILL.md) | Rust implementation, compiler feedback, crate selection, and focused validation |
| [foxy-agent-maintenance](foxy-agent-maintenance/SKILL.md) | Instructions, discovery loaders, paired agent roles, and metadata checks |
| [foxy-locale-translator](foxy-locale-translator/SKILL.md) | Requested localization, placeholders, UTF-8, and fallback cleanup |
| [foxy-gui-driver](foxy-gui-driver/SKILL.md) | Desktop snapshots, screenshots, input, accessibility, and FPS inspection |
| [foxy-testkit](foxy-testkit/SKILL.md) | Isolated UX/performance cases, calibration, ledger, and regression evidence |

The five foundation skills above provide implementation, maintenance, translation, desktop-driver, and testkit mechanics. The following specialized workflows add domain-specific decisions and evidence requirements; load only the ones relevant to the task.

## Diagnosis and reproducibility

| Skill | Use for |
| --- | --- |
| [foxy-bug-bundle-triage](foxy-bug-bundle-triage/SKILL.md) | Manifest-first log diagnosis and evidence-backed failure hypotheses |
| [foxy-regression-reproducer](foxy-regression-reproducer/SKILL.md) | Isolated regression cases with controlled mutations and correctness checks |
| [foxy-startup-crash-diagnosis](foxy-startup-crash-diagnosis/SKILL.md) | Native graphics crashes, Rust panics, renderer fallback, and startup evidence |
| [foxy-cancellation-recovery](foxy-cancellation-recovery/SKILL.md) | Task cancellation, rollback, cleanup, restart, and retry-state verification |
| [foxy-diagnostic-fixture-sanitizer](foxy-diagnostic-fixture-sanitizer/SKILL.md) | Sanitized diagnostic fixtures that preserve causal and measurement evidence |

## Sync correctness and database integrity

| Skill | Use for |
| --- | --- |
| [foxy-sync-regression-check](foxy-sync-regression-check/SKILL.md) | Relevant sync guardrails, regression scenarios, and acceptance evidence |
| [foxy-shared-repository-integrity](foxy-shared-repository-integrity/SKILL.md) | Repository-instance identity, shared addons, sibling state, and propagation |
| [foxy-delta-patch-verification](foxy-delta-patch-verification/SKILL.md) | Republished manifests, shifted parts, empty entries, fallback, and final hashes |
| [foxy-turso-schema-recovery](foxy-turso-schema-recovery/SKILL.md) | Live-schema checks, repair gates, rebuilds, and restart recovery |
| [foxy-manifest-state-validation](foxy-manifest-state-validation/SKILL.md) | Manifest freshness, persisted part completeness, caches, and download plans |

## Performance engineering

| Skill | Use for |
| --- | --- |
| [foxy-performance-experiment](foxy-performance-experiment/SKILL.md) | One-hypothesis optimization loops with frozen workloads and correctness gates |
| [foxy-benchmark-evidence-audit](foxy-benchmark-evidence-audit/SKILL.md) | Comparable builds, diagnostics, cache states, environments, and references |
| [foxy-storage-hash-profiling](foxy-storage-hash-profiling/SKILL.md) | HDD/SSD read paths, cache effects, read amplification, and worker contention |
| [foxy-download-tail-analysis](foxy-download-tail-analysis/SKILL.md) | Download ramp, connection limits, scheduling, and final-tail attribution |
| [foxy-turso-write-profiling](foxy-turso-write-profiling/SKILL.md) | Transaction work, writer contention, gates, bulk inserts, and persistence timing |

## Desktop behavior and interaction

| Skill | Use for |
| --- | --- |
| [foxy-egui-responsiveness](foxy-egui-responsiveness/SKILL.md) | UI blocking, repaint pacing, layout passes, watchers, and resource attribution |
| [foxy-progress-correctness](foxy-progress-correctness/SKILL.md) | Monotonic progress, stage transitions, estimates, and honest completion state |
| [foxy-keyboard-accessibility-audit](foxy-keyboard-accessibility-audit/SKILL.md) | Navigation, focus, modals, scaling, reduced motion, and readable status |
| [foxy-game-space-switch-audit](foxy-game-space-switch-audit/SKILL.md) | Space-scoped state, cached paths, capabilities, and stale task results |
| [foxy-launch-readiness-verification](foxy-launch-readiness-verification/SKILL.md) | Launch/Join warning order, readiness checks, and continuation behavior |

## Platform integration and shipping

| Skill | Use for |
| --- | --- |
| [foxy-cross-platform-launch](foxy-cross-platform-launch/SKILL.md) | Process quoting, executable permissions, Wine, Steam, and unelevated launches |
| [foxy-workshop-integration](foxy-workshop-integration/SKILL.md) | Workshop discovery, helper isolation, frozen selections, and sharing contracts |
| [foxy-server-publish-validation](foxy-server-publish-validation/SKILL.md) | Generated repositories, manifests, launch lines, keys, and publication layouts |
| [foxy-release-readiness](foxy-release-readiness/SKILL.md) | Version provenance, update digests, packaged libraries, installers, and smoke checks |
| [foxy-contract-docs-sync](foxy-contract-docs-sync/SKILL.md) | CLI/help/JSON/example/UI-copy contracts and user/server documentation parity |

## Using and maintaining skills

In Codex invoke `$foxy-rust-change` or select a skill from `/skills`; in Claude Code invoke `/foxy-rust-change`. The other names work the same way. Automatic selection remains enabled. Other clients can read `skills/<name>/SKILL.md` directly. If a client does not refresh discovery during the session, restart it.

Use the `explorer`, `worker`, `reviewer`, and `async-safety-reviewer` roles for bounded delegation. Their definitions and shared handoff contract are documented in [agent workflows](../conventions/AGENT_WORKFLOWS.md). Model choice and permissions inherit the session.

After editing this setup, run from the repository root:

```text
cargo agent-check
git diff --check
```

The checker is Rust and uses a separate tools workspace. It validates metadata and entrypoint consistency without compiling or launching Foxy. It does not execute the procedures or validate their judgment; review those against the code and a realistic task. See [tools instructions](../tools/AGENTS.md) for checker development commands and [skill instructions](AGENTS.md) for maintenance rules.

Use `cargo agent-check --repo <checkout>` for another repository root, or `--help` for usage. Exit codes are `0` for valid metadata, `1` for failed checks, and `2` for usage errors. These are developer-tool codes, separate from the Foxy application CLI.

Accessibility impact: checker diagnostics use plain text and repository-relative paths; understanding status does not depend on color.
