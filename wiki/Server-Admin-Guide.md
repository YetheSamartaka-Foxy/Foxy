# Server Admin Guide

This guide covers everything a server administrator needs to set up and maintain Foxy-compatible mod repositories for Arma 3 and Arma Reforger communities with `foxy-server-backend-cli`.

---

## Table of Contents

1. [Overview](#overview)
2. [Prerequisites](#prerequisites)
3. [Setting Up a Repository](#setting-up-a-repository)
4. [Server Launch Line and Keys](#server-launch-line-and-keys)
5. [Arma Reforger Repositories](#arma-reforger-repositories)
6. [Repository Structure](#repository-structure)
7. [Repository Spaces](#repository-spaces)
8. [Checks, Previews, and Safe Publishing](#checks-previews-and-safe-publishing)
9. [App Updates Distribution](#app-updates-distribution)
10. [Configuration Reference](#configuration-reference)
11. [Hosting Behind a Reverse Proxy](#hosting-behind-a-reverse-proxy)
12. [Maintaining Repositories](#maintaining-repositories)
13. [Troubleshooting](#troubleshooting)

---

## Overview

As a server administrator, your role is to:

- Organize your mod folders on disk.
- Use `foxy-server-backend-cli` to generate a repository with checksums and manifests.
- Serve the output directory over HTTP/HTTPS so Foxy clients can sync mods.
- Optionally group several repositories into a **repository space** for your community, generated in one pass with `create-space`.
- Optionally keep your server launch scripts and key folder in step with every generated repository.
- Optionally host a **self-hosted app updater** so your community always runs the latest Foxy version.

Foxy clients connect to your repository URL, read the `repo.json` manifest, and download or update only the files (and file parts) that changed.

### Command summary

| Command | Purpose |
|---------|---------|
| `new [config.json] [--game arma3\|reforger]` | Write a blank repository config |
| `create <config> <output>` | Generate one repository |
| `new-space [space.json]` | Write a blank repository space config |
| `create-space <space.json> <output>` | Generate every repository of a space plus `repository_space.json` |
| `validate <config> [--space] [--output <dir>]` | Check a config without hashing or writing |
| `verify <output>` | Rehash generated output against its manifests |
| `diff <old> <new>` | Compare two generated outputs |
| `audit-keys <config> [--space] [--strict]` | Check Arma 3 keys and PBO signatures |
| `export-reforger-config <config> <output>` | Write an Arma Reforger `game.mods` fragment |
| `setup-app-updater` / `new-app-update` | Create or extend a Foxy app update manifest |

Global flags go before the subcommand: `--json` prints one machine-readable result on stdout (progress and messages go to stderr), and `--no-progress` turns off the animated progress bar. `--version` prints the version and the source commit, so a build from source can be told apart from a published build of the same version.

### Compatibility with Swifty

Foxy is fully backwards compatible with Swifty repositories. The generator supports three modes so you can migrate at your own pace:

| Mode | Flag | Hashing | Output artifacts |
|------|------|---------|------------------|
| **FoxyMode** (default) | `--mode foxy` | BLAKE3 | `foxy_addon.json` per mod, `foxy_addons.json`, `repo.json` |
| **SwiftyMode** | `--mode swifty` | MD5 | `mod.srf` per mod, `repo.json` |
| **HybridMode** | `--mode hybrid` | BLAKE3 + MD5 | All of the above side by side |

HybridMode serves both Foxy and legacy Swifty clients from the same repository. Once your community has migrated, switch to FoxyMode and drop the legacy artifacts.

---

## Prerequisites

1. **A web server** that serves static files over HTTP or HTTPS (nginx, Apache, Caddy, IIS, or any static file host). It must support HTTP range requests, and it must follow symlinks if you use the `pool` or `link` space layouts.

2. **Mod folders** organized in directories, each mod in its own folder (for example `@cba_a3/`, `@ace/`). The structure inside each mod should match what the game expects (for Arma 3 typically `addons/`, `keys/`, optionally `optionals/`).

3. **The `foxy-server-backend-cli` binary.** Build it from the repository root:
   ```
   cargo build --release -p foxy-server-backend-cli
   ```
   The binary lands in the workspace `target/release/` folder as `foxy-server-backend-cli` (Linux) or `foxy-server-backend-cli.exe` (Windows).

---

## Setting Up a Repository

### Step 1: Generate a config template

```
foxy-server-backend-cli new config.json
```

This writes a blank Arma 3 `config.json`. Use `new config.json --game reforger` for an Arma Reforger template. If the file already exists, the command refuses to overwrite it.

The Arma 3 template looks like this:

```json
{
  "repoName": "My Repository",
  "game": "arma3",
  "basePath": ".",
  "appUpdateUrl": "",
  "requiredMods": [
    { "modName": "@example_mod", "enabled": true }
  ],
  "optionalMods": [
    { "modName": "@client_side_sound", "enabled": false, "clientSide": true }
  ],
  "iconImagePath": "icon.png",
  "repoImagePath": "repo.png",
  "clientParameters": "",
  "modLineFiles": [],
  "dlcContent": {
    "csla": false,
    "ef": false,
    "gm": false,
    "rf": false,
    "spe": false,
    "vn": false,
    "ws": false
  },
  "repoBasicAuthentication": {
    "username": "",
    "password": ""
  },
  "version": "3.2.0.0",
  "servers": [
    {
      "name": "Main Server",
      "address": "127.0.0.1",
      "port": "2302",
      "password": "",
      "battleEye": true
    }
  ]
}
```

### Step 2: Edit the config

```json
{
  "repoName": "My Community Mods",
  "basePath": "D:/Arma3/ServerMods",
  "appUpdateUrl": "https://mods.example.com/foxy/",
  "requiredMods": [
    { "modName": "@cba_a3" },
    { "modName": "@ace" },
    { "modName": "@tfar" }
  ],
  "optionalMods": [
    { "modName": "@shacktac_ui", "enabled": false, "clientSide": true }
  ],
  "iconImagePath": "icon.png",
  "repoImagePath": "repo.png",
  "clientParameters": "-skipIntro -noSplash -world=empty",
  "dlcContent": ["gm", "spe"],
  "modLineFiles": ["../server/start-server.cmd"],
  "version": "1.0.0",
  "servers": [
    {
      "name": "Main Server",
      "address": "arma3.example.com",
      "port": "2302",
      "password": "",
      "battleEye": true
    },
    {
      "name": "Training Server",
      "address": "arma3-training.example.com",
      "port": "2402",
      "password": "",
      "battleEye": false
    }
  ]
}
```

Key points:

- **`basePath`** is the root folder containing your mod folders. Mod names in `requiredMods`/`optionalMods` resolve relative to it. On Windows, use forward slashes or escaped backslashes in JSON.
- **`enabled`** defaults to `true`. Set it to `false` for mods that clients see but leave unchecked by default.
- **`clientSide`** marks a mod that only players run (Arma 3). It is never added to the server launch line, and the Foxy client uses it to explain what it enables when joining.
- **Wildcards** are supported in the last path segment: `"@*"` matches every folder starting with `@` inside `basePath`, and `"collections/*"` matches subfolders.
- **Nested mods** such as `@ace/optionals/@ace_noactionmenu` are published as a standalone `@ace_noactionmenu` mod.
- **Mod names are lowercased** in the output. A source folder named `@ACE3` becomes `@ace3`.
- **Images** (`iconImagePath`, `repoImagePath`) resolve relative to `basePath`. If found, they are copied to the output and their SHA-1 checksums are written into `repo.json`.
- **`dlcContent`** lists the Arma 3 Creator DLCs the repository uses (see [DLC content](#dlc-content)).
- **`modLineFiles`** lists server launch scripts to keep in step with the generated launch line (see [Server Launch Line and Keys](#server-launch-line-and-keys)).

### Step 3: Build the repository

```
foxy-server-backend-cli create config.json ./output
```

This reads the config, discovers the files in each mod folder, copies them to the output, computes checksums, writes the manifest files, and prints the server launch line last.

Useful options:

| Option | Effect |
|--------|--------|
| `--mode foxy\|swifty\|hybrid` | Generation mode (default `foxy`) |
| `--threads <n>` | Worker threads (default 1 for deterministic ordering; raise it for large repositories) |
| `--app-update-url <url>` | Set or override `appUpdateUrl` in `repo.json`; wins over the config value |
| `--mod-line-prefix <dir>` | Server-side folder prefix for each mod in the launch line (for example `mods`) |
| `--mod-line-include-optional` | Add the optional mods to the launch line |
| `--prune-unused-optionals` | Leave root-level `optionals` folders out of the published copies |
| `--collect-keys`, `--keys-output <dir>`, `--additional-keys <path>` | Collect `.bikey` files into one folder |
| `--dry-run` | List every planned write without writing |
| `--incremental` | Reuse checksums of unchanged mods |
| `--atomic --yes` | Build beside the output and swap it in only after success |
| `--report` | Print added, changed, and removed mods with estimated download bytes |

For scripts or screen-reader-friendly output, turn off the progress bar with the global flag:

```
foxy-server-backend-cli --no-progress create config.json ./output
```

### Step 4: Serve the output

Point your web server at the output directory. Foxy clients access `https://your-server.example.com/repo/repo.json` (or whatever your URL path is). Players add the folder URL, for example `https://your-server.example.com/repo/`.

---

## Server Launch Line and Keys

### The printed launch line

`create` finishes by printing the server launch line for the generated repository and saves the same single line to `<output>/server_mod_line.txt`, so a wrapper script can read it later:

```
foxy-server-backend-cli create config.json ./output --mod-line-prefix mods
# Server mod line:
# -mod=gm;spe;mods/@cba_a3;mods/@ace;mods/@tfar;
```

For Arma 3, the Creator DLC codes from `dlcContent` come first, then the enabled required mods. Disabled and client-side mods are never added; `--mod-line-include-optional` appends the enabled optional mods. Put a nested mod path in `requiredMods` to include it in the default line.

### Keeping launch scripts in step (`modLineFiles`)

List the launch scripts or server configs that should always carry the current mod line:

```json
"modLineFiles": ["../server/start-server.cmd", "/opt/arma3/start.sh"]
```

After a successful generation, each listed file keeps its content except for the value of the launch parameters Foxy produces: `-mod=` for Arma 3, `-addonsDir` and `-addons` for Arma Reforger. Quoting is preserved, so `start.exe "-mod=@old;" -config=server.cfg` becomes `start.exe "-mod=mods/@cba_a3;" -config=server.cfg` and nothing else on the line moves. Every occurrence in a file is updated; `--mod=`, `-modules=`, and `-addons` inside `-addonsDir` are left alone. Relative paths resolve from the config file's own folder.

A listed file that is missing, is not UTF-8 text, or has no such parameter fails the run before any hashing starts, so a typo never leaves a server silently running the old mod set. `validate` performs the same check, `--dry-run` lists the files as `update-mod-line` actions, and the rewrite runs only after the repository output is published. In a space, two repositories may not list the same file.

### Pruning unused optionals

`--prune-unused-optionals` leaves the root `optionals` folder out of published mods (the source is untouched). When the output still holds an `optionals` folder from an earlier run, the command lists it and asks for `--yes` before removing it; a fresh or already-pruned output needs no `--yes`.

### Collecting keys

`--collect-keys` copies every `.bikey` from the generated mods into one flat folder (`<output>/keys` by default) so you can push it to the server in one step:

```
foxy-server-backend-cli create config.json ./output --collect-keys --keys-output ./server/keys --additional-keys ./a3-keys
```

`--keys-output` picks the destination, and `--additional-keys` (repeatable) adds a key file or folder that the generator does not produce, such as `a3.bikey` and the Creator DLC keys. Both imply `--collect-keys`. Keys are flattened by file name: identical duplicates are skipped, and a name clash between two different keys keeps the first one and is reported. Collection never deletes anything already in the destination.

---

## Arma Reforger Repositories

Set `"game": "reforger"` in the config (`new config.json --game reforger` writes a template):

```json
{
  "repoName": "Example Reforger Repository",
  "game": "reforger",
  "basePath": ".",
  "requiredMods": [{ "modName": "MyReforgerMod", "enabled": true }],
  "optionalMods": [{ "modName": "OptionalReforgerMod", "enabled": false }],
  "clientParameters": "-noSplash",
  "modLineFiles": ["server/start-reforger.sh"],
  "servers": [{ "name": "Main Server", "address": "203.0.113.10", "port": "2001" }]
}
```

- Unpacked addon folders are hashed the same way as Arma 3 mods. `.pak` archives inside them are parsed so clients can update individual entries.
- The printed launch line is `-addonsDir <prefix or .> -addons <id,...>`. Each id is the mod's `.gproj` GUID, with the project ID, the Workshop `ServerData.json` id, and finally the folder name as fallbacks.
- `dlcContent` and `clientSide` mean nothing for Reforger; they are accepted with a warning and ignored.
- The generated `repo.json` carries `"game": "reforger"`. Arma 3 output does not include the key.

To configure a dedicated server, export the mod list as a `game.mods` fragment:

```
foxy-server-backend-cli export-reforger-config config.json reforger_mods.json
foxy-server-backend-cli export-reforger-config config.json reforger_mods.json --include-optional
```

```json
{
  "game": {
    "mods": [
      { "modId": "ABCDEF1234567890", "name": "MyReforgerMod" }
    ]
  }
}
```

Every exported mod needs a resolvable mod id.

---

## Repository Structure

After `create`, the output directory has the following layout, depending on the generation mode.

### FoxyMode (default)

```
output/
  repo.json                     # Repository manifest (mod lists empty, data in foxy_addons.json)
  foxy_addons.json              # Repo-level mod listing with BLAKE3 checksums
  server_mod_line.txt           # The printed server launch line
  icon.png                      # Repository icon (if configured)
  repo.png                      # Repository banner image (if configured)
  keys/                         # Combined keys (only with --collect-keys)
  @cba_a3/
    foxy_addon.json             # Per-mod manifest (BLAKE3 checksums, file parts)
    addons/
      cba_main.pbo
      ...
    keys/
      cba.bikey
  ...
```

In FoxyMode, `repo.json` holds the metadata (name, servers, client parameters, and so on) but its `requiredMods` and `optionalMods` arrays are **empty**. The mod listings with BLAKE3 checksums live in `foxy_addons.json`, and each mod folder has a `foxy_addon.json` with per-file and per-part checksums.

### SwiftyMode

```
output/
  repo.json                     # Repository manifest with MD5 checksums in mod lists
  icon.png
  repo.png
  @cba_a3/
    mod.srf                     # Per-mod manifest (MD5 checksums, Swifty-compatible)
    addons/
      ...
  ...
```

### HybridMode

```
output/
  repo.json                     # Has foxyMode field + MD5 mod lists for Swifty clients
  foxy_addons.json              # BLAKE3 mod listing for Foxy clients
  @cba_a3/
    mod.srf                     # Swifty manifest (MD5)
    foxy_addon.json             # Foxy manifest (BLAKE3)
    addons/
      ...
  ...
```

HybridMode writes both artifact sets. `repo.json` includes the `foxyMode` marker (so Foxy clients read `foxy_addons.json`) and the MD5 mod lists (so Swifty clients sync normally).

### File parts

Files are split into **parts** for fine-grained checksums and delta updates:

- **PBO files** (Arma 3) are parsed into a `$$HEADER$$` part, one part per archived entry, and a `$$END$$` tail part, so clients patch at the entry level.
- **PAK files** (Arma Reforger, PAC1 format) are parsed into a `$$HEADER$$` part, one part per entry, `$$GAP:n$$` parts for bytes between entries, and a `$$END$$` part.
- **Other files** are split into 5 MB chunks. A 12 MB file becomes three parts: `filename_5000000`, `filename_10000000`, `filename_12000000`.
- **`.srf` files** in source mod folders are skipped (they are regenerated), and so is a `foxy_addon.json` at a mod's root, so an earlier output can be reused as a source.

### The `foxyMode` field

FoxyMode and HybridMode write `"foxyMode": "FoxyModeV1"` into `repo.json`. That tells Foxy clients to fetch `foxy_addons.json` and the per-mod `foxy_addon.json` files. Without it (SwiftyMode), clients use the MD5 lists in `repo.json` and the `mod.srf` files.

---

## Repository Spaces

A **repository space** groups several repositories under one URL, so players can add your whole community setup in one step. The Foxy client downloads every repository of a space into one shared folder.

### Generating a space with `create-space`

`create-space` generates every repository of a space plus the `repository_space.json` that links them. The space config only points at the per-repository configs that `create` already uses:

```
foxy-server-backend-cli new-space space.json
foxy-server-backend-cli create-space space.json ./www --layout pool
```

```json
{
  "name": "My Community",
  "baseUrl": "https://mods.example.com/repos/",
  "appUpdateUrl": "https://mods.example.com/foxy/",
  "iconImagePath": "icon.png",
  "repoImagePath": "space.png",
  "repositories": [
    { "config": "modern/config.json", "folder": "modern", "required": true },
    { "config": "vietnam/config.json", "folder": "vietnam", "name": "Vietnam", "required": true },
    { "config": "ww2/config.json", "folder": "ww2", "required": false, "address": "https://cdn.example.com/ww2/" }
  ]
}
```

| Field | Description |
|-------|-------------|
| `name` | Display name of the space |
| `baseUrl` | Public URL the repository folders are served under |
| `appUpdateUrl` | Optional Foxy app update source, written into `repository_space.json` |
| `iconImagePath`, `repoImagePath` | Space icon and banner, relative to the space config |
| `repositories[].config` | Repository config file, relative to the space config |
| `repositories[].folder` | Output subfolder (defaults to the config file name) |
| `repositories[].name` | Display name (defaults to the repository's `repoName`) |
| `repositories[].address` | Public URL (defaults to `<baseUrl>/<folder>/`) |
| `repositories[].required` | Whether players must take this repository (default `true`) |

Each repository lands in `<output>/<folder>/`. Each repository's `basePath` keeps the `create` meaning and resolves from the working directory.

### Layouts

`--layout` decides how mod folders are stored:

| Layout | What happens | When to use |
|--------|--------------|-------------|
| `copy` (default) | Every repository gets a full copy of every mod, exactly like running `create` per repository. | No symlink support on the server or web host. |
| `pool` (recommended) | Each distinct mod is copied once into `<output>/pool` (`--pool-dir` to move it), and every repository holds a relative symlink to it. Shared mods use disk space once and the whole tree can be moved. | Everywhere symlinks work: Linux hosts, or Windows with Developer Mode or an elevated shell. |
| `link` | Every repository symlinks straight to the source mod folder; nothing is copied. Requires `--yes`: manifests are written into the source folders, and any later change there breaks the published checksums until you run `create-space` again. | Only when the sources are already the served copy and never edited in place. |

The web server must follow symlinks for `pool` and `link` (nginx does by default; Apache needs `Options FollowSymLinks`).

A mod name that appears in several repositories must hash identically in all of them, because clients download a space into one shared folder. `create-space` refuses otherwise. Existing real folders are never replaced by symlinks.

### Space options

- `--mode`, `--threads`, `--app-update-url`, the launch line options, `--dry-run`, `--incremental`, `--atomic --yes`, and `--report` work as for `create`, applied to every repository. A launch line and `server_mod_line.txt` are written per repository, and each repository's `modLineFiles` are rewritten with its own line.
- `--collect-keys` gathers the keys of the whole space into `<output>/keys`. `--per-repo-keys` also writes each repository's keys (plus `--additional-keys`) into `<output>/<folder>/keys`, so a server can symlink one repository's keys folder directly.
- `--only <folder>` (repeatable) regenerates only the selected repositories and keeps the full space manifest. It refuses mods shared with repositories you did not select, and it cannot rebuild the combined keys folder.
- `--prune-unused-optionals` works with `copy` and `pool`, not with `link`.
- For pool output, `--clean --dry-run` lists orphaned generated pool folders and their symlinks, and `--clean --yes` removes them after a successful generation. Cleanup never touches unrelated folders, and a pool outside the output needs an inventory from an earlier `create-space` run.

### The generated `repository_space.json`

```json
{
  "name": "My Community",
  "image": "space.png",
  "imageChecksum": "<sha1>",
  "icon": "icon.png",
  "iconChecksum": "<sha1>",
  "appUpdateUrl": "https://mods.example.com/foxy/",
  "entries": [
    { "Name": "Modern", "Address": "https://mods.example.com/repos/modern/", "Requiered": true },
    { "Name": "Vietnam", "Address": "https://mods.example.com/repos/vietnam/", "Requiered": true },
    { "Name": "WW2", "Address": "https://cdn.example.com/ww2/", "Requiered": false }
  ]
}
```

| Field | Description |
|-------|-------------|
| `name` | Display name for the space in the Foxy client |
| `image`, `imageChecksum` | Banner image path and checksum |
| `icon`, `iconChecksum` | Icon image path and checksum |
| `appUpdateUrl` | URL of a `foxy-app-updater.json` manifest (fills the client's update source) |
| `entries[].Name` | Display name for the repository |
| `entries[].Address` | Full URL of the repository root (where `repo.json` lives) |
| `entries[].Requiered` | `true` for required repositories, `false` for optional ones (the legacy spelling is intentional) |

You can still write this file by hand if you generate repositories separately; keep the same shape.

### How spaces work with Foxy

When a player adds the space URL, the client reads `repository_space.json` and adds the required repositories automatically; optional ones can be picked by the player. The space's `appUpdateUrl` has the **highest priority** for filling the client's app update source, ahead of any `repo.json` value.

Serve the space folder with its images:

```
https://mods.example.com/repos/
  repository_space.json
  space.png
  icon.png
  modern/  vietnam/  pool/
```

Players then add `https://mods.example.com/repos/` as a repository space in Foxy.

---

## Checks, Previews, and Safe Publishing

```
foxy-server-backend-cli validate config.json --output ./www/repo
foxy-server-backend-cli validate space.json --space --output ./www
foxy-server-backend-cli audit-keys config.json --strict
foxy-server-backend-cli create-space space.json ./www --layout pool --dry-run --clean
foxy-server-backend-cli create-space space.json ./www --layout pool --incremental --report
foxy-server-backend-cli verify ./www
foxy-server-backend-cli diff ./old-www ./www --json
```

- **`validate`** checks the config, mod references, published names, and `modLineFiles` without hashing or writing. With `--output`, it also checks output path overlap and layout collisions.
- **`--dry-run`** on `create` and `create-space` lists planned copies, manifest writes, links, pruning, key collection, launch-script updates, and pool cleanup without writing.
- **`--incremental`** reuses a mod's previous checksums when its source files have the same paths, sizes, and modification times and the published files still match. It keeps a `.foxy-hash-cache.json` in the output.
- **`verify`** rehashes generated output against its manifests. Use it when you want a full content check, for example after `--incremental` runs.
- **`--report`** and **`diff`** compare mod checksums before and after and estimate client download bytes from the sizes of added and changed mods.
- **`--atomic --yes`** builds in a sibling folder and replaces the whole output only after success. If publishing fails, it tries to restore the old output. It cannot be combined with `--incremental` or a custom pool or keys destination.
- **`audit-keys`** checks that each PBO has a nearby `.bisign`, that the matching `.bikey` is available (including `--additional-keys`), and that no two different keys share a name. `--strict` fails on findings. It does not verify signatures cryptographically.
- **`--json`** gives one result object on stdout for every command, with checksums or launch lines on success and a message on error.

---

## App Updates Distribution

Foxy supports **self-hosted app updates**, so each community can distribute Foxy releases itself through a `foxy-app-updater.json` manifest.

### Initial setup

```
foxy-server-backend-cli setup-app-updater \
  --version 1.1.0 \
  --windows-installer ./installers/Foxy-1.1.0-setup.exe \
  --linux-installer ./installers/Foxy-1.1.0-linux-x86_64-installer.sh \
  --linux-aarch64-installer ./installers/Foxy-1.1.0-linux-aarch64-installer.sh \
  --changelog ./CHANGELOG.md \
  --output ./update-server
```

This produces:

```
update-server/
  foxy-app-updater.json         # Update manifest (BLAKE3 hashes, schema version 1)
  changelogs/
    1.1.0.json                  # Structured changelog extracted from CHANGELOG.md
```

Requirements:

- A Windows installer is required for every version; the Linux x86_64 and Linux ARM64 installers are optional.
- `--changelog` points to a `CHANGELOG.md` file. Headings like `# 1.1.0` and `# [1.1.0] - 2026-03-28` are supported, and the version must exist in the file.

### Adding new releases

```
foxy-server-backend-cli new-app-update \
  --version 1.2.0 \
  --windows-installer ./installers/Foxy-1.2.0-setup.exe \
  --linux-installer ./installers/Foxy-1.2.0-linux-x86_64-installer.sh \
  --linux-aarch64-installer ./installers/Foxy-1.2.0-linux-aarch64-installer.sh \
  --changelog ./CHANGELOG.md \
  --output ./update-server
```

This keeps every earlier version in the manifest (so players can downgrade from the Version Browser), prepends the new one, and updates `latest`. A version that already exists cannot be added again; remove it from `foxy-app-updater.json` first if you need to republish.

### Server directory layout

```
update-server/
  foxy-app-updater.json
  installers/
    Foxy-1.2.0-setup.exe
    Foxy-1.2.0-linux-x86_64-installer.sh
    Foxy-1.2.0-linux-aarch64-installer.sh
    Foxy-1.1.0-setup.exe
    ...
  changelogs/
    1.2.0.json
    1.1.0.json
```

The manifest references installers by relative path (for example `installers/Foxy-1.2.0-setup.exe`), so place them in an `installers/` folder next to it.

### Connecting updates to repositories

To let Foxy clients find your update source automatically, set `appUpdateUrl` in either:

- **`repository_space.json`** (highest priority), through the space config or by hand.
- **`repo.json`**, through `appUpdateUrl` in `config.json`.
- **The command line** when building: `--app-update-url https://mods.example.com/updates/` (wins over the config value).

The URL points to the folder containing `foxy-app-updater.json`. Clients fill it into their settings only when their update source field is empty; a URL the player typed is treated as an override and never replaced.

### Manifest format

```json
{
  "schemaVersion": 1,
  "latest": "1.2.0",
  "versions": [
    {
      "version": "1.2.0",
      "changelog": "changelogs/1.2.0.json",
      "platforms": {
        "windows-x86_64": {
          "installerPath": "installers/Foxy-1.2.0-setup.exe",
          "installerHash": "<blake3-hex>",
          "installerSize": 12345678
        },
        "linux-x86_64": {
          "installerPath": "installers/Foxy-1.2.0-linux-x86_64-installer.sh",
          "installerHash": "<blake3-hex>",
          "installerSize": 9876543
        },
        "linux-aarch64": {
          "installerPath": "installers/Foxy-1.2.0-linux-aarch64-installer.sh",
          "installerHash": "<blake3-hex>",
          "installerSize": 9765432
        }
      }
    },
    {
      "version": "1.1.0",
      "changelog": "changelogs/1.1.0.json",
      "platforms": { ... }
    }
  ]
}
```

Platform keys are `windows-x86_64`, `linux-x86_64`, and `linux-aarch64`. The client verifies the installer's BLAKE3 hash before running it.

---

## Configuration Reference

### config.json (input to `create`)

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `repoName` | string | yes | - | Display name of the repository |
| `game` | string | no | `"arma3"` | `arma3` or `reforger`; selects the server launch line |
| `basePath` | string | yes | - | Root folder containing the mod folders |
| `appUpdateUrl` | string | no | `""` | Foxy app update source written into `repo.json` |
| `requiredMods` | array | no | `[]` | Required mod references |
| `optionalMods` | array | no | `[]` | Optional mod references |
| `iconImagePath` | string | no | `""` | Repository icon (relative to `basePath`) |
| `repoImagePath` | string | no | `""` | Repository banner (relative to `basePath`) |
| `clientParameters` | string | no | `""` | Launch parameters suggested to clients |
| `dlcContent` | object or array | no | omitted | Arma 3 Creator DLCs the repository uses |
| `modLineFiles` | array | no | `[]` | Launch scripts whose launch parameters are rewritten (relative to the config file) |
| `repoBasicAuthentication` | object | no | empty | HTTP Basic Auth credentials for protected repositories |
| `version` | string | no | `"3.2.0.0"` | Repository protocol version |
| `servers` | array | no | `[]` | Game server entries |

#### Mod reference format

```json
{ "modName": "@ace", "enabled": true, "clientSide": false }
```

- `modName` (string, required) - folder name or path relative to `basePath`. Supports glob wildcards in the last segment (`@*`, `collections/*`, `mods/@[ac]*`). Absolute paths work for mods stored outside `basePath`.
- `enabled` (boolean, default `true`) - whether the mod is checked by default in the Foxy client.
- `clientSide` (boolean, default `false`, Arma 3) - the mod runs only on players' machines and is left out of the server launch line.

#### Server entry format

```json
{
  "name": "Main Server",
  "address": "arma3.example.com",
  "port": "2302",
  "password": "",
  "battleEye": true
}
```

- `name` (string, required) - display name.
- `address` (string, required) - server IP or hostname.
- `port` (string, required) - game port.
- `password` (string, default `""`) - server password.
- `battleEye` (boolean, default `false`) - whether BattlEye is enabled.

#### Basic authentication

```json
"repoBasicAuthentication": {
  "username": "myuser",
  "password": "mypassword"
}
```

When set, Foxy clients send HTTP Basic Auth headers with every request to this repository. Leave both fields empty to disable it, and make sure the web server enforces the same credentials.

### repo.json (generated output)

| Field | Description |
|-------|-------------|
| `repoName` | Repository display name |
| `game` | Present only for non-Arma 3 repositories, for example `"reforger"` |
| `checksum` | Repository-level checksum (BLAKE3 in FoxyMode, SHA-1 in SwiftyMode) |
| `foxyMode` | `"FoxyModeV1"` in FoxyMode and HybridMode |
| `requiredMods` | Array of `{ modName, checkSum, enabled, clientSide? }` (empty in FoxyMode, MD5 in Swifty/Hybrid) |
| `optionalMods` | Same shape as `requiredMods` |
| `iconImagePath`, `iconImageChecksum` | Icon image path and SHA-1 checksum |
| `repoImagePath`, `repoImageChecksum` | Banner image path and SHA-1 checksum |
| `appUpdateUrl` | Foxy app update source (omitted if empty) |
| `clientParameters` | Suggested launch parameters |
| `repoBasicAuthentication` | `{ username, password }` for HTTP Basic Auth |
| `version` | Repository protocol version |
| `servers` | Game server entries |
| `dlcContent` | Full Creator DLC object (omitted when not configured) |

`clientSide` appears on a mod entry only when it is `true`.

### foxy_addons.json (FoxyMode/HybridMode)

| Field | Description |
|-------|-------------|
| `version` | `"FoxyModeV1"` |
| `hashAlgorithm` | `"BLAKE3"` |
| `checksum` | Repository-level BLAKE3 checksum |
| `requiredMods` | Array of `{ modName, checkSum, enabled, clientSide? }` with BLAKE3 checksums |
| `optionalMods` | Same shape as `requiredMods` |

### foxy_addon.json (per mod, FoxyMode/HybridMode)

| Field | Description |
|-------|-------------|
| `name` | Mod folder name (lowercased) |
| `version` | `"FoxyModeV1"` |
| `checksum` | Mod-level BLAKE3 checksum |
| `hashAlgorithm` | `"BLAKE3"` |
| `files` | Array of file entries |

Each file entry:

| Field | Description |
|-------|-------------|
| `path` | Relative file path (forward slashes) |
| `checksum` | File-level BLAKE3 checksum |
| `length` | File size in bytes |
| `fileType` | `"FoxyPboFile"` for `.pbo` files, `"FoxyFile"` for all others |
| `parts` | Array of `{ path, checksum, start, length }` parts |

### DLC content

`dlcContent` tells players which Arma 3 Creator DLCs the repository uses:

```json
"dlcContent": {
  "csla": false,
  "ef": false,
  "gm": true,
  "rf": false,
  "spe": true,
  "vn": false,
  "ws": false
}
```

The config accepts that object form or a shorthand list of the enabled codes:

```json
"dlcContent": ["gm", "spe"]
```

Both produce the full object in `repo.json`. Valid codes are `csla` (CSLA Iron Curtain), `ef` (Expeditionary Forces), `gm` (Global Mobilization), `rf` (Reaction Forces), `spe` (Spearhead 1944), `vn` (S.O.G. Prairie Fire), and `ws` (Western Sahara). An unknown code fails config parsing instead of being dropped. Omit `dlcContent` to leave it out of `repo.json`.

The enabled codes also lead the printed `-mod=` server line. Players apply the selection through **Auto apply repo.json DLC content** in Foxy, and when they join a server Foxy offers to match the Creator DLCs that server actually runs.

---

## Hosting Behind a Reverse Proxy

### nginx

```nginx
server {
    listen 443 ssl;
    server_name mods.example.com;

    ssl_certificate     /etc/ssl/certs/mods.example.com.pem;
    ssl_certificate_key /etc/ssl/private/mods.example.com.key;

    # Repository root
    location /repo/ {
        alias /var/www/foxy-repo/;
        autoindex off;

        # Allow large mod file downloads
        client_max_body_size 0;

        # Cache manifests briefly so updates propagate quickly
        location ~* \.(json)$ {
            expires 5m;
            add_header Cache-Control "public, max-age=300";
        }

        # Cache mod files longer (they are checksum-verified)
        location ~* \.(pbo|pak|bikey|bisign|cpp|bin|png|jpg)$ {
            expires 7d;
            add_header Cache-Control "public, max-age=604800";
        }
    }

    # App update server
    location /foxy/ {
        alias /var/www/foxy-updates/;
        autoindex off;
    }
}
```

nginx follows symlinks by default, so `pool` and `link` space layouts work as is.

### Apache

```apache
<VirtualHost *:443>
    ServerName mods.example.com

    SSLEngine on
    SSLCertificateFile    /etc/ssl/certs/mods.example.com.pem
    SSLCertificateKeyFile /etc/ssl/private/mods.example.com.key

    DocumentRoot /var/www/foxy-repo

    <Directory /var/www/foxy-repo>
        Options -Indexes +FollowSymLinks
        AllowOverride None
        Require all granted
    </Directory>

    # Cache control for JSON manifests
    <FilesMatch "\.(json)$">
        Header set Cache-Control "public, max-age=300"
    </FilesMatch>

    # Cache control for mod files
    <FilesMatch "\.(pbo|pak|bikey|bisign)$">
        Header set Cache-Control "public, max-age=604800"
    </FilesMatch>

    Alias /foxy/ /var/www/foxy-updates/
</VirtualHost>
```

### General tips

- **HTTPS is recommended.** Foxy supports HTTP, but HTTPS protects file integrity in transit.
- **Disable directory listings.** The manifests contain everything clients need.
- **Set cache headers.** JSON manifests (`repo.json`, `foxy_addons.json`, `foxy_addon.json`, `repository_space.json`) should have short TTLs (5-15 minutes) so updates propagate quickly. Mod files can be cached longer because clients verify them by checksum.
- **HTTP range requests** must work, because Foxy downloads large files in parallel ranges and patches individual parts.
- **Compression.** Enable gzip or brotli for `.json` files. Mod archives are already compressed and do not benefit from transport compression.
- **Basic authentication.** If you set `repoBasicAuthentication`, enforce the same credentials in the web server.
- **Bandwidth.** Large modsets can be tens of gigabytes. Delta patching reduces update sizes significantly, but plan hosting for full first downloads.

---

## Maintaining Repositories

### Updating after mod changes

When mods change, run the same command again:

```
foxy-server-backend-cli create config.json ./output
```

This re-discovers the files, recomputes checksums, regenerates the manifests, and rewrites any `modLineFiles`. Add `--incremental` to skip rehashing mods whose files did not change, `--report` to see what changed, and `--atomic --yes` so players never see a half-written output.

Foxy clients detect changes through the repository checksum in `repo.json` and download only the files and file parts that differ.

### Adding or removing mods

1. Edit `requiredMods` / `optionalMods` in `config.json`.
2. Run `validate` to catch typos.
3. Run `create` (or `create-space`) again. The generated manifests and the launch line reflect the new list.

### Switching generation modes

You can switch modes at any time with `--mode`. Foxy clients detect the `foxyMode` field and use the new manifests; legacy Swifty clients stop working unless you use HybridMode.

A recommended migration path:

1. Start with `--mode hybrid` to serve both clients.
2. Wait for your community to switch to Foxy.
3. Switch to `--mode foxy` to drop the legacy artifacts and benefit from faster BLAKE3 hashing.

### Automation

```bash
#!/bin/bash
# rebuild-repo.sh - run after mod updates
set -e

SPACE_CONFIG="/etc/foxy/space.json"
OUTPUT_DIR="/var/www/foxy-repo"

foxy-server-backend-cli validate "$SPACE_CONFIG" --space --output "$OUTPUT_DIR"
foxy-server-backend-cli --no-progress --json create-space "$SPACE_CONFIG" "$OUTPUT_DIR" \
  --layout pool --incremental --collect-keys --threads 8 > /var/log/foxy-rebuild.json

echo "Repositories updated at $(date)"
```

Use `--threads` matching your CPU cores for large repositories; BLAKE3 (FoxyMode) benefits most. With `modLineFiles` set, your server start scripts already carry the new mod line when the command returns.

---

## Troubleshooting

### "basePath does not exist or is not a directory"

`basePath` must point to an existing folder. On Windows, use forward slashes or escaped backslashes in JSON:

```json
"basePath": "D:/Arma3/ServerMods"
```

### "Mod directory does not exist"

A mod name in `requiredMods` or `optionalMods` resolves to a folder that does not exist under `basePath`. Check the spelling and the folder.

### "Wildcard pattern matched no directories"

A wildcard such as `@*` found no matching folders. This is a warning; the build continues with the mods it found.

### "No mods found after expanding all mod references"

After expanding every reference, no mod folders were found. Check `basePath` and your mod references.

### A `modLineFiles` entry fails the run

The listed file is missing, is not UTF-8 text, or has no `-mod=` (Arma 3) or `-addonsDir`/`-addons` (Reforger) parameter to replace. Fix the path (relative paths resolve from the config file's folder) or add the parameter to the script. `validate` reports the same error without generating anything.

### `create-space` refuses a shared mod

The same mod name produced different checksums in two repositories, usually because the repositories point at different source folders or one source changed mid-build. Point both configs at the same source, or rename one of the mods.

### Symlink errors with `pool` or `link`

Creating symlinks on Windows needs Developer Mode or an elevated shell. Use `--layout copy` where symlinks are not available.

### "--yes" is required

Some actions remove published data or write into your sources: `--layout link`, `--clean`, `--atomic`, and `--prune-unused-optionals` when earlier optionals are published. Run with `--dry-run` first to see what would change, then add `--yes`.

### Clients see no update after rebuild

1. Check that `checksum` in `repo.json` changed. Identical mod files produce the same checksum.
2. Check web server caching. A long cache on JSON files serves a stale `repo.json`.
3. Make sure the client's repository URL points to the folder that contains `repo.json`.

### Clients cannot connect

1. Open `https://mods.example.com/repo/repo.json` in a browser; it should return valid JSON.
2. If you use basic authentication, check that the credentials match between `config.json` and the web server.
3. Check that the web server supports HTTP range requests.
4. For `pool` or `link` layouts, check that the web server follows symlinks.

### Archive parse warnings

"Content format parse failed for ..., treating as single file" means a `.pbo` or `.pak` archive is malformed or uses an unsupported layout. The file is hashed as plain chunks, which works but disables per-entry patching for that file.

### Large repository build times

- Increase threads: `--threads 8` or more, matching your CPU cores.
- Use FoxyMode (BLAKE3).
- Use `--incremental` for routine rebuilds and `verify` occasionally for a full check.
- Keep source and output on fast storage (SSD preferred).
- For very large modsets, split into several repositories in one space with `--layout pool`.

### Version already exists in app updater manifest

`new-app-update` refuses a version that is already in the manifest. To republish one:

1. Open `foxy-app-updater.json`.
2. Remove the version from the `versions` array.
3. Update `latest` if needed.
4. Run `new-app-update` again.
