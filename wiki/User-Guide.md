# Foxy User Guide

Welcome to Foxy, a mod updater and launcher built for speed and reliability. Arma 3 is the reference game, and Foxy also supports Total War: WARHAMMER III, Arma Reforger, and other Steam games through a generic game module. This guide walks you through the application so you can set up repositories, keep your mods up to date, and launch your game with confidence.

The same help is available inside the app: press **F1** or click the question mark in the footer.

---

## Table of Contents

1. [Getting Started](#getting-started)
2. [Game Spaces](#game-spaces)
3. [Adding Repositories](#adding-repositories)
4. [Repository Spaces and Folders](#repository-spaces-and-folders)
5. [Syncing and Updating](#syncing-and-updating)
6. [Repository Settings](#repository-settings)
7. [Profiles](#profiles)
8. [Managing Addons](#managing-addons)
9. [Launching the Game](#launching-the-game)
10. [Steam Workshop](#steam-workshop)
11. [Extra Files and Config Packs](#extra-files-and-config-packs)
12. [Editor Missions](#editor-missions)
13. [Application Settings](#application-settings)
14. [Game Space Settings](#game-space-settings)
15. [Scheduling](#scheduling)
16. [Benchmarks](#benchmarks)
17. [Backup Manager](#backup-manager)
18. [Direct Download](#direct-download)
19. [Cleanup](#cleanup)
20. [Customization](#customization)
21. [TS3 Plugins](#ts3-plugins)
22. [Swifty Migration](#swifty-migration)
23. [Keyboard Navigation](#keyboard-navigation)
24. [Troubleshooting](#troubleshooting)
25. [CLI Usage](#cli-usage)

---

## Getting Started

### First launch

When you open Foxy, you see the repository list on the left side of the window and a detail panel on the right. The panel at the top of the sidebar shows the open **game space** (see [Game Spaces](#game-spaces)). The repository list stays empty until you add your first repository.

The footer at the bottom of the window contains:
- An **activity log toggle** (bottom right) that shows what Foxy is doing and recent core messages, which is useful for troubleshooting.
- An **info icon** that opens the About page.
- A **question mark icon** that opens the in-app Help page.
- The **version number**, which opens the changelog when clicked.
- An **update badge** when a newer Foxy version is available.

### Initial setup

1. **Check that the right game is open.** The top of the sidebar shows the active game space. Use **Switch** there to create or open a game space for another game. On a first start, or after upgrading from an older Foxy, an Arma 3 game space is ready for you.
2. **Set the game directory.** Open **Game space settings** from the gear icon at the top of the sidebar and point the game directory (for example **Arma 3 Directory**) to your installation, or click **Auto-detect** to find it from your Steam library.
3. **Optionally set app-wide paths** in **Settings > Application**:
   - **Steam Directory** - your Steam installation folder, or click **Auto-detect**. This enables Steam Workshop discovery and the Steam check before launch.
   - **Temporary Directory** - Foxy uses this for cache and intermediate files. If left empty, it defaults to the Foxy config directory (`%APPDATA%\Foxy` on Windows, `~/.config/Foxy` on Linux).
   - **Addon Backup Directory** - where addon backups are stored. If left empty, Foxy uses a `backups` folder inside its config directory.
4. If your Arma 3 profile files are stored in Documents or OneDrive, set **Arma 3 Profiles Directory** in Game space settings so Foxy launches with `-profiles=<path>` and avoids cloud-sync conflicts.

Foxy warns when a path you choose is inside a OneDrive folder, because cloud sync can lock or corrupt files Foxy and the game are writing.

### Swifty compatibility

If you are coming from Swifty, Foxy is fully backwards compatible with Swifty repositories. No server changes are required: Foxy detects the legacy MD5 protocol automatically and syncs accordingly. The [Swifty Migration](#swifty-migration) wizard can import your existing setup.

---

## Game Spaces

A **game space** is a separate workspace for one game, with its own repositories, repository spaces, game settings, mod stores, benchmarks, and database. Exactly one game space is open at a time, and Foxy reopens the last one on the next launch.

### Supported games

| Game | What Foxy does |
|------|----------------|
| **Arma 3** | Repositories, profiles, server join, Creator DLCs, Steam Workshop, TeamSpeak 3 plugins, editor missions. |
| **Total War: WARHAMMER III** | Repositories of `.pack` mods, Steam Workshop management, and launch through the `used_mods.txt` mods manifest, including continuing a campaign save from the command line. |
| **Arma Reforger** | Repositories, launch with `-addons`/`-addonsDir` built from the enabled addons, server join with `-client`, and Workshop addons managed by GUID from the command line. |
| **Generic game** | Any other Steam game: you set the executable, launch arguments, mods manifest file, and optional Steam App ID yourself. |

### The game space panel

The panel at the top of the sidebar shows the open game space:
- **Switch** opens the **Game spaces** list.
- The **gear icon** opens **Game space settings**.
- The **game logo** opens the **game space overview**, which shows repositories, repository spaces, unique addons, mods on disk, last launch and update times, TeamSpeak 3 status, and your recent benchmarks.

### Creating, switching, and removing game spaces

- In the Game spaces list, click **Create game space**, choose the game, and enter a name. You can create several game spaces for the same game to keep separate setups apart.
- **Open game space** switches right away without restarting Foxy. Pending changes are saved first. Foxy does not switch while downloads or scans are running, so let them finish first.
- **Remove game space** deletes only the Foxy workspace: its repository list, game settings, and local database. Game installs and downloaded mods stay on disk.
- **Rename** in Game space settings changes only the name shown in Foxy. The game space folder and its data stay where they are.

### What is shared and what is per game

Language, renderer, backups, and the other **Application** settings are shared by all game spaces. Scheduled jobs, cleanup folders, additional search folders, and pending updates belong to the game space they were created in.

Each game space stores its data under `games/<game space>` in the Foxy config directory. Only one Foxy window or command can use a game space at a time.

### Upgrading from an older Foxy

When you upgrade from a Foxy version without game spaces, your existing setup moves into an Arma 3 game space automatically. The previous files are kept as `.pre-gamespaces.bak` copies, so the change can be rolled back.

---

## Adding Repositories

To add a repository:

1. Click **+ Add repository** at the top of the sidebar.
2. Paste either:
   - A **repository URL** (the direct address of the repository).
   - A **repository space URL** (a manifest that lists several repositories, see the next section).
3. Choose the **local folder** where the repository files should be stored. This is where Foxy checks files, downloads updates, and launches from.
4. Confirm to add the repository.

If you pasted a repository space URL, Foxy shows the repositories in that space and lets you choose which ones to add and where they should be stored. A repository URL always ends in a trailing slash; Foxy adds it for you.

### Duplicate detection

If you add a repository that already exists, Foxy asks whether you want to add it again. The same repository URL installed to two different folders is treated as two independent installs, each with its own state.

### Filtering and ordering repositories

- Use the **Filter repositories** field below the add button to search by name. The sidebar shows how many repositories match.
- Drag and drop repository cards to reorder them, and keep the repositories you use most near the top.

---

## Repository Spaces and Folders

### Repository spaces

Repository spaces group several repositories under one shared manifest. Communities use them to publish related repositories (for example a main modset, a training modset, and an optional extras modset) under one URL.

When you add a repository space, it appears as a collapsible section in the sidebar under **Spaces**. Click the space header to open its detail page, which shows:
- The space name and shared path.
- A list of **Available repositories** defined by the space, each with an **Add** button and a filter field.
- A **Matching existing repositories** section that can associate repositories you added earlier with the space.

Foxy rereads each space's manifest from the server at launch and every 30 minutes while it runs, so repositories the community adds to the space appear in **Available repositories** without re-adding the space. To reread it right away, click the globe **Refresh from server** button on the space page, or right-click the space in the sidebar and choose **Refresh from server**. A refresh updates the list of repositories the space offers, its name, and its images; it never installs or removes a repository on your disk.

### Shared paths

A repository space can define a common local folder for all its repositories. When a repository belongs to a space, its local path is inherited from the space's shared path and cannot be changed individually. Required repositories of the space are added automatically and see the same shared folder.

### Bulk operations

The repository space toolbar operates on every repository in the space at once:

- **Recheck all repositories** - runs a remote data refresh on every repository in the space. A confirmation dialog lists the repositories first.
- **Quick local check** - runs a quick local content check on all repositories in the space.
- **Update all repositories** - downloads pending updates for every repository in the space that has updates.

A progress indicator in the space header shows how many repositories are done and which one is active.

### Scanning and moving existing repositories

If you added repositories individually before adding the space, click **Scan existing repositories** to find matches, select the ones you want, and click **Move selected repositories**.

### Visual folders

Visual folders group repositories in the sidebar without changing where their files live. You can name, color, and collapse a folder, drag repositories into it, and run quick check, recheck, or update for everything inside it. When you delete a folder, you can choose whether to also remove the repositories it contains.

---

## Syncing and Updating

After adding a repository, check it against the remote server and download any updates. The check actions are in the repository toolbar.

### Refresh (remote data recheck)

Click the **refresh icon** to fetch the latest metadata from the remote server and build an update plan. This is the main way to see whether your local files are up to date.

### Quick local check

Click the **book icon** to run a quick local content check. It compares local content hashes (BLAKE3 fingerprints) to detect files that changed on disk, without contacting the server. It is fast and useful for catching local drift, for example after you edited a file by hand.

### Recheck repository integrity

Available from **Repository Settings > Configuration**, this performs a full remote metadata fetch and rebuilds all stored checksums by reading every file. Use it after major local changes or suspected corruption.

### Understanding the update flow

1. Run **Refresh** to check the remote server.
2. If updates are available, an **Update ready** banner appears on the repository.
3. Click the banner to open the **update view**, which shows which mods changed, how many files are affected per mod, and the total download size.
4. Start the download. A progress bar and status banner track the work, and the most active mods move to the top of the list.
5. When the download completes, the repository shows **Synced**.

Before downloading, Foxy checks that the destination has enough free space and is on a drive it can use safely. If space runs short, Foxy tells you how much is missing and on which drive, and does not start writing.

### Delta patching

When possible, Foxy downloads only the changed parts of a file instead of the whole file. If a patch fails validation, Foxy falls back to downloading the full file automatically.

### Cancelling

You can cancel an active sync while Foxy is hashing or downloading. Foxy stops pending work, cleans up temporary download parts, and returns to a clean idle state.

### Progress and status banners

During any operation, the repository detail view shows a status banner with the operation name, the current step, a detail line, an elapsed time counter, and a progress bar when applicable. After the operation, a completed banner shows the result; dismiss it or use its action button (for example to review an update).

---

## Repository Settings

Open repository settings with the **gear icon** in the repository toolbar, or press **Enter** when a repository is selected. The view has four tabs: **Configuration**, **Addons**, **Optional Addons**, and **External Addons**.

### Configuration tab

#### Identity

- **Name** - the display name for this repository.
- **Address** - the remote URL of the repository.
- **Local Path** - the folder for this repository's files. If the repository belongs to a space, the path is inherited from the space and cannot be edited here.

#### Sync and display settings

Each of these can be **Use global** (inherits from Application or Game space settings), **On (override)**, or **Off (override)**. Only the settings that apply to the open game appear:

- **Auto recheck on launch** - refresh this repository's remote data when Foxy starts.
- **Auto quick scan on launch** - run a quick local check on this repository when Foxy starts.
- **Auto backup addons before update** - back up changed addons before downloading updates.
- **Auto apply repo.json launch parameters** - apply launch parameters published by the repository.
- **Auto apply repo.json DLC content** - apply the Creator DLC selection published by the repository (Arma 3).
- Showing or hiding the repository image, the **Editor Missions** list, and the **Servers** list.

While **Auto apply repo.json launch parameters** is on, the launch parameter fields are read-only and marked as managed by `repo.json`, because the repository overwrites them on every refresh. Turn the setting off, or select a launch profile, to edit them.

#### Hashing algorithm

- **Prefer Foxy (BLAKE3)** (default) - fast BLAKE3 hashing.
- **Prefer Swifty (MD5)** - forces legacy MD5 hashing for older server setups.

If a repository does not support FoxyMode, Foxy uses MD5 regardless of this setting and shows a **Legacy Protocol (MD5)** warning banner.

#### Maintenance actions

- **Recheck repository integrity** - full remote fetch and a rebuild of the stored checksums for every file.
- **Force redownload repository** - removes local files and downloads everything again. Foxy first checks that the repository can be reached, so an offline server never leaves you with deleted files and nothing to download.
- **Wipe repository database entries** - clears cached metadata for this repository without deleting local files.
- **Delete repository** - removes the repository from Foxy (local files stay on disk).

---

## Profiles

There are two kinds of profiles in Foxy.

### Launch profiles (per repository)

Launch profiles are presets for how the game starts with a specific repository. They store Creator DLC toggles, launch parameters, addon enablement, and extra command-line parameters, so you can switch between servers, unit setups, or optional addon combinations quickly.

- **Switching**: a profile dropdown appears next to the repository name when profiles exist. **Default** uses the repository's base settings.
- **Creating**: in Repository Settings > Configuration, create a new profile. It starts from the current repository settings.
- **Copy Profile** duplicates an existing profile.
- **Export** copies the selected profile to the clipboard as JSON; **Import** adds a profile from the clipboard. A name clash gets a suffix.

Each profile controls:
- **Creator DLC toggles** (Arma 3) - CSLA, Expeditionary Forces, Global Mobilization, Reaction Forces, Spearhead 1944, S.O.G. Prairie Fire, Western Sahara.
- **Basic launch parameters** - for Arma 3: `-skipIntro`, `-noSplash`, `-world=empty`, `-loadMissionToMemory`, `-enableHT`, `-hugePages`, `-noLogs`. Other games show their own set.
- **Additional parameters** - free text for extra command-line arguments.
- **Addon enablement** - which addons, optional addons, and external addons are enabled.
- **Include Steam addons** - whether Steam Workshop addons appear in the external addons list.

Profile names support UTF-8 text, so non-English names display and launch correctly.

### Arma 3 player profiles

The **Profiles** tab in Game space settings manages your Arma 3 player profiles: clone, rename, or delete them. Deleted profiles are moved to the Foxy backup directory, default profiles are protected, and changes are blocked while Arma 3 is running.

---

## Managing Addons

Repository Settings has three addon tabs: **Addons**, **Optional Addons**, and **External Addons**.

### Addons tab

Lists the addons provided by the repository. Each addon is a card with its name and local path. You can:
- **Click a card** or its **checkbox** to enable or disable the addon.
- Use **Enable all** / **Disable all**.
- **Filter** by name, by state (All, Enabled, Disabled), or by favorites, and mark addons as favorites.
- Turn on **Search addon files** to match the filter against files inside addon folders; matching addons expand automatically.
- **Right-click** a card for more actions:
  - **Open addon directory**
  - **Manual addon backup** (requires a backup directory)
  - **Restore addon backup**
  - **Recheck addon integrity**
  - **Standalone download**
  - **Force redownload addon**

### Optional Addons tab

Lists addons the repository marks as optional. It works like the Addons tab. Your optional addon choices persist across refreshes and restarts, so a disabled optional addon is never silently re-enabled or re-downloaded.

### External Addons tab

External addons are mods found outside the repository: in repository-space shared paths, in additional search folders you register in Game space settings, or in your Steam Workshop content. The tab adds:
- **Include Steam Addons** - toggles Steam Workshop addons in the list.
- **Origin filter** and **Group by origin**.
- **State filter** (All, Enabled, Disabled).
- A **Refresh** button that rescans every addon source.

### Client-side addons

For Arma 3, a repository can mark addons as client-side (for example UI mods the server does not run). Foxy uses that to explain what it is about to enable when you join a server.

---

## Launching the Game

### Server cards

When a repository defines servers, the detail view shows **server cards** with each server's name, address, online status, and player count. Refresh the status with the **satellite icon**. Use **Arrow Left** / **Arrow Right** to move between cards.

### Launch and Join

- **Launch** starts the game with the addons and settings of the selected profile.
- **Join** starts the game and connects to the selected server. Offline servers have no Join action.

When you join an Arma 3 server, Foxy reads what the server reports and can:
- warn when the server needs addons that you have locally but disabled for this launch, and
- offer to **match the server's Creator DLCs**: enable the Creator DLCs the server requires and disable enabled ones it does not use, for that launch only.

### How each game launches

- **Arma 3** - Foxy builds the `-mod=` line from the enabled addons and Creator DLCs and adds your launch parameters.
- **Total War: WARHAMMER III** - Foxy writes the enabled mods to `used_mods.txt` in the game directory before launch.
- **Arma Reforger** - Foxy builds the `-addons` and `-addonsDir` parameters from the enabled addons, and passes a selected server as `-client <address>:<port>`. Packed addon folders containing a `.gproj` and root-level `.pak` files appear in the addon list without needing an `addons` subfolder. The Arma 3 editor mission list is hidden in this game space.
- **Generic game** - Foxy uses the executable, argument template, and mods manifest you set in Game space settings.

Enabled [extra files](#extra-files-and-config-packs) are copied into the game directory before each launch.

### Steam and TeamSpeak checks

With **Check Steam is running before launching** turned on in Game space settings, Foxy warns when Steam is not running and offers to start it. **Check TeamSpeak is running before joining** does the same for TeamSpeak 3 when the repository ships a TS3 plugin. Repositories can override both checks.

Foxy never runs the game with administrator rights, even if Foxy itself was started as administrator, so Discord, TeamSpeak hotkeys, and OBS capture keep working in the game window.

### After launch

In **Settings > Application**:
- **Close after launch** closes Foxy after the game starts.
- **Hide to tray after launch** minimizes Foxy to the system tray instead. It is available only when Close after launch is off.

---

## Steam Workshop

For Steam games, Foxy manages Steam Workshop mods next to repository content. Open **Game space settings** and choose the **Steam Workshop** tab while that game space is open. The game space needs a Steam App ID: built-in games have one already, and for a Generic game you set it in Game space settings.

### Adding and sharing mods

- **Import share code** accepts Workshop ids, Workshop links, collections, or a pipe-separated list from a friend. Keep **Subscribe and download through Steam** on to download the mods, and turn on **Freeze each mod after downloading** to pin them right away.
- **Copy share code** copies your enabled mods as a pipe-separated list that other mod managers understand.
- **Compare with** checks a friend's share code against your setup. Foxy shows whether your enabled mods match, whether the load order differs, and offers **Import missing** to download only the mods you do not have.
- The **state checksum** covers your enabled mods, the game build, and load order. Click it to copy; a friend with the same checksum runs the same setup.

### Managing the list

- Enable or disable each mod, and use **Load earlier** / **Load later** to change the load order.
- **Freeze** keeps a private copy of a mod at its current version so a Workshop update cannot change your setup. **Unfreeze** follows Steam updates again, and **Freeze all** freezes every mod at once.
- **Export bundle** writes a `.foxyshare` file with the mod list and every frozen copy. **Import bundle** reads one a friend sent you.
- **Refresh** fetches titles, sizes, and update times from Steam; **Open** shows a mod's Workshop page.
- **Remove** takes a mod out of this game space. Turn on **Also delete the downloaded files and frozen copies** to free the disk space too.

Arma Reforger Workshop addons are managed by GUID on the command line with `foxy game reforger`, including addon freezing.

---

## Extra Files and Config Packs

- **Managed extra files** store config folders and other files that are not mods inside Foxy, and copy the enabled ones into the game directory before each launch. Manage them with `foxy config extra-file list|add|remove|set|activate`.
- A **`.foxypack` config pack** exports a game space's repositories, Workshop ids, profiles, and extra files so you can import them on another machine. Packs reference mods by id and never carry mod files. Use `foxy config export <file>` and `foxy config import <file>`.

Both are command-line features for now.

---

## Editor Missions

For Arma 3, editor missions are shown inside the repository view, close to the addons, profiles, and servers they depend on.

- Open, duplicate, or delete detected singleplayer and multiplayer mission folders, or launch Eden Editor for them. Mission subfolders are scanned recursively.
- Use the terrain filter next to **Show folders** to narrow the list to one map.
- Use the mission context menu to remove addon dependencies from `mission.sqm`.
- If additional or external addons are enabled, Foxy warns before launching Eden Editor, because saving can write those dependencies into `mission.sqm`. You can launch with addons, launch without additional/external addons, or cancel.

Game space settings can hide the Editor Missions list, and Repository Settings can override that per repository.

---

## Application Settings

Open Settings with the gear icon or **F2**. The Settings view has these tabs: **Application**, **Benchmarks**, **Cleanup**, **Direct download**, **Backup Manager**, **Scheduling**, and **Customization**. Everything that belongs to one game lives in [Game Space Settings](#game-space-settings) instead.

### Application tab

#### General options

- **Language** - System (auto-detect) or one of more than 40 bundled languages, including right-to-left languages.
- **Download Speed Limit** - a maximum download speed in Mbps, or **Unlimited**.
- **Auto backup addons before update** - back up addons before any download.
- **Auto recheck repositories on launch** - refresh all repository metadata when Foxy starts.
- **Auto quick scan for changes on launch** - run quick local checks on all repositories when Foxy starts.
- **Hide repository image** - hide the banner image at the top of repository and repository space views.
- **Close after launch** / **Hide to tray after launch** - see [Launching the Game](#launching-the-game).

#### Performance options

- **Renderer** - **Auto (WGPU)** (recommended), **WGPU**, or **Glow**. Auto uses WGPU but lets Foxy switch to Glow (OpenGL) after a graphics crash. Changes take effect after a restart.
- **Hashing profile** - **Auto** benchmarks the first hashing work and picks the fastest profile; **Conservative**, **Balanced**, and **Aggressive** set the concurrency yourself.
- **Skip unchanged files after a database reset** - after a database reset, trust files Foxy already verified and that Windows shows as untouched since, instead of reading them again. An integrity recheck always reads every file.

#### Diagnostics

- **Extended diagnostics logging** - writes detailed hashing, download, database, disk, and memory diagnostics to the log files. Turn it on when you report a performance problem.
- **Show FPS counter** and **Show memory diagnostics icon in footer** - help diagnose UI performance and memory use. **F4** opens memory diagnostics.
- **Show Debug Windows** - developer debug panels.

#### Utility buttons

- **Open config directory** - opens the folder with Foxy's configuration, settings, and database files.
- **Open log folder** - opens the folder with Foxy's log files.
- **Export logs to ZIP** - packages the log files into a timestamped ZIP archive for support.
- **Migrate from Swifty** - opens the [Swifty Migration](#swifty-migration) wizard.
- **Reset all settings** - resets all settings to their defaults. Repository data is cleared too.
- **Wipe Database** - removes all cached repository data from the database, optionally also deleting saved benchmarks. Use it only as a last resort.

#### Paths

- **Steam Directory** - supports **Browse** and **Auto-detect**.
- **Temporary Directory** - optional working directory for cache and intermediate files.
- **Addon Backup Directory** - where addon backups go. If empty, a `backups` folder in the Foxy config directory.

#### App Updates

Foxy can check for updates to itself from two sources:

- **Server** - a self-hosted `foxy-app-updater.json` manifest. Enter it in **Update source URL**. If the field is empty, Foxy fills it from repository-space metadata first and repository metadata second. A value you type yourself is treated as your override.
- **GitHub** - the releases of a public GitHub repository, in `owner/repo` format.

Options:
- **Auto-check for updates on launch**.
- **Check Now** - check right away.
- **Browse All Versions** - opens the Version Browser to upgrade, reinstall, or downgrade to any published release.

When an update is available, Foxy shows a prompt at startup (it asks again on every launch until the update is installed) and a red update badge in the footer. Downloads are verified with a BLAKE3 hash before installation.

---

## Game Space Settings

Open Game space settings from the gear icon at the top of the sidebar. The tabs that appear depend on the game:

- **Game tab** (named after the game) - the game space name, the game directory with **Auto-detect**, and the settings for that game. For Arma 3 these are **TeamSpeak 3 Directory**, **Arma 3 Profiles Directory**, **Auto apply repo.json launch parameters**, **Auto apply repo.json DLC content**, **Warn before launching editor with external addons**, **Show Editor Missions list**, **Show Servers list**, **Check server addons before joining**, **Check TeamSpeak is running before joining**, and **Check Steam is running before launching**. A Generic game adds the executable, launch arguments, mods manifest, and Steam App ID. Repositories inherit these toggles unless they override them.
- **Profiles** - Arma 3 player profiles (see [Profiles](#profiles)).
- **Steam Workshop** - see [Steam Workshop](#steam-workshop).
- **Additional search folders** - folders where Foxy discovers external addons. Click **Add new folder**, give it an optional **Alias**, and use **X** to unregister it (the folder on disk is not deleted).
- **TS3 Plugin** - see [TS3 Plugins](#ts3-plugins).

You can open the settings of a game space that is not open. Changes are saved to that game space and take effect when you open it. Profiles, Steam Workshop, and TS3 Plugin need the game space to be open.

---

## Scheduling

The **Scheduling** tab in Settings runs rechecks and update downloads at a chosen time.

- Jobs can run once or on recurring weekdays, for a single repository, a repository space, or a custom selection of repositories.
- Each job can be enabled, disabled, edited, deleted, or started right away with **Run now**.
- A job can close Foxy or shut down the PC when it finishes, optionally only if everything succeeded. Shutdown starts a 60-second countdown that you can cancel.
- Jobs only run while Foxy is open, and they belong to the game space they were created in.

---

## Benchmarks

Benchmarks record how a recheck, update, or redownload performed on your computer, so you can compare runs over time or attach them when you report a performance problem. They are stored per game space, together with their metrics, charts, and the matching slice of the log.

### Recording a benchmark

1. Open **Settings > Benchmarks** and turn on **Enable benchmarks**. Extended diagnostics logging stays on while benchmarks are enabled, so the log files grow faster.
2. Start a recheck, update, or redownload yourself.
3. When it finishes, Foxy asks whether to save the run. Choose **Save benchmark** or **Discard**.

### Reviewing benchmarks

- The Benchmarks tab lists your saved runs. Use **Search benchmarks**, the action, outcome, and repository filters, and the sort order to find a run.
- Open a benchmark to see stage durations, files checked, download sizes and speeds, patch savings, peak memory, average CPU, the power source and power plan, and charts for CPU, memory, disk writes, and transfer rates.
- **Speed of light** shows how close each operation ran to its reference, such as a bound, the run's own peak, or a nominal device rate. Cancelled or failed runs are excluded from best-case comparison.
- **Add notes about this run** and **Save notes** let you remember what was different, for example another disk, network, or hashing profile.
- Mark runs as **Favourite**, hide them with **Hide benchmark**, or delete them with **Remove benchmark**. **Show hidden** brings hidden runs back.

### Comparing and sharing

- Select two benchmarks and choose **Compare selected** to see them side by side. The older run is A by default; **Oldest as A** controls that, and **Swap A and B** exchanges the sides.
- **Export benchmark to ZIP** packages a benchmark with its charts and log slice for support. The file is named after the Foxy version, start time, and operation. **Open benchmarks folder** shows the stored files.
- The game space overview lists your recent benchmarks; **Open benchmarks** jumps to the tab.

---

## Backup Manager

The **Backup Manager** tab manages stored addon backups. Backups are created automatically before updates (if enabled) or manually from the addon context menu in Repository Settings.

### Viewing backups

The Backup Manager shows the total number of backups, unique addons tracked, total storage used, and the backup root directory. A **Filter** field searches by addon name, hash, or folder name. Backups are grouped by addon, and each entry shows its folder name, creation date, content hash, and size.

### Managing backups

- **Refresh** rescans the backup directory.
- **Open folder** opens the backup root directory.
- **Run cleanup now** applies the retention rules.
- **Delete backup** removes one backup; **Delete all backups** removes all backups for one addon.

### Retention rules

- **Keep latest backups per addon** - how many recent backups to keep per addon (0 = unlimited).
- **Delete backups older than N days** - remove backups older than the given age.

Both rules apply when you click **Run cleanup now**; they do not run on their own.

### Restoring backups

Go to **Repository Settings > Addons** (or Optional Addons), right-click the addon, and choose **Restore addon backup**. Restoring happens there because Foxy needs to know the target addon path.

---

## Direct Download

The **Direct download** tab downloads repositories, addons, or individual files from a URL without syncing them into Foxy's database.

1. Go to **Settings > Direct download** and click **Direct download**.
2. Enter the **Download URL**.
3. Set the **Destination folder** (it defaults to the Temporary Directory, then the Foxy config directory).
4. Keep **Use global speed limit**, or uncheck it to set a custom limit or **Unlimited**.
5. Start the download.

The tab then shows the status, source URL, destination, and file progress. **Display update view** opens the full per-file progress view. Direct downloads get the same free-space and drive checks as repository updates.

---

## Cleanup

The **Cleanup** tab lists addons that no repository uses any more, such as leftovers from removed repositories. Filter the list and click **X** next to an addon to remove it.

---

## Customization

The **Customization** tab personalizes the interface without touching repository data.

- **Themes** - apply a built-in preset, save the current look as a named theme, load or delete saved themes, and **Import theme...** / **Export theme...** to share a theme file.
- **UI scale** - make the whole interface larger or smaller.
- **Font sizes** - adjust sizes per view (main, settings, repository, update, repository settings, help, about). **Reset font sizes** restores the defaults.
- **Palette colors** - adjust accent, backgrounds, text, log, success, and action colors. **Reset colors** restores the defaults.

---

## TS3 Plugins

The **TS3 Plugin** tab in Game space settings (Arma 3) manages TeamSpeak 3 plugin files found in your repository addons.

- Foxy scans your repository addon folders for `.ts3_plugin` files. Each plugin card shows the addon it belongs to, its file path, and its status: **Up to date**, **Update available**, or **Not installed**.
- **Install** (or **Reinstall**) opens the plugin with TeamSpeak 3. TeamSpeak must be closed first; while it runs, a warning appears and the install buttons are disabled.
- **Recheck** rescans for plugins.
- When a sync changes a plugin file, a banner on the repository offers to install the update.

---

## Swifty Migration

If you are switching from Swifty, the **Swifty Migration** wizard imports your existing repositories without changing your Swifty data. It opens automatically on first launch when Swifty data is found, and from **Settings > Application > Migrate from Swifty**.

The wizard:
1. **Scans** your Swifty installation for repositories, their names, addresses, mod folders, launch parameters, and Creator DLC selections.
2. **Detects server settings** - if your repositories point to a server that also hosts a Foxy update manifest or repository space, it fills in those URLs. You can edit them before importing.
3. **Detects global settings** such as the Arma 3 and temporary directories, and offers to apply them when Foxy's are empty.
4. **Lists** all detected repositories with checkboxes (**Select all** / **Deselect all**).
5. **Imports** the selection, including launch parameters, autocheck settings, and repository space bindings.

If a legacy setup has several entries from the same source, Foxy can migrate them as separate profile variants. After importing, Foxy fetches remote metadata for each repository automatically.

---

## Keyboard Navigation

| Key | Action |
|-----|--------|
| **F1** | Open Help from any screen |
| **F2** | Open or close Settings |
| **F3** | Show or hide the activity log |
| **F4** | Open memory diagnostics (when its footer icon is enabled) |
| **Tab** / **Shift+Tab** | Move focus between controls |
| **Enter** | Activate the focused control, open Repository Settings for the selected repository, or open the selected repository space |
| **Arrow Up** / **Arrow Down** | Move through repository lists, addon lists, and other vertical lists |
| **Arrow Left** / **Arrow Right** | Move between server cards |
| **Escape** | Close dialogs, Help, Settings, and other overlay panels |

List navigation pauses while a dialog is open or a filter field has focus. When a startup prompt is open, **Enter** confirms its primary action. Repeated clicks on Launch, Update, and other long-running actions are ignored while the action is starting or running.

---

## Troubleshooting

### My repositories are missing

Check which game space is open at the top of the sidebar. Each game space has its own repository list.

### Another Foxy is using this game space

Only one Foxy window or command can use a game space at a time. If Foxy reports that another process is using the game space's database, close the other Foxy window or command first.

### Foxy asks to wipe and rebuild the database

After an update, the stored database may have been made by an incompatible version. Rebuilding keeps your downloaded mods on disk, and Foxy rechecks your repositories afterward. Files Foxy already verified and that have not changed are restored from its verified-hash record instead of being read again, unless you turned off **Skip unchanged files after a database reset**.

### Storage check notice

At startup Foxy checks the drives behind its paths and repositories. It warns about risky setups (a FAT32 or exFAT drive, a network share, a RAM disk, a read-only drive, or paths too long for Windows) and refuses to download where files cannot be stored safely, for example a file over 4 GiB on FAT32. Move the folder to a local NTFS drive, or follow the notes in the notice.

### Not enough free space

If an update does not fit on the drive, Foxy shows the update size, the free space, and how much is missing. Free up space or move the repository, then start the update again.

### Legacy Protocol (MD5) warning

A yellow **Legacy Protocol (MD5)** banner means the server still uses the older Swifty/MD5 protocol. Foxy works fine with it, but BLAKE3 (FoxyMode) is much faster. Ask your server administrator about migrating; hybrid mode lets a server support Foxy and Swifty clients at the same time.

### Interrupted or failed updates

1. Run **Quick local check** (book icon) to detect local file drift.
2. Run **Refresh** (refresh icon) to fetch fresh metadata.
3. If the problem persists, use **Recheck repository integrity** in Repository Settings > Configuration.
4. As a last resort, use **Wipe repository database entries**, then refresh again.

If downloads fail repeatedly, check your connection, open the repository URL in a browser, and read the activity log for the specific error.

### Force redownload

If a repository stays in a bad state, use **Force redownload repository** in Repository Settings > Configuration. It asks for confirmation, checks that the server can be reached, then removes local files and downloads everything again.

### Steam Workshop tab missing or empty

Open that game space first and make sure it has a Steam App ID in Game space settings.

### External addons not detected

Check the additional search folders in Game space settings, and turn on **Include Steam Addons** if you use Workshop content.

### Launch fails or the wrong addons load

Check the selected profile in Repository Settings, the addon paths, and the Creator DLC toggles. If Foxy rejects a profile path, check whether it is inside OneDrive or contains characters that are unsafe for file handling.

### Foxy does not start, or starts with Glow

If Foxy crashes inside the graphics driver during startup, the next launch automatically tries a safer graphics setup, and if that also fails it switches the renderer to Glow and shows a notice explaining why. Keep the renderer on **Auto** unless you have a reason to force WGPU.

Overlays such as NVIDIA GeForce Experience or NVIDIA App, Discord, Steam, Bandicam, OBS, and Xbox Game Bar can attach to Foxy and cause pop-ups or rendering problems. Foxy is a desktop app, not a game, so you can exclude it from these overlays without losing anything. If the logs mention Vulkan layers such as `VK_LAYER_bandicam_helper`, update or disable that tool and restart Foxy. The in-app Help has per-tool steps under **Third-party overlays**.

### Slow checks or updates

Turn on **Enable benchmarks** in Settings > Benchmarks, repeat the action, save the benchmark, and share it with **Export benchmark to ZIP**.

### Activity log and log files

Open the activity log from the bottom-right footer button to see current work, recent core events, and errors. For deeper troubleshooting, use **Open log folder** or **Export logs to ZIP** in Settings > Application. Log files older than 90 days are removed automatically, and Foxy keeps up to 16 recent log files.

---

## CLI Usage

Foxy includes a full command-line interface in the same binary (`Foxy.exe` on Windows, `foxy` below). It uses the same config directory and game spaces as the desktop app, and works on the active game space.

### Common commands

| Command | Description |
|---------|-------------|
| `foxy version` | Print the Foxy version, build kind, and source commit |
| `foxy settings show` / `set` / `reset` | Inspect or change settings |
| `foxy repo list` | List repositories |
| `foxy repo add --address <url> --path <path>` | Add a repository |
| `foxy repo sync --repo-name "Name" --mode remote-refresh` | Refresh a repository |
| `foxy sync --repo-name "Name" --mode download` | Download pending updates (`sync` is short for `repo sync`) |
| `foxy repo force-redownload --repo-name "Name" --yes` | Remove local files and download everything again |
| `foxy addon list --repo-name "Name"` | List addons for a repository |
| `foxy profile list --repo-name "Name"` | List launch profiles |
| `foxy profile select --repo-name "Name" --profile "Profile"` | Switch the active profile |
| `foxy space list` | List repository spaces |
| `foxy space sync --space-id <id> --recheck-all` | Recheck every repository in a repository space |
| `foxy game list` / `use <id>` / `create <name> --game <game>` / `remove <id>` | Manage game spaces |
| `foxy game launch --execute` | Launch a game space that has no repositories (Total War: WARHAMMER III, Generic game) |
| `foxy game reforger ...` | Manage Arma Reforger Workshop addons by GUID |
| `foxy workshop ...` | Add, import, enable, order, freeze, share, and remove Steam Workshop items |
| `foxy config export <file>` / `import <file>` | Export or import a `.foxypack` config pack |
| `foxy config extra-file ...` | Manage extra files |
| `foxy direct-download --address <url>` | Download content by URL without a database sync |
| `foxy launch --repo-name "Name" --execute` | Launch the game with a repository and profile |
| `foxy ui` | Start the desktop app from a terminal |

Run `foxy --help` to list every command, and add `--help` after any command to see its options.

### Global flags

| Flag | Description |
|------|-------------|
| `--config-dir <path>` | Use a different config directory (also `FOXY_CONFIG_DIR`) |
| `--json` | Machine-readable JSON output |
| `--quiet` | Less progress and informational output |
| `--no-progress` | No live progress updates (screen-reader friendly) |
| `--yes` | Confirm destructive operations |
| `--dry-run` | Preview a change without writing it |

### Repository selectors

Most repository commands accept either `--repo-name <name>` (case-insensitive) or `--repo-url <url>` (normalized with a trailing slash).

### Sync modes

`repo sync` and `sync` support:
- `remote-refresh` - fetch the latest remote metadata.
- `quick-check` - fast local content check.
- `recheck` - full recheck against remote data.
- `recheck-integrity` - full remote fetch and local hash recalculation.
- `download` - download pending updates.

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `2` | Validation error |
| `3` | Not found |
| `4` | Operation failed |
| `5` | Partial success |
| `6` | Database busy: another Foxy window or command is using this game space |
