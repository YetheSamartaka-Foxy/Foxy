# Server Admin Guide

This guide covers everything a server administrator needs to set up and maintain Foxy-compatible mod repositories for Arma 3 communities.

---

## Table of Contents

1. [Overview](#overview)
2. [Prerequisites](#prerequisites)
3. [Setting Up a Repository](#setting-up-a-repository)
4. [Repository Structure](#repository-structure)
5. [Repository Spaces](#repository-spaces)
6. [App Updates Distribution](#app-updates-distribution)
7. [Configuration Reference](#configuration-reference)
8. [Hosting Behind a Reverse Proxy](#hosting-behind-a-reverse-proxy)
9. [Maintaining Repositories](#maintaining-repositories)
10. [Troubleshooting](#troubleshooting)

---

## Overview

As a server administrator, your role is to:

- Organize your Arma 3 mod files on disk
- Use `foxy-server-backend-cli` to generate a repository structure with checksums and manifests
- Serve the output directory over HTTP/HTTPS so Foxy clients can sync mods
- Optionally group multiple repositories into a **repository space** for your community
- Optionally host a **self-hosted app updater** so your community always runs the latest Foxy version

Foxy clients connect to your repository URL, read the `repo.json` manifest, and download or update only the files that have changed.

### Compatibility with Swifty

Foxy is fully backwards compatible with Swifty repositories. The `foxy-server-backend-cli` supports three generation modes so you can migrate at your own pace:

| Mode | Flag | Hashing | Output artifacts |
|------|------|---------|------------------|
| **FoxyMode** (default) | `--mode foxy` | BLAKE3 | `foxy_addon.json` per mod, `foxy_addons.json`, `repo.json` |
| **SwiftyMode** | `--mode swifty` | MD5 | `mod.srf` per mod, `repo.json` |
| **HybridMode** | `--mode hybrid` | BLAKE3 + MD5 | All of the above side by side |

HybridMode lets you serve both Foxy and legacy Swifty clients from the same repository. Once your community has fully migrated, you can switch to FoxyMode and drop the legacy artifacts.

---

## Prerequisites

Before you begin, you need:

1. **A web server** capable of serving static files over HTTP or HTTPS (nginx, Apache, Caddy, IIS, or any static file host).

2. **Arma 3 mod files** organized in directories. Each mod should be in its own folder (e.g., `@CBA_A3/`, `@ACE3/`). The folder structure inside each mod should match what Arma 3 expects (typically `addons/`, `keys/`, optionally `optionals/`, etc.).

3. **The `foxy-server-backend-cli` binary.** Build it from source:
   ```
   cd foxy-server-backend-cli
   cargo build --release
   ```
   The binary will be at `target/release/foxy-server-backend-cli` (Linux) or `target\release\foxy-server-backend-cli.exe` (Windows).

---

## Setting Up a Repository

### Step 1: Generate a config template

```
foxy-server-backend-cli new config.json
```

This creates a blank `config.json` template. If the file already exists, the command will refuse to overwrite it.

The generated template looks like this:

```json
{
  "repoName": "My Repository",
  "basePath": ".",
  "appUpdateUrl": "",
  "requiredMods": [
    { "modName": "@example_mod", "enabled": true }
  ],
  "optionalMods": [],
  "iconImagePath": "icon.png",
  "repoImagePath": "repo.png",
  "clientParameters": "",
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

Update the config to match your setup:

```json
{
  "repoName": "My Community Mods",
  "basePath": "D:\\Arma3\\ServerMods",
  "appUpdateUrl": "https://mods.example.com/foxy/",
  "requiredMods": [
    { "modName": "@CBA_A3" },
    { "modName": "@ace" },
    { "modName": "@TFAR" }
  ],
  "optionalMods": [
    { "modName": "@ShackTac_UI", "enabled": false }
  ],
  "iconImagePath": "icon.png",
  "repoImagePath": "repo.png",
  "clientParameters": "-skipIntro -noSplash -world=empty",
  "repoBasicAuthentication": {
    "username": "",
    "password": ""
  },
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

- **`basePath`** is the root directory containing your mod folders. Mod names in `requiredMods`/`optionalMods` are resolved relative to this path.
- **`enabled`** defaults to `true` if omitted. Set it to `false` for mods that should appear in the client but be unchecked by default.
- **Wildcard mod references** are supported. For example, `"modName": "@*"` matches all directories starting with `@` inside `basePath`. You can also use patterns like `"modName": "collections/*"` to match subdirectories. Wildcards apply only to the final path segment.
- **Mod names are lowercased** in the output. A source directory named `@ACE3` becomes `@ace3` in the generated repository.
- **Image files** (`iconImagePath`, `repoImagePath`) are resolved relative to `basePath`. If found, they are copied to the output and their SHA-1 checksums are written into `repo.json`.

### Step 3: Build the repository

```
foxy-server-backend-cli create config.json ./output
```

This reads the config, discovers all files in each mod directory, copies them to the output directory, computes checksums, and writes the manifest files.

#### Generation mode

The default mode is FoxyMode (BLAKE3). To generate for legacy Swifty clients or both:

```
foxy-server-backend-cli create config.json ./output --mode swifty
foxy-server-backend-cli create config.json ./output --mode hybrid
```

#### Thread control

By default, `foxy-server-backend-cli` uses 1 thread to ensure deterministic output ordering. For large repositories, increase parallelism:

```
foxy-server-backend-cli create config.json ./output --threads 8
```

#### App update URL override

You can set or override the `appUpdateUrl` via the command line. The CLI flag takes precedence over the config file value:

```
foxy-server-backend-cli create config.json ./output --app-update-url https://mods.example.com/foxy/
```

#### Progress display

A progress bar is shown by default. For scripted/automated usage or screen-reader-friendly output, disable it:

```
foxy-server-backend-cli --no-progress create config.json ./output
```

Note that `--no-progress` is a global flag and must appear before the subcommand.

### Step 4: Serve the output

Point your web server's document root at the output directory. Foxy clients will access `https://your-server.example.com/repo/repo.json` (or whatever your URL path is).

---

## Repository Structure

After running `foxy-server-backend-cli create`, the output directory has the following layout, depending on the generation mode:

### FoxyMode (default)

```
output/
  repo.json                     # Repository manifest (mod lists empty, data in foxy_addons.json)
  foxy_addons.json              # Repo-level mod listing with BLAKE3 checksums
  icon.png                      # Repository icon (if configured)
  repo.png                      # Repository banner image (if configured)
  @cba_a3/
    foxy_addon.json             # Per-mod manifest (BLAKE3 checksums, file parts)
    addons/
      cba_main.pbo
      ...
    keys/
      cba.bikey
  @ace/
    foxy_addon.json
    addons/
      ...
  ...
```

In FoxyMode, `repo.json` contains metadata (name, servers, client parameters, etc.) but its `requiredMods` and `optionalMods` arrays are **empty**. The actual mod listings with BLAKE3 checksums live in `foxy_addons.json` at the repo root. Each mod folder also contains a `foxy_addon.json` with per-file and per-part checksums.

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

In SwiftyMode, `repo.json` contains the full mod lists with MD5 checksums. Each mod has a `mod.srf` file for Swifty client compatibility.

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

HybridMode produces both artifact sets. The `repo.json` includes both the `foxyMode` marker (so Foxy clients know to look for `foxy_addons.json`) and the MD5 mod lists (so Swifty clients can sync normally).

### File parts

Files are broken into **parts** for granular checksum tracking:

- **PBO files** are parsed into their internal structure: a `$$HEADER$$` part, one part per PBO entry (the archived files inside the PBO), and a `$$END$$` tail part. This enables delta patching at the PBO entry level.
- **Non-PBO files** are split into 5 MB chunks. A 12 MB file becomes three parts: `filename_5000000`, `filename_10000000`, `filename_12000000`.
- **`.srf` files** in the source mod directories are automatically excluded from the output (they are regenerated).

### The `foxyMode` field

When FoxyMode or HybridMode is used, `repo.json` includes `"foxyMode": "FoxyModeV1"`. This tells Foxy clients to fetch `foxy_addons.json` and per-mod `foxy_addon.json` files instead of relying on the legacy `mod.srf` manifests. When this field is absent (SwiftyMode), clients use the MD5 mod lists in `repo.json` and `mod.srf` files.

---

## Repository Spaces

A **repository space** groups multiple repositories under a single URL, allowing players to subscribe to your entire community setup with one action.

### Creating a repository_space.json

Create a `repository_space.json` file manually and place it at a URL accessible to your players. The format is:

```json
{
  "name": "My Community",
  "image": "image.png",
  "imageChecksum": "5d41402abc4b2a76b9719d911017c592",
  "icon": "icon.png",
  "iconChecksum": "7d793037a0760186574b0282f2f435e7",
  "appUpdateUrl": "https://mods.example.com/foxy/",
  "entries": [
    {
      "Name": "Modern",
      "Address": "https://mods.example.com/repos/modern",
      "Requiered": true
    },
    {
      "Name": "Vietnam",
      "Address": "https://mods.example.com/repos/vietnam",
      "Requiered": true
    },
    {
      "Name": "WW2",
      "Address": "https://mods.example.com/repos/ww2",
      "Requiered": false
    }
  ]
}
```

### Field reference

| Field | Description |
|-------|-------------|
| `name` | Display name for the space in the Foxy client |
| `image` | Relative path to a banner image |
| `imageChecksum` | MD5 checksum of the banner image |
| `icon` | Relative path to an icon image |
| `iconChecksum` | MD5 checksum of the icon image |
| `appUpdateUrl` | URL to a `foxy-app-updater.json` manifest (auto-fills the client update source) |
| `entries` | Array of repository entries |

Each entry in `entries`:

| Field | Description |
|-------|-------------|
| `Name` | Display name for the repository |
| `Address` | Full URL to the repository root (where `repo.json` lives) |
| `Requiered` | `true` for mandatory repositories, `false` for optional ones (note: the field name preserves the legacy spelling) |

### How spaces work with Foxy

When a player adds your space URL in Foxy, the client fetches `repository_space.json` and automatically adds all listed repositories. Required repositories are always synced; optional ones can be toggled by the player.

The `appUpdateUrl` in the space has the **highest priority** for auto-detecting the app update source. If both `repository_space.json` and individual `repo.json` files specify an `appUpdateUrl`, the space value wins.

### Hosting

Place `repository_space.json` alongside its image files and serve them from a web server:

```
https://mods.example.com/space/
  repository_space.json
  image.png
  icon.png
```

Players then add `https://mods.example.com/space/` as a repository space in Foxy.

---

## App Updates Distribution

Foxy supports **self-hosted app updates**, allowing each community to distribute Foxy releases independently. This uses a `foxy-app-updater.json` manifest generated by `foxy-server-backend-cli`.

### Initial setup

Create the first update manifest:

```
foxy-server-backend-cli setup-app-updater \
  --version 1.0.0 \
  --windows-installer ./installers/Foxy-1.0.0-setup.exe \
  --linux-installer ./installers/Foxy-1.0.0-linux-installer.sh \
  --changelog ./CHANGELOG.md \
  --output ./update-server
```

This produces:

```
update-server/
  foxy-app-updater.json         # Update manifest (BLAKE3 hashes, schema version 1)
  changelogs/
    1.0.0.json                  # Structured changelog extracted from CHANGELOG.md
```

Requirements:

- At least one installer must be provided (`--windows-installer` or `--linux-installer`). A Windows installer is required for each version entry.
- The `--changelog` flag points to a standard `CHANGELOG.md` file. The parser supports headings like `# 1.0.0` and `# [1.0.0] - 2026-03-28`.
- The target version must exist in the changelog file.

### Adding new releases

When a new Foxy version is released, add it to the existing manifest:

```
foxy-server-backend-cli new-app-update \
  --version 0.8.1 \
  --windows-installer ./installers/Foxy-0.8.1-setup.exe \
  --linux-installer ./installers/Foxy-0.8.1-linux-installer.sh \
  --changelog ./CHANGELOG.md \
  --output ./update-server
```

This preserves all previous version entries in the manifest (supporting downgrade) and updates the `latest` field. The new version is prepended to the versions array.

A version cannot be added if it already exists in the manifest. Remove it manually from `foxy-app-updater.json` first if you need to re-publish.

### Server directory layout

After setting up updates, your server directory should look like:

```
update-server/
  foxy-app-updater.json
  installers/
    Foxy-1.0.0-setup.exe
    Foxy-1.0.0-linux-installer.sh
    Foxy-0.8.1-setup.exe
    Foxy-0.8.1-linux-installer.sh
  changelogs/
    1.0.0.json
    0.8.1.json
```

Place the installer files in an `installers/` directory on the server. The manifest references them via relative paths (e.g., `installers/Foxy-1.0.0-setup.exe`).

### Connecting updates to repositories

To make Foxy clients auto-detect your update source, set `appUpdateUrl` in either:

- **`repository_space.json`** (highest priority):
  ```json
  "appUpdateUrl": "https://mods.example.com/updates/"
  ```

- **`repo.json`** via config.json:
  ```json
  "appUpdateUrl": "https://mods.example.com/updates/"
  ```

- **CLI override** when building the repository:
  ```
  foxy-server-backend-cli create config.json ./output --app-update-url https://mods.example.com/updates/
  ```

The URL should point to the directory containing `foxy-app-updater.json`. Foxy clients auto-fill this URL into their settings if the update source field is empty. A manually entered URL in the client is treated as a user override and is not replaced.

### Manifest format

The `foxy-app-updater.json` manifest follows this structure:

```json
{
  "schemaVersion": 1,
  "latest": "0.8.1",
  "versions": [
    {
      "version": "0.8.1",
      "changelog": "changelogs/0.8.1.json",
      "platforms": {
        "windows-x86_64": {
          "installerPath": "installers/Foxy-0.8.1-setup.exe",
          "installerHash": "<blake3-hex>",
          "installerSize": 12345678
        },
        "linux-x86_64": {
          "installerPath": "installers/Foxy-0.8.1-linux-installer.sh",
          "installerHash": "<blake3-hex>",
          "installerSize": 9876543
        }
      }
    },
    {
      "version": "1.0.0",
      "changelog": "changelogs/1.0.0.json",
      "platforms": { ... }
    }
  ]
}
```

Platform keys are `windows-x86_64` and `linux-x86_64`. Installer integrity is verified via BLAKE3 hash before the client runs the installer.

---

## Configuration Reference

### config.json (input to `foxy-server-backend-cli create`)

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `repoName` | string | yes | - | Display name of the repository |
| `basePath` | string | yes | - | Root directory containing mod folders |
| `appUpdateUrl` | string | no | `""` | URL to a Foxy app update server (written into `repo.json`) |
| `requiredMods` | array | no | `[]` | List of required mod references |
| `optionalMods` | array | no | `[]` | List of optional mod references |
| `iconImagePath` | string | no | `""` | Path to repository icon (relative to `basePath`) |
| `repoImagePath` | string | no | `""` | Path to repository banner image (relative to `basePath`) |
| `clientParameters` | string | no | `""` | Arma 3 launch parameters suggested to clients |
| `repoBasicAuthentication` | object | no | empty | HTTP Basic Auth credentials for protected repositories |
| `version` | string | no | `"3.2.0.0"` | Repository protocol version |
| `servers` | array | no | `[]` | Game server entries |

#### Mod reference format

```json
{ "modName": "@ace", "enabled": true }
```

- `modName` (string, required): Directory name or path relative to `basePath`. Supports glob wildcards in the final segment (`@*`, `collections/*`, `mods/@[ac]*`).
- `enabled` (boolean, optional, default `true`): Whether the mod is checked by default in the Foxy client.

Absolute paths are also supported in `modName` for mods stored outside `basePath`.

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

- `name` (string, required): Display name
- `address` (string, required): Server IP or hostname
- `port` (string, required): Game port
- `password` (string, optional, default `""`): Server password
- `battleEye` (boolean, optional, default `false`): Whether BattlEye is enabled

#### Basic authentication

```json
"repoBasicAuthentication": {
  "username": "myuser",
  "password": "mypassword"
}
```

When set, Foxy clients send HTTP Basic Auth headers with every request to this repository. Leave both fields empty to disable.

### repo.json (generated output)

The following fields appear in the generated `repo.json`:

| Field | Description |
|-------|-------------|
| `repoName` | Repository display name |
| `checksum` | Repository-level checksum (BLAKE3 in FoxyMode, SHA-1 in SwiftyMode) |
| `foxyMode` | Present when FoxyMode or HybridMode is used. Value: `"FoxyModeV1"` |
| `requiredMods` | Array of `{ modName, checkSum, enabled }` (empty in FoxyMode, MD5 in Swifty/Hybrid) |
| `optionalMods` | Array of `{ modName, checkSum, enabled }` (empty in FoxyMode, MD5 in Swifty/Hybrid) |
| `iconImagePath` | Relative path to icon image |
| `iconImageChecksum` | SHA-1 checksum of icon image |
| `repoImagePath` | Relative path to banner image |
| `repoImageChecksum` | SHA-1 checksum of banner image |
| `appUpdateUrl` | URL to Foxy app update manifest (omitted if empty) |
| `clientParameters` | Suggested Arma 3 launch parameters |
| `repoBasicAuthentication` | `{ username, password }` for HTTP Basic Auth |
| `version` | Repository protocol version |
| `servers` | Array of game server entries |

### foxy_addons.json (FoxyMode/HybridMode)

| Field | Description |
|-------|-------------|
| `version` | `"FoxyModeV1"` |
| `hashAlgorithm` | `"BLAKE3"` |
| `checksum` | Repository-level BLAKE3 checksum |
| `requiredMods` | Array of `{ modName, checkSum, enabled }` with BLAKE3 checksums |
| `optionalMods` | Array of `{ modName, checkSum, enabled }` with BLAKE3 checksums |

### foxy_addon.json (per-mod, FoxyMode/HybridMode)

| Field | Description |
|-------|-------------|
| `name` | Mod directory name (lowercased) |
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
| `fileType` | `"FoxyPboFile"` for `.pbo` files, `"FoxyFile"` for others |
| `parts` | Array of `{ path, checksum, start, length }` sub-file parts |

### DLC content (client-side)

The example `repo.json` also supports a `dlcContent` object that controls which Arma 3 DLC content is suggested for players. This is configured directly in the `repo.json` (not generated by `foxy-server-backend-cli` at this time):

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

Players can choose to apply these DLC suggestions via their Foxy settings (`apply_repo_json_dlc_content`).

---

## Hosting Behind a Reverse Proxy

### nginx

A minimal nginx configuration for serving a Foxy repository:

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

        # CORS headers (needed if clients use browser-based access)
        add_header Access-Control-Allow-Origin "*" always;
        add_header Access-Control-Allow-Methods "GET, HEAD, OPTIONS" always;

        # Cache manifests briefly so updates propagate quickly
        location ~* \.(json)$ {
            expires 5m;
            add_header Cache-Control "public, max-age=300";
        }

        # Cache mod files longer (they are checksum-verified)
        location ~* \.(pbo|bikey|bisign|cpp|bin|png|jpg)$ {
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

### Apache

```apache
<VirtualHost *:443>
    ServerName mods.example.com

    SSLEngine on
    SSLCertificateFile    /etc/ssl/certs/mods.example.com.pem
    SSLCertificateKeyFile /etc/ssl/private/mods.example.com.key

    DocumentRoot /var/www/foxy-repo

    <Directory /var/www/foxy-repo>
        Options -Indexes
        AllowOverride None
        Require all granted

        Header set Access-Control-Allow-Origin "*"
        Header set Access-Control-Allow-Methods "GET, HEAD, OPTIONS"
    </Directory>

    # Cache control for JSON manifests
    <FilesMatch "\.(json)$">
        Header set Cache-Control "public, max-age=300"
    </FilesMatch>

    # Cache control for mod files
    <FilesMatch "\.(pbo|bikey|bisign)$">
        Header set Cache-Control "public, max-age=604800"
    </FilesMatch>

    Alias /foxy/ /var/www/foxy-updates/
</VirtualHost>
```

### General tips

- **HTTPS is recommended.** Foxy supports HTTP, but HTTPS protects file integrity in transit.
- **Disable directory listings.** The manifests contain all the information clients need. Exposed listings can leak your directory structure.
- **Set appropriate cache headers.** JSON manifests (`repo.json`, `foxy_addons.json`, `foxy_addon.json`) should have short TTLs (5-15 minutes) so updates propagate quickly. Mod files (`.pbo`, `.bikey`, etc.) can be cached longer since they are verified by checksum.
- **HTTP range requests** should be supported by your server for efficient partial downloads and delta patching.
- **Compression.** Enable gzip/brotli for `.json` files. Mod files (`.pbo`) are already compressed and do not benefit from transport compression.
- **Basic authentication.** If you configured `repoBasicAuthentication` in the config, ensure your web server also enforces HTTP Basic Auth for the repository path.
- **Bandwidth.** Large Arma 3 modsets can be tens of gigabytes. Plan your hosting accordingly and consider Foxy's delta patching, which significantly reduces update sizes.

---

## Maintaining Repositories

### Updating after mod changes

When mods are updated (new versions from Steam Workshop, custom mod changes, etc.), re-run the `create` command with the same config:

```
foxy-server-backend-cli create config.json ./output --mode foxy
```

This re-discovers all files, recomputes checksums, and regenerates all manifest files. The output directory is overwritten with the new content.

Foxy clients detect changes via the repository-level checksum in `repo.json`. When the checksum changes, connected clients know an update is available and will sync only the files (and file parts) that differ.

### Adding or removing mods

1. Edit `config.json` to add or remove entries from `requiredMods` / `optionalMods`.
2. Re-run `foxy-server-backend-cli create config.json ./output`.
3. The generated `repo.json` (and `foxy_addons.json` in FoxyMode) reflects the updated mod list.

### Switching generation modes

You can switch modes at any time by changing the `--mode` flag. If switching from Swifty to Foxy, Foxy clients will detect the `foxyMode` field in `repo.json` and use the new manifests. Legacy Swifty clients will stop working unless you use HybridMode.

A recommended migration path:

1. Start with `--mode hybrid` to serve both clients.
2. Wait for your community to switch to Foxy.
3. Switch to `--mode foxy` to drop legacy artifacts and benefit from faster BLAKE3 hashing.

### Automation

Since `foxy-server-backend-cli` is a single command-line tool, it integrates easily into scripts and CI pipelines:

```bash
#!/bin/bash
# rebuild-repo.sh - Run after Steam Workshop updates
set -e

REPO_CONFIG="/etc/foxy/config.json"
OUTPUT_DIR="/var/www/foxy-repo"

echo "Rebuilding Foxy repository..."
foxy-server-backend-cli --no-progress create "$REPO_CONFIG" "$OUTPUT_DIR" --threads 4

echo "Repository updated at $(date)"
```

For large repositories, using `--threads` with a value matching your available CPU cores can significantly speed up the hashing process. BLAKE3 (FoxyMode) benefits particularly from multi-threaded processing and larger I/O buffers.

---

## Troubleshooting

### "basePath does not exist or is not a directory"

The `basePath` in your config must point to an existing directory. Verify the path is correct and accessible:

```
ls -la /path/to/your/basePath
```

On Windows, use forward slashes or escaped backslashes in JSON:

```json
"basePath": "D:\\Arma3\\ServerMods"
```

or

```json
"basePath": "D:/Arma3/ServerMods"
```

### "Mod directory does not exist"

A mod name in `requiredMods` or `optionalMods` resolves to a directory that does not exist under `basePath`. Check the spelling and ensure the directory is present:

```
ls -la /path/to/basePath/@mod_name
```

### "Wildcard pattern matched no directories"

If you use wildcard patterns (e.g., `@*`), ensure that matching directories exist. This is a warning, not an error -- the build continues with whatever mods were found.

### "No mods found after expanding all mod references"

After expanding all wildcard and direct mod references, no valid mod directories were found. Verify that `basePath` contains the expected mod folders and that your mod references are correct.

### Clients see no update after rebuild

1. Check that the `checksum` field in `repo.json` actually changed. If the mod files are identical, the checksum will not change.
2. Check web server caching. If you have aggressive caching on JSON files, clients may be seeing a stale `repo.json`. Reduce the cache TTL for `.json` files.
3. Ensure the client's repository URL points to the correct directory (where `repo.json` is located).

### Clients cannot connect

1. Verify the URL is accessible from a browser: `https://mods.example.com/repo/repo.json` should return valid JSON.
2. Check for CORS issues if the client reports network errors.
3. If using basic authentication, verify the credentials match between `config.json` and the web server configuration.
4. Check that your web server supports HTTP range requests (required for efficient downloads).

### PBO parse warnings

If you see warnings like "PBO parse failed for ..., treating as single file", the PBO file may be malformed or use an unsupported format. The tool falls back to treating the entire file as a single chunk, which works correctly but disables per-entry delta patching for that file.

### Large repository build times

- Increase threads: `--threads 8` (or higher, matching your CPU cores)
- Use FoxyMode (`--mode foxy`) which uses BLAKE3, an algorithm designed for speed
- Ensure the source and output directories are on fast storage (SSD preferred)
- For very large modsets, consider splitting into multiple repositories grouped by a repository space

### Version already exists in app updater manifest

When running `new-app-update`, the version must not already exist in the manifest. To re-publish a version:

1. Open `foxy-app-updater.json` in a text editor
2. Remove the version entry from the `versions` array
3. Update the `latest` field if needed
4. Re-run `foxy-server-backend-cli new-app-update`
