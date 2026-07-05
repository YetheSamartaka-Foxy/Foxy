# 1.1.0
## Added
- Repository visual folders let repositories be grouped, colored, and collapsed in the repository list, independently of repository spaces, with drag-and-drop into folders, folder-level quick check / recheck / update actions, and an option to remove contained repositories when a folder is deleted.
- Arma 3 profile management in Settings can detect, rename, clone, and safely delete profiles, with confirmation dialogs, protected default profiles, and backup-based deletion.
- Addon file search: a "Search addon files" toggle for the repository, optional, and external addon lists matches filter terms against files inside addon folders, auto-expanding matching addons, with manual expand/collapse via a context-menu action.
- An "Export Repository Structure" action.
- A collapsible per-addon diff in the download view.
- New themes: Red, Viola, and Austrian Owl.
- Early experimental macOS build support.

## Changed
- Startup quick scans are faster and churn less: addon fast-path preflight, serialized per-repo scans, skipping redundant tree-hash verification after bootstrap hashing, and skipping unchanged checksum/content-hash/display-name writes to reduce database churn.
- Hashing profile detection now tunes scheduling by storage class, using safer auto profiles and capped large-part hashing on HDDs while allowing boosted concurrency on SSDs, with clearer scheduler logging.
- General hashing and hash-persistence improvements, including a content-hash format without file creation time and automatic bounded startup database compaction and stale-artifact cleanup.
- Pending-update transfer estimates are more accurate, derived from prepared download targets.

## Fixed
- Addon folders now resolve case-insensitively across launch, backup, join preflight, force-redownload, and sync paths, avoiding failures on manifest/on-disk case mismatches.
- Repository-space required repositories are now auto-imported correctly, and a database-wipe race condition was resolved.
- Theme switching no longer causes garbled characters to appear in non-localized text.
- A dangling addon inventory cache was fixed.
- Checksum readiness for partless files is preserved, and quick scans no longer run against incomplete part metadata.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 1.0.0
## Added
- Theme customization support, including importing and exporting themes, built-in theme presets, and a new "Swiftier" theme, with improved color-picker spacing.
- A UI scale slider was added so the entire interface can be sized up or down to taste.
- Toast notifications were added across the app to give clearer, immediate feedback for common actions.
- Scheduled jobs can now be created from Settings to recheck repositories, download available updates, and optionally close Foxy or shut down the PC at a chosen time while Foxy is open.
- Scheduled jobs support one-time and recurring weekday schedules, single repository targets, whole repository spaces, custom repository selections, enable/disable controls, edit/delete actions, and a manual "Run now" action.
- Scheduled post-actions now show a cancellable countdown before Foxy closes or the PC shuts down, and can be limited to only run when all scheduled operations succeed.
- Pre-launch readiness checks for TeamSpeak 3 and Steam: before joining a server or launching Arma 3, Foxy can warn when a required TeamSpeak 3 plugin is shipped but TS3 is not running, or when Steam is not running, and offers in-modal "Launch TeamSpeak" / "Launch Steam" buttons. Both checks have a global toggle and a per-repository override, and auto-clear once the client starts.
- The download screen now shows live timeline graphs for download speed, disk write speed, hashing throughput, CPU, and memory usage.
- Automatic detection of the Arma 3 installation folder.
- File size information was added in more places across the UI.
- An agent GUI driver was added (a local `foxy ui --agent-gui` / `foxy agent-gui` interface) for automated UI inspection and control, egui_mcp along with an optional on-screen FPS counter in the footer.
- Repository filtering was added to the repository list, accompanied by inline filter help.
- "Export logs to ZIP" and "Open log folder" actions are now also available directly from the activity log bar.
- Hover support and tooltips were expanded across interactive controls for better discoverability.
- Build channels now distinguish official, development, and prerelease builds.
- License
- Added Slovak language

## Changed
- The local database backend was migrated to Turso, replacing the previous SQLite/SeaORM stack for faster and more reliable repository metadata storage.
- Repository hashing was reworked for faster first-download hash preparation, cumulative hashing with retained speed history, and more accurate recalculation, with several correctness fixes.
- Startup quick scans now run per repository concurrently, skip full tree loads on already-verified paths, and use a persistent addon hash cache to avoid repeated folder hashing on warm scans, for noticeably faster startup.
- Repository sync state is now scoped by both repository URL and local path, so repositories sharing a URL at different folders no longer interfere with one another.
- Small-update sync and sibling-repository propagation are faster, finalizing shared content hashes from addon rows instead of reloading full file/part trees.
- Maximum download speed utilization was improved.
- The download summary was reworked with grouped transfer, hash, and duration statistics and a localized speed graph.
- Addon card sizing and the download-screen font sizes were improved for better readability.
- Long addon, mission, and activity-log lists now reuse cached per-row text shaping for smoother scrolling.
- The default UI font was changed to Roboto.
- Theme rendering was improved for more consistent colors across the app.
- External Addons handling was improved, including better caching and visuals.
- The customization (theme/appearance) screen visuals were refreshed.
- The Help view, About view (now with Markdown rendering), and changelog view were improved for readability.
- The "Recheck Complete" message is now a dismissible information block instead of a persistent banner.
- The settings view UX and default values were refined.
- Repository update download and size estimates were made more accurate.
- The total-size and enabled-addons summary text was clarified.
- Swifty migration now strips the `-noLand` launch parameter from imported configurations.
- The bundled locale translation tooling was improved, along with translation fixes across languages.
- The Windows installer can now request administrator elevation when needed.

## Fixed
- Join preflight now performs additional missing-addon checks so required external addons are detected more reliably before launch.
- Stale preflight addon handling was corrected so external addons resolve and download from the right source.
- The startup check settings are now respected in situations where they were previously ignored.
- Resuming a cut-off download now works correctly instead of restarting the transfer.
- File system watcher behavior was corrected.
- A wrong update-state cache flow that could show an incorrect update status was fixed.
- Hash progress no longer flickers into the download progress bar during active downloads.
- Repeated editor-mission log spam was removed from the activity log.
- Expanding a repository space no longer occasionally requires an extra double-click.
- The selected server is now more clearly highlighted in the repository server list.
- The missing visual state for the editor mission "Show folders" toggle was restored.
- Repository image handling was improved to avoid misrendered or stale repository images.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.9.1
## Added
- Repository-space members can now use a custom local path override, with a "Default" action in repository settings to return to the shared space path.
- Ability to delete specific addon in repository settings.
- Repository deletion confirmations now include an optional "Delete files" checkbox for removing downloaded local files together with the repository entry.
- Destructive repository, addon, mission, folder, dependency, cleanup, and force-redownload actions now show confirmation dialogs before they proceed.
- A repository-shipped Foxy locale translation skill was added to make future localization updates easier to validate and review.

## Changed
- Repository deletion keeps local files by default and requires an explicit opt-in before downloaded files are removed.
- Localization coverage was refreshed across all non-English languages for the latest destructive-action confirmations.
- The i18n checker now reports duplicate locale keys and can verify that specific changed strings were translated away from their English fallback.
- Quick scans that cannot make a reliable decision now leave the repository status unknown instead of treating the result as clean.
- Pending-update and active-download size estimates now account for planned delta-patch transfer sizes where available.
- Hash recheck and recalculation progress now includes file-part progress, including profile hashing.
- Force redownload now clears repository files and metadata on a background worker before starting the forced download.
- Single-repository database wipes now use bulk SQLite cleanup for faster repository metadata removal.
- Repository-space manifest imports, repository metadata fetches, addon hash recalculation, cached pending-update reads, and Join server-status checks now run off the UI thread.
- Swifty migration now binds imported repositories to imported repository spaces more reliably, preserves per-repository mod folder paths, and imports duplicate Swifty repositories as profiles on the existing Foxy repository.

## Fixed
- Recheck and hash bootstrap cancellation no longer leaves the UI stuck in a loading or cancelling state.
- Full-tree hash recalculation and auto hash-profile benchmarking now stop more cleanly when cancellation is requested.
- Finished repository sync workers are cleaned up more promptly, preventing database wipes, direct downloads, and maintenance actions from starting while a sync is still finishing.
- Quick scans with unavailable local paths no longer progress toward clean or update decisions.
- Quick-scan path preflight no longer flags a never-downloaded repository as mismatched only because its folder already exists.
- The local-path safety guard now recognizes addon content that is present on disk but not at the manifest's expected paths, and pauses before a likely near-full redownload.
- Explicit force redownload is no longer blocked by the local-path safety guard.
- Repository purge now clears leftover download and part records more reliably during force redownload and database wipes.
- Deleting a repository now also removes its database metadata, while preserving addons and files that are still referenced by another repository.
- Repository deletion from the main list and repository settings now uses the same confirmation behavior, including the optional file-removal choice.
- Stale delta-patch plans are cleared when a file falls back to a full download or no useful patch is available.
- Join preflight now recognizes matching external addons from other configured repositories and offers them as launch additions instead of misleading download suggestions.
- Join preflight remote-addon selections now trigger the specific source repository addon download and log the resolved source/addon decision.
- Addon hash recalculation no longer reports an addon as missing when all of its files already have local checksums.
- Repository-space shared-path changes no longer overwrite repositories that have a custom local path.
- TeamSpeak 3 plugin detection now recognizes manually installed plugins by comparing the `.ts3_plugin` package payload against files in TeamSpeak's plugin folder.
- The TeamSpeak 3 plugin settings card no longer overflows past the right margin.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.9.0
## Added
- Support for launching with `-profiles=<path>` so Arma 3 profile roots can be explicitly targeted.
- Addons can now be marked as favorites and filtered by favorite state.
- Drag-and-drop repository reordering support was introduced in the repository list.
- Server-side addon preflight and client-side addon metadata flow were added.
- Migration support was added for handling multiple repository entries of the same source as profile variants.
- New Application setting: UI renderer preference (`Auto`, `WGPU`, `Glow`).
- Automatic renderer recovery markers were added so Foxy can detect previous `egui-wgpu` panics and recover on the next launch.
- A startup notice modal was added when Foxy auto-switches renderer after a detected WGPU crash.
- New resource profiling utility was added to classify runtime memory pressure (`normal`, `constrained`, `severe`) for scheduler decisions.

## Changed
- Repository Settings and App Settings screens were reorganized and visually refined for clearer control grouping.
- Search now accepts multiple entries in a single query input.
- UTF-8 handling for profile and mission names was improved.
- Hashing pipeline behavior was optimized across several internal passes.
- Quick local check and remote data recheck descriptions were clarified.
- Logging behavior was improved, including cleanup of log files older than 90 days and increasing the max retained log files to 16.
- `eframe` now builds with the `glow` feature enabled so OpenGL renderer fallback is available at runtime.
- Hash scheduling now adapts concurrency to detected system memory pressure, including safe caps for auto and manual profiles.
- Auto hash-profile benchmarking now narrows candidate profiles under constrained/severe resources and records cap reasons in decisions/logs.
- Download scheduling now applies resource-tier limits (large/small file slots, active range requests, per-file range workers, and chunk targets).
- Download batching and range splitting now use scheduler-provided runtime limits instead of static constants.
- Settings schema/examples were extended with `ui_renderer: "Auto"` for config compatibility.

## Fixed
- Fixed accidental near-full repository redownload scenarios after repository path resets or mismatched local addon roots by adding a pre-download safety guard that pauses and explains the issue.
- Fixed repository local-path identity comparisons to treat equivalent path variants (trailing separators, canonicalized forms, Windows case differences) as the same path, reducing false reset/redownload triggers.
- Download progress and redownload sometimes crashing the app.
- Foxy now persists a WGPU crash marker from panic hook paths and switches to Glow on next launch, preventing repeated startup crashes on unstable WGPU/driver setups.
- Renderer fallback notice markers are cleaned up after dismissal to avoid repeated prompts.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.8.0
## Added
- German, French, Spanish, Portuguese, Brazilian Portuguese, Russian, Ukrainian, Polish, Japanese, and Simplified Chinese localizations are now included with full pluralization rules and formatting.
- Arabic, Bengali, Dutch, Hindi, Indonesian, Italian, Korean, Persian, Tagalog, Thai, Turkish, and Vietnamese localizations are now supported natively.
- Hebrew and Urdu localizations are now supported with dedicated font rendering.
- Swedish, Norwegian Bokmål, Danish, Finnish, Greek, Hungarian, and Romanian localizations are now included.
- Bulgarian, Serbian, and Croatian localizations are now included.
- Slovenian, Lithuanian, Latvian, and Estonian localizations are now included.
- Editor mission management tools can now be used directly from within the repository interface.
- Editor Missions now include a terrain filter dropdown beside "Show folders" for quickly narrowing the list to a single map.
- Editor mission dependencies can now be removed from `mission.sqm` directly from the mission context menu.
- Launching an editor mission with additional or external addons enabled now shows a warning with options to launch with addons, launch without them, or cancel.
- Launching an editor mission now shows a toast confirming that Arma 3 is starting, and repeated clicks during startup are suppressed so the app stays responsive while Arma 3 loads.
- Subfolder structures inside editor missions are now recursively supported and traversed flawlessly.
- A Swifty migration onboarding view now greets legacy users to simplify the shift to the new application environment.
- TeamSpeak 3 plugin installation integration now provides a seamless way to deploy and track required plugins.
- Drag-and-drop repository list reordering now allows users to customize the exact display ranking of their entries.
- An "Export logs to ZIP" feature in the Settings view now automatically compresses all diagnostic logs via Deflate into a local timestamped archive.
- Sync cancellation functionality now cleanly and safely aborts in-flight large hashing or downloading operations.
- Protective confirmation dialogs now intervene before profile deletion, profile reset, or settings reset operations.
- Observability logs are now thoroughly piped into the download, hashing, and delta patch systems to provide precise internal metrics.
- The external wiki documentation now includes a rigorous User Tutorial section to help beginners.
- A Server Admin tutorial section is now provided on the wiki to document advanced administration flows.
- Local content hashing passes now support generic payload optimizations, specifically targeting `gzip` archives.
- Mod downloading operations now visibly log detailed metadata during chunk retrieval.
- Progress events are now broadcast globally via an internal tokio channel during complex syncs.
- The UI application state now retains historical download speeds during an active session.
- Global settings can now hide the Editor Missions list and the Servers list from the repository view via two independent tickboxes (enabled by default), with per-repository override support similar to other global settings.

## Changed
- Downloader and sync pipeline optimizations now drastically improve file transfer velocities and general application stability.
- Hashing modules were refactored to introduce highly optimized file I/O algorithms for faster disk recalculations.
- Active mod downloads now dynamically bubble to the top of the queue so the mod with the highest progress is seen first.
- Application start-up sequences now run quicker and omit redundant diagnostic logging across regular boots.
- Repository space labels and name definitions can now accept an increased maximum character limit.
- Repository cards in the main application list now feature a denser vertical layout, enabling more on-screen items.
- SQLite connections now enforce enhanced WAL, synchronous schemas, and filesystem safety constraints during updates.
- TeamSpeak 3 plugin parsing logic has been refined, offering better safety guarantees and installation state resolution.
- Log outputs during complex file fetches and repository sync operations have been made cleaner and more concise.
- Internal layout rendering logic was modified to limit overall GUI idle CPU overhead.
- Download speed limiters have been re-tuned for smoother scaling across different network bandwith profiles.
- Temporary file cleanups have been hardened, preventing accidental accumulation of aborted data parts.
- Large repository operations such as tree recalculation now scale much more robustly on multi-core environments.
- The in-app help guide phrasing was revamped to better explain Foxy's specific configuration logic.
- Migration and repository space views were altered to support a clearer UI hierarchy.
- The download batching logic now favors size-adaptive split counts over rigid static allocation blocks.
- Repository list synchronization states were streamlined to trigger fewer expensive `egui` view recalculations.

## Fixed
- Over 20 significant cross-platform compatibility edge-cases between Windows and Linux were definitively addressed.
- Arma 3 profile paths located inside a Microsoft OneDrive synchronized location are now actively flagged and rejected to prevent file corruption.
- Advanced launch failure diagnostic messages now accurately guide users around networking and local permission faults.
- Severe `egui` ID-salt collision panics that could trigger under extremely fast download update scenarios are now impossible.
- An additional subset of UI re-rendering bugs (`egui id fixes vol2`) were caught and repaired prior to rollout.
- Users are no longer able to rapidly double-click interface buttons, avoiding duplicate operation starts.
- Transient download states now fully and visibly revert back to zero when a sync operation is canceled manually.
- Margins and UI padding inside the repository update interface and the swifty migration window are properly aligned.
- Repository database wipes now completely purge legacy metadata, fixing cases where "MD5 protocol" or "update available" banners lingered.
- Multiple rare filesystem lock conditions and application crashes occurring during download stream interruptions have been resolved.
- Activity logging traces will no longer become flooded with harmless Vulkan renderer framework warnings.
- The internal download speed and ETA estimation logic no longer presents wildly erratic numbers on very slow hard disks.
- Spurious "rogue update summaries" will no longer inadvertently appear when resolving aborted synchronization tasks.
- Mod sorting routines within active tasks correctly default to alphabetical organization when download progress points tie.
- Settings saving routines will correctly wait for the disk commit, solving scenarios where closing the app exactly after a change was discarding it.
- Canceling a download gracefully clears all internal pending queries.
- Missing pointer cursor feedback on hover inside the repository list interface was added back.
- Offline repository servers no longer show a Join action in their context menu.
- Cross-platform file path sanitization now more aggressively catches edge-case symbols and spaces.
- Optional addon enable/disable choices now persist across repository refreshes and app restarts, so unchecked optional addons are no longer re-downloaded or silently re-enabled after every sync.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.7.0
## Added
- GitHub Releases can now be used as an app update source alongside self-hosted update manifests, including update checks, version browsing, and changelog import from release bodies.
- Steam integration now auto-detects the Steam installation, adds a configurable Steam directory in Settings/CLI, and can start Steam automatically before launching Arma 3.
- Repository spaces now support bulk operations for rechecking or updating multiple attached repositories at once, with review/selection UI and CLI `space sync` include or exclude filters.
- Repositories can now supply Arma 3 launch parameters and Creator DLC content through `repo.json`, with global and per-repository opt-in controls.
- Foxy can now auto-discover an app update URL from `repository_space.json` or `repo.json`, and `foxy-server-backend-cli create` can write `appUpdateUrl` into generated metadata.

## Changed
- Repository space screens were redesigned for denser cards, clearer grouping, better scanability, and easier movement of matching repositories into a space.
- Settings now expose quicker maintenance actions such as opening the config directory and provide clearer save and folder-open feedback.
- App update settings now let users switch between server-hosted updates and GitHub-based updates from the same UI.
- Project docs and in-app descriptions were refreshed to better explain FoxyMode, Swifty compatibility, repository spaces, Steam integration, and update distribution.

## Fixed
- Long repository, repository-space, and profile names no longer break list rows, headers, toolbar actions, or close buttons in the UI.
- Steam Workshop addon detection and path handling are more reliable, reducing misclassification in external addon discovery.
- Manual app update URLs are now treated as user overrides until cleared, preventing metadata auto-fill from overwriting an intentional setting.
- Shared addons in repository spaces are no longer repeatedly re-downloaded across sibling repositories when local files already match the same remote version.
- Sibling checksum and content-hash propagation now uses guarded retryable SQLite transactions, reducing stale-state races during sequential space sync flows.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.6.1
## Added
- No additions in this release.

## Changed
- No changes in this release.

## Fixed
- No fixes in this release.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.6.0
## Added
- In-app application update system with configurable update source URL, launch-time auto-check, manual check, and footer update badge.
- New App Update and Version Browser screens with per-version changelog preview and ability to upgrade, reinstall, or downgrade to any server-provided version.
- Secure installer download pipeline for app updates with progress reporting, BLAKE3 integrity verification, and one-click install/restart flow.
- Repository update view can now copy an update manifest to the clipboard with the repository total and per-addon update sizes for sharing.
- Brief in-app toast notifications for clipboard-driven actions such as copying the update manifest, copying the activity log, and importing or exporting profiles.
- New `foxy-server-backend-cli` commands for app-update distribution: `setup-app-updater` and `new-app-update`, including `foxy-app-updater.json` generation and per-version changelog JSON export from `CHANGELOG.md`.
- Platform installer tooling: Windows Inno Setup script (`installer/windows/foxy-setup.iss`) and Linux self-extracting installer pipeline (`installer/linux/build-installer.sh`, installer header, `.desktop` template).
- CI artifact workflow support for building and uploading Windows and Linux installers via the `build_installers` toggle.
- English and Czech localization coverage for the new app-update and version-browser UI.

## Changed
- Windows launch behavior now detects installer-based deployments and skips runtime Start Menu shortcut registration when `unins000.exe` is present.
- Release documentation now includes local Windows/Linux installer build steps and app-update distribution workflow examples.

## Fixed
- Prevented duplicate/competing Start Menu shortcut creation paths by deferring shortcut management to the installer when applicable.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.5.0
## Added
- Hybrid hashing protocol support for repositories: FoxyMode (BLAKE3) plus HybridMode compatibility with legacy Swifty MD5 artifacts for gradual server migration.
- New `foxy-server-backend-cli` generation modes (`foxy`, `swifty`, `hybrid`) with Foxy manifest output (`foxy_addon.json`/`foxy_addons.json`) and optional non-animated progress mode for accessibility.
- Legacy protocol warning banner in the repository UI when a repository still uses the old Swifty MD5 protocol.
- Optional aliases for additional addon folders, available in both Settings UI and CLI (`settings set --set-additional-folder-alias` / `--clear-additional-folder-alias`).
- Collapsible origin groups in External Addons with persisted collapsed state.
- Improved external addon origin detection for Steam Workshop content with explicit `Steam Workshop` origin labeling.
- Locale-aware plural translation keys and formatting support for counts/messages that vary by language.

## Changed
- Repository settings header now includes the selected repository name (`Repository Settings - {repository_name}`) for clearer context.
- External addon discovery now uses stronger path normalization/canonicalization and deterministic origin selection.
- CLI help text and in-app help copy were expanded to better explain config directory behavior, sync modes, launch behavior, and direct-download defaults.
- UI interaction polish improved with broader hover feedback on interactive surfaces.
- Localization pipeline improved with locale-aware number/date/size formatting and locale-aware collation in user-visible lists.
- Download screen visuals were refreshed for clearer update-state presentation.
- Project dependencies were updated, including egui/eframe 0.34.x and related runtime crates.

## Fixed
- Case-insensitive addon name matching when resolving repository membership for discovered addons.
- External addon scanning now avoids pulling folders that are outside declared repository/additional-folder scope.
- CLI error output is now structured (`Action`, `Error`, `Detail`) to improve readability and assistive-tool parsing.
- Keyboard accessibility in repository flows improved with arrow-key navigation and Enter activation support.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.4.0
## Added
- Context menu actions for repository server rows, including quick refresh and join actions.
- Short-lived refresh indicators in the repository server list so active status checks show a spinner while they are in progress.

## Changed
- Local-only hashing now uses shared BLAKE3-based content hashes for quick checks, addon backup inventory, and related integrity diagnostics while keeping server-compatible MD5 tree checks where required.
- SQLite connection setup now applies WAL, busy-timeout, synchronous, temp-store, and foreign-key settings to every pooled connection, with shared retryable write transactions across sync and hash persistence paths.
- Repository refresh and quick-scan bootstrap queries were reduced and existing rows are reused more aggressively, improving large-repository responsiveness and lowering redundant database work.
- File-part hashing and delta-patch application now reuse file handles and buffers and avoid unnecessary full-file rereads during verification.
- Downloads now choose split counts based on file size, use a simpler buffered path for small files, and reduce bandwidth-limiter and per-mod progress overhead.
- Repository status banners were consolidated, and quick local check completion now reports elapsed time more clearly.
- Repository server rows were redesigned with clearer selection styling, status badges, and smoother refresh repaint behavior.

## Fixed
- Repository launch now preserves quoted additional launch parameters, applies selected profile launch options more reliably, and closes the app through the normal shutdown path after launch.
- Download and delta-patch fallback flows now retry interrupted range transfers, flush buffered writes before verification, and surface size or integrity mismatches more reliably.
- Purge and download-table cleanup flows now behave more predictably with foreign-key cascades enabled and transactional table truncation.
- Repository settings row expand/collapse state now persists through cached saves without marking the whole repository dirty.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.3.2
## Added
- Collapse/expand controls for repository spaces and the ungrouped repositories section in the repository list.

## Changed
- Repository list and repository-space views were optimized for better responsiveness with larger datasets.
- Repository addon, optional addon, and external addon lists now use cached/virtualized rendering for smoother scrolling and filtering.
- Settings/repository persistence and backup inventory refresh now run asynchronously with clearer in-app status feedback.
- Global repaint scheduling was tightened to reduce unnecessary redraws and lower idle UI overhead.

## Fixed
- Profile options, Creator DLCs, and basic startup parameter controls now resize and wrap correctly in repository settings.
- Repository settings no longer rebuild addon list data every frame, reducing UI stalls on large repositories.
- Missing pointer cursor feedback on the repository launch button.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.3.1
## Added
- No additions in this release.

## Changed
- No changes in this release.

## Fixed
- Color theme to be consistent across windows Light/Dark theme settings

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.3.0
## Added
- Delta patching for updates, with automatic fallback to full-file downloads when a patch cannot be applied.
- CLI support for scripted and automation-friendly operations.
- Standalone/direct download tools for downloading specific repository, addon, or file targets outside the normal database-managed flow.
- Addon backup support with a backup manager in settings.
- Memory diagnostics view.
- In-app help screen.
- Persistent window state across restarts.
- Repository operation-status indicators, elapsed-time feedback, and better progress visibility for long-running actions.
- Addon grouping by repository origin and repository-origin filtering for external addons.
- Option to hide the app to tray after launch.
- Button to open the destination folder for direct downloads.

## Changed
- Default language now follows the user's system locale.
- Startup quick-scan and recheck behavior was optimized for faster app startup.
- Repository, settings, and addon-list UI layouts were refined for clearer actions and better scaling behavior.
- User-entered local paths are now trimmed and sanitized consistently across UI and CLI.

## Fixed
- Local change detection edge cases involving deleted content, malformed PBO part paths, and quick-check accuracy.
- Fresh downloads now correctly handle deleted files and deleted-repository plans.
- Repository-space settings saving and repository-origin lookup issues.
- Crashes or broken UI states when opening addon tabs, closing version/about/help screens, or resizing/scaling the app window.
- Repo sync repainting issues and startup console/window behavior glitches.
- Language persistence and remaining unlocalized strings.
- Wipe-repository-database flow now clears repository status correctly.
- cDLC launch settings and external-addon save edge cases.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.2.3
## Added
- Better Linux support.
- Update summary is now shown when downloads finish in the background.

## Changed
- Hashing now runs after each downloaded addon instead of one large hash pass at the end.
- Hash persistence behavior was improved to reduce large write spikes during content/hash refresh operations.

## Fixed
- Potential UI corruption from repeated window resizing.
- Startup background task scheduling that could freeze the UI.
- Unconditional repaint loop causing unnecessary UI churn.
- Blocking image download/decode work on the UI thread.
- Activity log rendering path that cloned and processed too much data every frame.
- Repository list rendering path that recomputed and allocated too much every frame.
- Update modal work that performed large clone/sort operations every frame.
- Missing hot-path database indexes.
- N+1 query pattern in download target to addon/mod lookup.
- Per-file download target upserts in batch flow.
- Count-query behavior that materialized full tables.
- Download progress bar flickering.
- Missing numbers on the finished update screen.
- UI filter changes triggering repository saves during interaction.
- Incremental hashing path reloading the full tree repeatedly.
- Content-hash refresh persistence not updating rows efficiently.
- Normal sync link-table behavior that caused append-only growth (table bloat risk).
- SQLite pool/concurrency behavior that increased lock contention and could freeze the app under load.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.2.2
## Added
- Repository Spaces support for grouping repositories under shared space entries.
- Import support for remote `repository_space.json` manifests.
- New repository-space management flows in UI (select, view details, add, detach, and remove associations).
- Queue-based per-space sync processing and related state handling.
- New example fixture files under `examples/json/` for appdata and remote repository-space manifests.

## Changed
- Repository list UI now supports space-aware grouping and space detail presentation.
- Repository add flow was refined to better support repository spaces.
- Shared path reconciliation behavior was improved when loading or resetting repository-space data.
- Image checksum validation now accepts both MD5 and SHA1 with automatic detection.

## Fixed
- Improved matching/scanning behavior for linking existing repositories into repository spaces.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.

# 0.2.1
## Added
- Adaptive download speed limit (Mbps) setting with optional unlimited mode.
- Per-repository override for auto quick-scan on launch.
- Startup option hover descriptions for Arma launch flags and extra CLI params.
- More detailed hash recalculation progress reporting during sync.
- Safer app-close handling with retry/timeout logic for stubborn window shutdowns.

## Changed
- Local data model naming was standardized from "mods" to "addons".
- Quick local verification now focuses on pending updates and targeted file checks instead of broad scans.
- Hash recalculation pipeline now uses batched persistence and improved concurrency control for better large-repo behavior.
- Repository reset now forces a full download flow instead of a recheck-only pass.
- Startup quick-scan scheduling now filters repositories to only eligible targets.

## Fixed
- Improved path safety when removing repository content to avoid deleting outside intended directories.
- Better handling of synthetic debug repositories/folders so temporary debug entries are not persisted.
- Local file presence and path matching checks now better detect moved/missing files before deciding to skip downloads.
- Unexpected local addon files are now detected and cleaned up during targeted verification.
- Fixed cases where local tree-hash bootstrap could over-scan by initializing only missing hashes when possible.
- Fixed edge cases where files with valid part checksums but invalid part layout/size were incorrectly treated as valid.

## Removed
- Removed the previous governor-based download limiter in favor of the adaptive limiter.

## Reverted
- No reverted changes in this release.

# 0.2.0
## Added
- SeaORM downloader is now connected to the GUI.
- New in-app changelog screen.
- New in-app about screen.
- Localization support with English and Czech languages.
- New in-app activity log with copy support and file logging.
- Download controls for filtering, pausing/resuming, and force redownload.
- Repository tools: context actions, quick local scan/verify, hash recalculation, and DB wipe.
- Customizable UI palette and font sizes in settings.
- New settings action to open the log folder.

## Changed
- App branding renamed from Swiftier to Foxy.
- UI code reorganized under `src/ui`.
- Backend internals refactored for consistency.
- Hashing performance improved via parallelization and other optimizations.
- UI styling for interactive rows/checkboxes centralized through the palette system.
- Better Windows integration (Start menu registration and configurable icon build behavior).

## Fixed
- Fixed Czech locale encoding and BOM-related locale parsing issues.
- Prevented duplicate repository DB wipes when changing repository paths.
- Improved handling and visuals for unknown repository state.
- Included folder icon/build fixes and dependency updates.

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.
