---
name: foxy-gui-driver
description: "Drive or debug Foxy's real egui desktop UI from Claude Code. Use when launching Foxy in GUI mode, opening views, inspecting text/snapshots, taking screenshots, sending clicks/scroll/keyboard input, reading FPS, or validating UI changes through the local agent-gui harness and egui_mcp."
argument-hint: "[gui task]"
---

# Foxy GUI Driver

This Claude Code project skill is a loader for the repo-maintained Agent Skill at `skills/foxy-gui-driver/SKILL.md`.

Before acting, read the canonical skill:

```powershell
Get-Content -Raw skills\foxy-gui-driver\SKILL.md
```

Follow its instructions exactly. When it points to `references/harness-design.md`, read:

```powershell
Get-Content -Raw skills\foxy-gui-driver\references\harness-design.md
```

Use this project skill as `/foxy-gui-driver` in Claude Code. Keep this file small; update the canonical skill in `skills/foxy-gui-driver/` when the GUI driver workflow changes.

## Quick reference (full details in the canonical skill)

- **Launch both channels:** set `$env:FOXY_CONFIG_DIR` to a throwaway dir and `$env:EGUI_INSPECTION = "1"`, then run `Foxy.exe ui --agent-gui --agent-port 0`. Set the same config env for `agent-gui` client commands. Keep inspection loopback.
- **egui_mcp:** Foxy enables eframe's `inspection` feature. `EGUI_INSPECTION=1` exposes upstream inspection on `127.0.0.1:5719`; set it to `127.0.0.1:<port>` if needed.
- **Real data, no 1.3 GB DB:** copy only `settings.json` / `repositories.json` / `repository_spaces.json` from `%APPDATA%\Foxy` into the isolated dir; addon lists rebuild from the disk scan.
- **Reach addon tabs:** `agent-gui open-view repository-settings --repo-index 0 --tab external-addons`.
- **Read state without nodes:** `agent-gui repositories`, `agent-gui addons --repo-index 0 --tab external-addons` (structured rows - addon rows aren't semantic), `agent-gui settings`, `agent-gui progress`.
- **Reproduce high-DPI / big-window relayout:** `agent-gui scale 150`, `agent-gui resize --width 1100 --height 720`.
- **Negative scroll works:** `agent-gui scroll --x 990 --y 400 --dy -600` (no `=` needed).
- **JSON shape:** payload is at `.data.data.<field>` (double envelope).
- **Detect multi-pass / scroll-FPS regression:** diff `cumulative_pass_nr` vs `cumulative_frame_nr` (from `fps`/`snapshot`) across a scroll burst; `extra_passes>0` = the bug. Caveat: discrete injection may not reproduce interactive multi-pass - a `0` reading isn't proof.
- **Cleanup:** `agent-gui close`, stop stray `target\*\debug\Foxy.exe`, delete the temp dir.
