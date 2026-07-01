# Foxy User Guide

Welcome to Foxy, a modern Arma 3 mod updater built for speed and reliability. This guide walks you through every feature of the application so you can set up repositories, keep your mods up to date, and launch Arma 3 with confidence.

---

## Table of Contents

1. [Getting Started](#getting-started)
2. [Adding Repositories](#adding-repositories)
3. [Repository Spaces](#repository-spaces)
4. [Syncing and Updating](#syncing-and-updating)
5. [Repository Settings](#repository-settings)
6. [Profiles](#profiles)
7. [Managing Addons](#managing-addons)
8. [Launching Arma 3](#launching-arma-3)
9. [Application Settings](#application-settings)
10. [Backup Manager](#backup-manager)
11. [Direct Download](#direct-download)
12. [Additional Search Folders and Cleanup](#additional-search-folders-and-cleanup)
13. [Customization](#customization)
14. [TS3 Plugins](#ts3-plugins)
15. [Swifty Migration](#swifty-migration)
16. [Keyboard Navigation](#keyboard-navigation)
17. [Troubleshooting](#troubleshooting)
18. [CLI Usage](#cli-usage)

---

## Getting Started

### First launch

When you open Foxy for the first time, you will see the main repository list on the left side of the window and a detail panel on the right. The repository list will be empty until you add your first repository.

The bottom of the window contains a footer bar with:
- An **activity log toggle** button (bottom right) that shows what Foxy is doing and recent core messages -- useful for troubleshooting.
- An **info icon** that opens the About page.
- A **question mark icon** that opens the in-app Help page.
- The **version number**, which opens the changelog when clicked.

### Initial setup

Before adding repositories, it helps to set up a few paths in **Settings > Application**:

1. **Arma 3 Directory** -- Point this to your Arma 3 installation folder. Foxy uses this to locate the game executable for launching.
2. **Steam Directory** -- Set this to your Steam installation folder, or click **Auto-detect** to let Foxy find it automatically. This enables Steam Workshop addon discovery and ensures Steam is running before game launch.
3. **Temporary Directory** -- Optional. Foxy uses this for cache and intermediate files. If left empty, it defaults to the Foxy config directory (e.g. `%APPDATA%\Foxy` on Windows).
4. **Addon Backup Directory** -- Optional. If you want automatic backups of addons before updates, set a folder here. If left empty, Foxy uses a default `backups` folder inside its config directory.

### Swifty compatibility

If you are coming from Swifty, Foxy is fully backwards compatible with Swifty repositories. No server changes are required -- Foxy detects the legacy MD5 protocol automatically and syncs accordingly. You can use Foxy right away with any existing Swifty repository.

---

## Adding Repositories

To add a repository:

1. Click **+ Add repository** at the top of the sidebar on the left.
2. In the dialog that appears, paste either:
   - A **repository URL** (the direct address of the repository).
   - A **repository space URL** (a manifest that contains multiple repositories -- see the next section).
3. Choose the **local folder** where the repository files should be stored. This is where Foxy will download mods to and launch from.
4. Click the confirmation button to add the repository.

The new repository will appear in the sidebar. If you pasted a repository space URL, Foxy will show you the available repositories within that space and let you choose which ones to add.

### Duplicate detection

If you try to add a repository that already exists, Foxy will show a confirmation dialog asking whether you want to add it again. This helps prevent accidental duplicates.

### Filtering repositories

When you have many repositories, use the **Filter repositories** text field below the add button to search by name. The sidebar shows how many repositories match the filter out of the total.

---

## Repository Spaces

Repository spaces group multiple repositories under a single shared manifest. Communities often use spaces to publish several related repositories (for example, a main modset, a training modset, and an optional extras modset) under one URL.

### How spaces appear

When you add a repository space, it appears as a collapsible section in the sidebar under **Spaces**. Click the space header to view its detail page, which shows:
- The space name and shared path.
- A list of **Available repositories** defined by the space.
- A **Matching existing repositories** section that can scan your existing repositories and associate them with the space.

### Adding repositories from a space

Each entry in the space's available-repositories list has an **Add** button. Click it to add that repository, using the space's shared path as the base folder. You can also filter the entries by name or address using the filter field.

### Shared paths

Repository spaces can define a common local path for all their repositories. When a repository belongs to a space, its local path is inherited from the space's shared path and cannot be changed individually. This keeps all space repositories organized under one root folder.

### Bulk operations

The repository space detail view has toolbar buttons for operating on all repositories in the space at once:

- **Recheck all repositories** -- Runs a remote data refresh on every repository in the space. A confirmation dialog shows the list of repositories before starting.
- **Quick local check** -- Runs a quick local content check on all repositories in the space.
- **Update all repositories** -- Downloads pending updates for every repository in the space that has updates available.

During bulk operations, a progress indicator in the space header shows how many repositories have been processed and which one is currently active.

### Scanning and moving existing repositories

If you added repositories individually before adding the space, click **Scan existing repositories** to find matches. Then select the ones you want and click **Move selected repositories** to associate them with the space.

---

## Syncing and Updating

After adding a repository, you need to check it against the remote server and download any updates. Foxy provides several check operations, accessible from the toolbar buttons in the repository detail view.

### Refresh (remote data recheck)

Click the **refresh icon** in the repository toolbar to fetch the latest metadata from the remote server and build an update plan. This is the primary way to check whether your local files are up to date.

### Quick local check

Click the **book icon** in the repository toolbar to run a quick local content check. This compares local content hashes (BLAKE3 fingerprints) to detect whether any files have changed locally -- without contacting the remote server. It is fast and useful for detecting local drift (for example, if you manually edited a file).

### Recheck repository integrity

Available from Repository Settings > Configuration, the **Recheck repository integrity** button performs a full remote metadata fetch and rebuilds all stored checksums for the repository. Use this as a deeper maintenance step after major local changes or suspected corruption.

### Understanding the update flow

1. Run **Refresh** to check the remote server.
2. If updates are available, a banner appears on the repository detail view with an **Update ready** message.
3. Click the banner to open the **update view**, which shows:
   - Which mods have changes.
   - How many files are affected per mod.
   - The total download size.
4. Start the download. A progress bar and status banner track the operation.
5. When complete, the repository state updates to **Synced**.

### Delta patching

Foxy uses delta patching when available: instead of redownloading entire files, it downloads only the changed portions. If a delta patch fails validation, Foxy automatically falls back to downloading the full file. This happens transparently -- you do not need to do anything.

### Progress and status banners

During any sync operation, the repository detail view shows a **status banner** with:
- The operation name and current step.
- A detail line describing what is happening.
- An elapsed time counter.
- A progress bar (when applicable).

After an operation completes, a **completed banner** appears with the result. You can dismiss it by clicking **Dismiss**, or click the action button if further steps are available (such as reviewing an update).

---

## Repository Settings

Open repository settings by clicking the **gear icon** in the repository toolbar, or by pressing **Enter** when a repository is selected. The settings view has four tabs: **Configuration**, **Addons**, **Optional Addons**, and **External Addons**.

### Configuration tab

The Configuration tab is divided into several sections:

#### Identity

- **Name** -- The display name for this repository. You can change it to anything you like.
- **Address** -- The remote URL of the repository. Changing this will update the repository metadata.
- **Local Path** -- The folder where this repository's files are stored. If the repository belongs to a space, this field is inherited from the space and cannot be edited here.

#### Sync settings

Each of these settings can be set to **Use global** (inherits from Application Settings), **On (override)**, or **Off (override)**:

- **Auto recheck on launch** -- Whether to automatically refresh this repository's remote data when Foxy starts.
- **Auto quick scan on launch** -- Whether to run a quick local check on this repository when Foxy starts.
- **Auto backup addons before update** -- Whether to back up changed addons before downloading updates.
- **Auto apply repo.json launch parameters** -- Whether to apply launch parameters defined in the remote `repo.json`.
- **Auto apply repo.json DLC content** -- Whether to apply DLC content toggles defined in the remote `repo.json`.

#### Hashing algorithm

- **Prefer Foxy (BLAKE3)** (default) -- Uses BLAKE3 hashing, which is much faster than MD5.
- **Prefer Swifty (MD5)** -- Forces legacy MD5 hashing for compatibility with older server setups.

If a repository does not support FoxyMode, Foxy uses MD5 regardless of this setting and displays a **Legacy Protocol (MD5)** warning banner.

#### Maintenance actions

- **Recheck repository integrity** -- Performs a full remote fetch and rebuilds stored checksums for all files in this repository.
- **Force redownload repository** -- Removes local files and re-downloads everything. Use with caution.
- **Wipe repository database entries** -- Clears cached metadata for this repository without deleting local files. Useful when metadata gets out of sync.
- **Delete repository** -- Removes the repository from Foxy entirely (does not delete local files from disk).

---

## Profiles

Profiles are presets that save your launch configuration for a specific repository. They store DLC toggles, basic launch parameters, addon enablement states, and additional CLI parameters. You can create multiple profiles per repository and switch between them quickly.

### Switching profiles

In the repository detail view, a **profile dropdown** appears next to the repository name (if profiles exist). Select a profile from the dropdown to switch to it. The **Default** option uses the repository's base settings without any profile.

### Creating profiles

In Repository Settings > Configuration, you can create a new profile. The new profile starts with the current repository settings as a baseline.

### Copying profiles

Use **Copy Profile** to duplicate an existing profile. This is useful when you want to start from a known-good preset and make small adjustments.

### Importing and exporting profiles

- **Export**: Copies the currently selected profile to the clipboard as JSON. You can share this with other players.
- **Import**: Reads a profile from the clipboard and adds it to the current repository. If a profile with the same name already exists, Foxy appends a suffix to avoid conflicts.

### What profiles store

Each profile independently controls:
- **Creator DLC toggles** -- CSLA, Expeditionary Forces, Global Mobilization, Reaction Forces, Spearhead 1944, S.O.G. PF, Western Sahara.
- **Basic launch parameters** -- `-skipIntro`, `-noSplash`, `-world=empty`, `-loadMissionToMemory`, `-enableHT`, `-hugePages`, `-noLogs`.
- **Additional parameters** -- A free-text field for extra CLI arguments.
- **Addon enablement** -- Which addons, optional addons, and external addons are enabled or disabled.
- **Include Steam addons** -- Whether Steam Workshop addons appear in the external addons list.

---

## Managing Addons

Repository Settings has three addon-related tabs: **Addons**, **Optional Addons**, and **External Addons**.

### Addons tab

This lists the core addons provided by the repository. Each addon is shown as a card with its name and local file path. You can:
- **Click a card** or use the **checkbox** to enable or disable the addon.
- Use **Enable all** / **Disable all** buttons to toggle all addons at once.
- **Filter** by name and **filter by state** (All, Enabled, Disabled).
- **Right-click** an addon card for a context menu with additional actions:
  - **Open addon directory** -- Opens the addon folder in your file manager.
  - **Manual addon backup** -- Creates a backup of this addon (requires a backup directory to be configured).
  - **Restore addon backup** -- Restores a previously saved backup for this addon.
  - **Recheck addon integrity** -- Rechecks integrity for this specific addon.
  - **Standalone download** -- Downloads this addon independently.
  - **Force redownload addon** -- Removes and re-downloads this specific addon.

### Optional Addons tab

This lists addons that the repository marks as optional. The interface works the same as the Addons tab. If the repository does not provide any optional addons, a message indicates this.

### External Addons tab

External addons are mods found outside the repository itself -- from other repository spaces, additional search folders you configure in Settings, or your Steam Workshop content.

Additional controls on this tab include:
- **Include Steam Addons** checkbox -- Toggles whether Steam Workshop addons appear in the list.
- **Origin filter** -- Filter by where the addon comes from (e.g., a specific search folder, Steam Workshop).
- **Group by origin** -- Organizes addons by their source folder.
- **State filter** -- Filter by Enabled / Disabled / All.
- **Refresh button** -- Rescans all known addon sources.

Right-clicking an external addon card provides an **Open addon directory** action.

---

## Launching Arma 3

### Server cards

When a repository defines servers, the repository detail view shows **server cards** below the toolbar. Each card displays:
- The server name.
- The server address and port.
- The current online/offline status (refreshed automatically or manually via the **satellite icon** button).
- Player count when available.

Use **Arrow Left** / **Arrow Right** keys to navigate between server cards.

### Launch and Join

- **Launch** -- Starts Arma 3 with the addons and settings from the currently selected profile, without connecting to a specific server.
- **Join** -- Starts Arma 3 and connects to the selected server. The Join button only works when the server is online.

### Steam auto-start

If Steam is not running when you click Launch or Join, Foxy will automatically start Steam first and wait for it to be ready before launching Arma 3. This requires the **Steam Directory** path to be configured in Settings (or auto-detected).

### Post-launch behavior

In Settings > Application, you can configure what happens after launching:
- **Close after launch** -- Foxy closes entirely after starting Arma 3.
- **Hide to tray after launch** -- Foxy minimizes to the system tray instead of closing. This option is only available when "Close after launch" is off.

---

## Application Settings

Open Settings by clicking the gear icon in the footer or header area. The Settings view has six tabs.

### Application tab

#### General options

- **Language** -- Choose between System (auto-detect), English, Czech, German, French, Spanish, Portuguese, Brazilian Portuguese, Russian, Ukrainian, Polish, Japanese, or Chinese.
- **Download Speed Limit** -- Set a maximum download speed in Mbps, or check **Unlimited** for no cap.
- **Auto backup addons before update** -- Globally enable automatic addon backups before any download.
- **Auto apply repo.json launch parameters** -- Let repositories set launch parameters via their metadata.
- **Auto apply repo.json DLC content** -- Let repositories toggle DLC content via their metadata.
- **Auto recheck repositories on launch** -- Automatically refresh all repository metadata when Foxy starts.
- **Auto quick scan for changes on launch** -- Automatically run quick local checks on all repositories when Foxy starts.

#### Utility buttons

- **Open config directory** -- Opens the folder where Foxy stores its configuration files (`settings.json`, `repositories.json`, etc.).
- **Open log folder** -- Opens the folder containing Foxy's log files, useful for troubleshooting.
- **Reset** -- Resets all settings and repositories to their default values. Use with caution.

#### Advanced options

- **Show Debug Windows** -- Enables egui debug panels for development.
- **Show memory diagnostics icon in footer** -- Adds a memory usage indicator to the footer.
- **Close after launch** / **Hide to tray after launch** -- Controls post-launch behavior (see [Launching Arma 3](#launching-arma-3)).
- **Wipe Database** -- Completely clears the internal database. This is a destructive development tool.

#### Paths

- **Arma 3 Directory** -- Path to your Arma 3 installation. Used to locate `arma3_x64.exe`.
- **Steam Directory** -- Path to your Steam installation. Supports **Browse** and **Auto-detect**.
- **Temporary Directory** -- Optional working directory for cache and intermediate files.
- **Addon Backup Directory** -- Where addon backups are stored. If empty, defaults to a `backups` folder in the Foxy config directory.

#### App Updates

Foxy can check for updates to itself from two sources:

- **Server** -- A self-hosted `foxy-app-updater.json` manifest. Enter the URL in the **Update source URL** field. If left empty, Foxy auto-detects the URL from repository-space or repository metadata.
- **GitHub** -- Fetches updates from a public GitHub repository's releases page. Enter the repository in `owner/repo` format.

Options:
- **Auto-check for updates on launch** -- Foxy checks for updates automatically when it starts.
- **Check Now** -- Manually check for updates right now.
- **Browse All Versions** -- Opens the Version Browser, which lists all available versions with changelogs and lets you upgrade, reinstall, or downgrade.

When an update is available, a badge appears in the footer linking to a changelog preview. Downloads are verified via BLAKE3 hash before installation.

---

## Backup Manager

The **Backup Manager** tab in Settings lets you manage stored addon backups. Backups are created automatically before updates (if enabled) or manually from the addon context menu in Repository Settings.

### Viewing backups

The Backup Manager shows:
- Total number of backups, unique addons tracked, and total storage used.
- The backup root directory path.
- A **Filter** field to search backups by addon name, hash, or folder name.

Backups are grouped by addon name. Each backup entry shows:
- The folder name.
- The creation date, content hash, and file size.

### Managing backups

- **Refresh** -- Rescans the backup directory for changes.
- **Open folder** -- Opens the backup root directory in your file manager.
- **Run cleanup now** -- Applies the configured retention rules to delete old backups.
- **Delete backup** -- Removes a specific backup.
- **Delete all backups** -- Removes all backups for a specific addon.

### Retention rules

- **Keep latest backups per addon** -- Set how many recent backups to keep per addon (0 = unlimited).
- **Delete backups older than N days** -- Automatically remove backups older than the specified age.

Both rules are applied when you click **Run cleanup now**. They do not run automatically.

### Restoring backups

To restore a backup, go to **Repository Settings > Addons** (or Optional Addons), right-click the addon you want to restore, and select **Restore addon backup**. Restoring is done from Repository Settings rather than the Backup Manager because Foxy needs to know the target addon path.

---

## Direct Download

The **Direct download** tab in Settings lets you download repositories, addons, or individual files from a URL without syncing them into Foxy's database.

### How to use it

1. Go to **Settings > Direct download**.
2. Click the **Direct download** button to open the download dialog.
3. Enter the **Download URL** of the content you want to download.
4. Set the **Destination folder** where files should be saved (or use the default, which falls back to your Temporary Directory, then the Foxy config directory).
5. Configure the speed limit:
   - **Use global speed limit** -- Inherits the limit from Application Settings.
   - Uncheck it to set a custom limit or select **Unlimited**.
6. Click **Direct download** to start.

### Monitoring progress

After starting a download, the Direct download tab shows the current status, source URL, destination path, and file progress. Click **Display update view** to see detailed per-file progress in the full update view.

---

## Additional Search Folders and Cleanup

### Additional search folders

The **Additional search folders** tab in Settings lets you register folders from which Foxy discovers external addons. These addons then appear in Repository Settings > External Addons.

- Click **Add new folder** and select a directory.
- Each folder can have an optional **Alias** to give it a friendly display name.
- Click the **X** button to unregister a folder (this does not delete the actual folder from disk).
- Use the **Filter** field to search your registered folders.

### Cleanup

The **Cleanup** tab shows addons that are not used by any repository and can be safely deleted. These are typically leftover folders from removed repositories or manually added content.

- Use the **Filter** field to narrow the list.
- Click the **X** button next to an addon to remove it.

---

## Customization

The **Customization** tab in Settings lets you personalize the Foxy interface without affecting any repository data.

### Font sizes

Adjust font sizes for different parts of the UI:
- **Main View** -- Window control icons, activity log toggle icon.
- **Settings View** -- Page title, close icon.
- **Repository View** -- Add repository button, toolbar icons, status banners, Launch/Join buttons.
- **Update View** -- Title, close icon, section titles, mod names, progress text, and more.
- **Repository Settings View** -- Page title, close icon, refresh icon, addon path text.

Use the sliders to increase or decrease each size. Click **Reset font sizes** to restore defaults.

### Palette colors

Customize the color scheme by adjusting:
- Primary Accent, Widget Background, Main Background, Card Background.
- Server Offline Background.
- Text Normal, Text Gray, Text Dim, Text Error.
- Log Error, Log Warning, Log Debug.
- Success, Success Muted, Action Info, Action Destructive.

Click **Reset colors** to restore the default palette.

---

## TS3 Plugins

The **TS3 Plugins** tab in Settings lets you manage TeamSpeak 3 plugin files found inside your repository addons.

### How it works

Foxy scans all your repository addon directories for TeamSpeak 3 plugin files (`.ts3_plugin`). Each discovered plugin is shown as a card with the addon it belongs to, its file path, and its current install status.

### Plugin statuses

- **Up to date** -- The plugin has been installed through Foxy and the file has not changed since.
- **Update available** -- The plugin file has changed since you last installed it (e.g., after a repository update).
- **Not installed** -- The plugin has not been installed through Foxy yet.

### Installing plugins

Click the **Install** (or **Reinstall**) button on a plugin card to open the plugin file with TeamSpeak 3. TeamSpeak must be closed before installing -- if TeamSpeak is running, a warning banner appears and install buttons are disabled.

### Recheck

Click **Recheck** to rescan all repositories for TS3 plugins and refresh the status of each one.

### Update banners

When a repository sync detects that a TS3 plugin file has changed, a banner appears on the repository detail view prompting you to install the updated plugin. You can install directly from the banner or dismiss it.

---

## Swifty Migration

If you are switching from Swifty to Foxy, the **Swifty Migration** wizard helps you import your existing repositories without losing your setup. Your Swifty data is never modified.

### Opening the wizard

The migration wizard opens automatically on first launch if Swifty data is detected on your system. You can also access it from the main view.

### What the wizard does

1. **Scans** your Swifty installation for configured repositories, detecting names, addresses, mod folder paths, and launch parameters.
2. **Detects server settings** -- If your Swifty repositories point to a server that also hosts a Foxy update manifest or repository space, the wizard auto-fills those URLs. You can edit them before importing.
3. **Detects global settings** -- The wizard picks up your Swifty Arma 3 directory and temporary directory paths and offers to apply them to Foxy if Foxy's paths are empty.
4. **Shows a list** of all detected repositories with checkboxes. Use **Select all** / **Deselect all** to quickly toggle.
5. **Imports** the selected repositories into Foxy, including launch parameters, autocheck settings, and repository space bindings.

### After importing

Foxy fetches remote metadata (servers, addons) for each imported repository automatically. If a repository space was successfully imported, repositories are bound to it and share the space's local path.

---

## Keyboard Navigation

Foxy supports keyboard navigation throughout the repository list and detail views.

| Key | Action |
|-----|--------|
| **Arrow Up** | Move selection up in the repository list |
| **Arrow Down** | Move selection down in the repository list |
| **Tab** | Move selection to the next item in the repository list |
| **Shift+Tab** | Move selection to the previous item in the repository list |
| **Arrow Left** | Select the previous server card |
| **Arrow Right** | Select the next server card |
| **Enter** | Open Repository Settings for the selected repository, or open the selected repository space |

Keyboard navigation is disabled when a modal dialog is open (such as the add-repository dialog or a confirmation prompt) or when the filter text field is focused.

---

## Troubleshooting

### Legacy Protocol (MD5) warning

If you see a yellow **Legacy Protocol (MD5)** banner on a repository, it means the server is still using the older Swifty/MD5 protocol. Foxy works fine with it, but BLAKE3 (FoxyMode) is much faster. Contact your server administrator about migrating -- hybrid mode lets the server support both Foxy and Swifty clients simultaneously.

### Interrupted or failed updates

If an update was interrupted or something looks wrong:

1. Run **Quick local check** (book icon) to detect local file drift.
2. Run **Refresh** (refresh icon) to fetch fresh metadata from the server.
3. If the problem persists, try **Recheck repository integrity** from Repository Settings > Configuration.
4. As a last resort, use **Wipe repository database entries** to clear cached metadata, then refresh again.

### Force redownload

If a repository is in a bad state and normal checks do not help, use **Force redownload repository** from Repository Settings > Configuration. This removes local files and re-downloads everything. It is a destructive operation and will prompt for confirmation.

### Activity log

Open the activity log from the **bottom-right footer button** to see what Foxy is doing. It shows current operations, recent core events, and error messages. This is the first place to check when something seems wrong.

### Log files

For deeper troubleshooting, click **Open log folder** in Settings > Application to access Foxy's log files. These contain detailed timestamped entries that can help diagnose issues.

### Wipe Database

If the internal database becomes corrupted, use **Wipe Database** in Settings > Application. This clears all cached repository data from the SQLite database. Your repositories and settings are preserved, but you will need to refresh all repositories afterward.

---

## CLI Usage

Foxy includes a full command-line interface in the same binary. When launched from a terminal, it provides automation-friendly commands with `--json`, `--quiet`, `--dry-run`, and `--yes` flags.

### Common commands

| Command | Description |
|---------|-------------|
| `Foxy.exe version` | Print the current Foxy version |
| `Foxy.exe settings show` | Display current settings |
| `Foxy.exe settings set <key> <value>` | Change a setting |
| `Foxy.exe repo list` | List all repositories |
| `Foxy.exe repo add --url <url> --path <path>` | Add a repository |
| `Foxy.exe repo sync --repo-name "Name" --mode remote-refresh` | Refresh a repository |
| `Foxy.exe sync --repo-name "Name" --mode download` | Download pending updates |
| `Foxy.exe addon list --repo-name "Name"` | List addons for a repository |
| `Foxy.exe profile list --repo-name "Name"` | List profiles |
| `Foxy.exe profile select --repo-name "Name" --profile "Profile"` | Switch active profile |
| `Foxy.exe space list` | List repository spaces |
| `Foxy.exe space sync --recheck-all` | Recheck all repositories in all spaces |
| `Foxy.exe direct-download --address <url>` | Download content directly from a URL |
| `Foxy.exe launch --repo-name "Name" --execute` | Launch Arma 3 |
| `Foxy.exe ui` | Open the desktop UI from terminal |

### Global flags

| Flag | Description |
|------|-------------|
| `--config-dir <path>` | Override the config root directory |
| `--json` | Emit machine-readable JSON output |
| `--quiet` | Reduce progress and informational output |
| `--no-progress` | Disable live progress updates (screen-reader friendly) |
| `--yes` | Confirm destructive operations automatically |
| `--dry-run` | Preview command behavior without applying changes |

### Repository selectors

Most repository-scoped commands accept either:
- `--repo-name <name>` -- Select by name (case-insensitive).
- `--repo-url <url>` -- Select by URL (normalized with trailing slash).

### Sync modes

The `repo sync` and top-level `sync` commands support these modes:
- `remote-refresh` -- Fetch latest remote metadata.
- `quick-check` -- Fast local content check.
- `recheck` -- Full recheck against remote data.
- `recheck-integrity` -- Full remote fetch and local hash recalculation.
- `download` -- Download pending updates.

For more details, run `Foxy.exe --help` or `Foxy.exe <command> --help` for any specific command.
