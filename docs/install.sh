#!/bin/sh
# Installs or updates Foxy for the current user on Linux (x86_64, aarch64) or macOS. Run:
#
#   curl -fsSL https://yethesamartaka-foxy.github.io/Foxy/install.sh | sh
#   curl -fsSL https://yethesamartaka-foxy.github.io/Foxy/install.sh | sh -s -- --uninstall
#
# It downloads the newest release from GitHub and checks its SHA-256 before installing.
# A current-user install needs no root and stays in your home folder.
#
# Linux:  ~/.local/opt/foxy, a foxy command, and a menu entry. When the only existing
#         install is the system copy in /opt/foxy, this updates that copy and asks for
#         administrator permission. --uninstall removes the current-user copy.
# macOS:  ~/Applications/Foxy.app, from the Apple Silicon disk image when the release
#         includes one. --uninstall removes that app.
#
# Settings are kept: ~/.config/Foxy on Linux ($XDG_CONFIG_HOME/Foxy) and
# ~/Library/Application Support/Foxy on macOS. Downloaded mods stay in the folders
# you chose.
#
# FOXY_INSTALL_REPO overrides the GitHub repository (owner/name). Point it only at a
# repository you trust, because this script installs what that repository publishes.
# FOXY_INSTALL_DRY_RUN=1 prints the chosen download and stops before installing.
#
# Everything runs from main at the very end, so a download cut short never runs half a script.

set -eu

REPO=${FOXY_INSTALL_REPO:-YetheSamartaka-Foxy/Foxy}
# HTTPS only, redirects included.
PROTO='=https'
BUNDLE_ID=community.foxy.Foxy

say() { printf '%s\n' "$*"; }
fail() { printf 'foxy install: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Installs or updates Foxy for the current user.

Usage: install.sh [--uninstall]

  --uninstall  remove the current-user Foxy install (settings and mods are kept)
  --help       show this help
EOF
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "this needs '$1', which is not installed."
}

# The SHA-256 of file $1 in lower-case hex.
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print tolower($1) }'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{ print tolower($1) }'
    else
        fail "this needs 'sha256sum' or 'shasum' to check the download."
    fi
}

# Prints field $3 from the GitHub asset object named $2 in JSON $1.
json_field() {
    printf '%s' "$1" | awk -v name="$2" -v field="$3" '
        {
            want = "\"name\":\"" name "\""
            i = index($0, want)
            if (i == 0) exit 1
            rest = substr($0, i)
            nextname = index(substr(rest, 2), "\"name\":\"")
            if (nextname > 0) rest = substr(rest, 1, nextname)
            key = "\"" field "\":\""
            k = index(rest, key)
            if (k == 0) exit 1
            rest = substr(rest, k + length(key))
            q = index(rest, "\"")
            if (q == 0) exit 1
            print substr(rest, 1, q - 1)
        }
    '
}

json_has() {
    json_field "$1" "$2" name >/dev/null 2>&1
}

fetch_release() {
    need curl
    release_json=$(curl -fsSL --retry 3 --proto "$PROTO" --proto-redir "$PROTO" \
        -H "User-Agent: Foxy-installer" \
        -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/$REPO/releases/latest") \
        || fail "could not look up the newest Foxy release. Nothing was installed."
    # One line, and no space after ":", so the asset search works for both compact and
    # pretty-printed GitHub responses.
    release_json=$(printf '%s' "$release_json" | tr -d '\n\r\t' | sed 's/:[[:space:]]*/:/g')
    tag=$(printf '%s' "$release_json" | awk '
        {
            key = "\"tag_name\":\""
            i = index($0, key)
            if (i == 0) exit 1
            rest = substr($0, i + length(key))
            q = index(rest, "\"")
            if (q == 0) exit 1
            print substr(rest, 1, q - 1)
            exit 0
        }
    ') || fail "could not read the newest Foxy release. Nothing was installed."
    version=$tag
    version=${version#v}
    [ -n "$version" ] || fail "could not read the newest Foxy release. Nothing was installed."
}

# Sets asset, asset_url, and expected for $1.
choose_asset() {
    asset=$1
    json_has "$release_json" "$asset" || fail "Release $tag has no $asset. Nothing was installed."
    asset_url=$(json_field "$release_json" "$asset" browser_download_url | sed 's|\\/|/|g')
    [ -n "$asset_url" ] || fail "Release $tag has no download URL for $asset. Nothing was installed."
    expected_sha256 "$asset"
}

# Sets expected to the SHA-256 for asset $1, from its .sha256 file, one SHA256SUMS.txt
# line, or the release digest.
expected_sha256() {
    name=$1
    expected=
    digest=$(json_field "$release_json" "$name" digest 2>/dev/null || true)
    case "$digest" in
        sha256:[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]*)
            digest=$(printf '%s' "$digest" | awk '{ print tolower(substr($0, 8)) }')
            ;;
        *) digest= ;;
    esac

    from_file=
    if json_has "$release_json" "$name.sha256"; then
        sidecar_url=$(json_field "$release_json" "$name.sha256" browser_download_url | sed 's|\\/|/|g')
        sidecar_text=$(curl -fsSL --retry 3 --proto "$PROTO" --proto-redir "$PROTO" \
            -H "User-Agent: Foxy-installer" "$sidecar_url") \
            || fail "could not download $name.sha256. Nothing was installed."
        from_file=$(printf '%s' "$sidecar_text" | tr -d '\r' | awk 'NR == 1 { print tolower($1) }')
    elif json_has "$release_json" "SHA256SUMS.txt"; then
        sums_url=$(json_field "$release_json" "SHA256SUMS.txt" browser_download_url | sed 's|\\/|/|g')
        sums_text=$(curl -fsSL --retry 3 --proto "$PROTO" --proto-redir "$PROTO" \
            -H "User-Agent: Foxy-installer" "$sums_url") \
            || fail "could not download SHA256SUMS.txt. Nothing was installed."
        from_file=$(printf '%s' "$sums_text" | tr -d '\r' | awk -v name="$name" '
            {
                hash = tolower($1)
                file = $2
                gsub(/^\*/, "", file)
                if (file == name && hash ~ /^[0-9a-f]{64}$/) {
                    if (seen && seen != hash) conflict = 1
                    seen = hash
                }
            }
            END {
                if (!conflict && seen) print seen
            }
        ')
    fi

    if [ -n "$from_file" ] && [ -n "$digest" ] && [ "$from_file" != "$digest" ]; then
        fail "the published checksum for $name does not match the release digest. Nothing was installed."
    fi
    result=$from_file
    [ -n "$result" ] || result=$digest
    printf '%s' "$result" | awk 'BEGIN { ok = 0 } /^[0-9a-f]{64}$/ { ok = 1 } END { exit ok ? 0 : 1 }' \
        || fail "Release $tag has no SHA-256 for $name. Nothing was installed."
    expected=$result
}

download_asset() {
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/foxy-install.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' HUP INT TERM
    say "Downloading $asset..."
    curl -fL --retry 3 --proto "$PROTO" --proto-redir "$PROTO" \
        -H "User-Agent: Foxy-installer" --progress-bar \
        -o "$tmp/$asset" "$asset_url" \
        || fail "could not download $asset_url"
    actual=$(sha256 "$tmp/$asset")
    [ "$actual" = "$expected" ] || fail "the download is damaged (SHA-256 does not match). Nothing was installed."
}

linux_user_prefix() {
    printf '%s\n' "$HOME/.local/opt/foxy"
}

linux_owned() {
    [ -f "$1/.installed_by_foxy_installer" ] || [ -f "$1/.foxy_install_prefix" ]
}

linux_running() {
    target=$1/Foxy
    for proc in /proc/[0-9]*; do
        [ -e "$proc/exe" ] || continue
        exe=$(readlink "$proc/exe" 2>/dev/null || true)
        exe=${exe%" (deleted)"}
        if [ "$exe" = "$target" ]; then
            return 0
        fi
    done
    return 1
}

linux_prefix_note() {
    if command -v foxy >/dev/null 2>&1; then
        say "Start it from your applications menu, or run: foxy"
    else
        say "Start it from your applications menu, or run: $1/Foxy"
        say "Note: $HOME/.local/bin is not in your PATH, so the 'foxy' command needs the full path. The menu entry works either way."
    fi
    say "Run this command again to update. Add --uninstall to remove the current-user copy."
}

linux_install() {
    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) fail "Foxy for Linux needs a 64-bit x86 or ARM processor. This one reports '$(uname -m)'." ;;
    esac
    need bash
    fetch_release
    choose_asset "Foxy-$version-linux-$arch-installer.sh"
    user_prefix=$(linux_user_prefix)
    system_prefix=/opt/foxy
    prefix=$user_prefix
    elevate=0
    existed=
    if linux_owned "$user_prefix"; then
        existed=1
        prefix=$user_prefix
    elif linux_owned "$system_prefix"; then
        existed=1
        prefix=$system_prefix
        elevate=1
    elif [ -x "$system_prefix/Foxy" ]; then
        say "Foxy is present in $system_prefix and was not installed by the Foxy installer. This command installs a separate copy for your user."
    fi

    if [ "${FOXY_INSTALL_DRY_RUN:-}" = 1 ]; then
        say "dry-run version=$version asset=$asset sha256=$expected prefix=$prefix"
        return 0
    fi

    download_asset
    say "A running Foxy is closed and then started again. Saved repositories, settings, and databases are kept."
    if [ "$elevate" -eq 1 ]; then
        say "Foxy is installed for all users in $prefix. This update needs administrator permission."
        need sudo
        sudo env HOME="$HOME" bash "$tmp/$asset" --silent --prefix="$prefix" \
            || fail "the installer did not finish. Saved Foxy data was left in place."
    else
        bash "$tmp/$asset" --silent --prefix="$prefix" \
            || fail "the installer did not finish. Saved Foxy data was left in place."
    fi

    if [ -n "$existed" ]; then
        say "Foxy is updated to $version: $prefix"
    else
        say "Foxy $version is installed: $prefix"
    fi
    linux_prefix_note "$prefix"
}

linux_remove_link() {
    link=$1
    prefix=$2
    if [ -L "$link" ]; then
        target=$(readlink "$link" 2>/dev/null || true)
        if [ "$target" = "$prefix/Foxy" ]; then
            rm -f "$link"
            say "Removed $link"
        fi
    fi
}

linux_remove_desktop() {
    file=$1
    prefix=$2
    if [ -f "$file" ] && grep -F "$prefix/Foxy" "$file" >/dev/null 2>&1; then
        rm -f "$file"
        say "Removed $file"
        share=$(dirname "$file")
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database -q "$share" 2>/dev/null || true
        fi
    fi
}

linux_uninstall() {
    prefix=$(linux_user_prefix)
    if [ ! -d "$prefix" ] || [ -L "$prefix" ] || ! linux_owned "$prefix"; then
        say "Foxy was not installed for this user by the install command (there is no $prefix), so nothing was removed."
        if linux_owned /opt/foxy; then
            say "A system install is in /opt/foxy. Remove it with the Linux installer from the latest release:"
            say "  sudo sh ./Foxy-<version>-linux-<arch>-installer.sh --uninstall --prefix=/opt/foxy"
        fi
        return 0
    fi
    if linux_running "$prefix"; then
        fail "Foxy is running. Quit it and run this command again."
    fi
    rm -rf "$prefix"
    say "Removed $prefix"
    linux_remove_link "$HOME/.local/bin/foxy" "$prefix"
    linux_remove_link /usr/local/bin/foxy "$prefix"
    linux_remove_desktop "$HOME/.local/share/applications/foxy.desktop" "$prefix"
    linux_remove_desktop /usr/share/applications/foxy.desktop "$prefix"
    config=${XDG_CONFIG_HOME:-$HOME/.config}
    case "$config" in
        /*) ;;
        *) config=$HOME/.config ;;
    esac
    say "Foxy is removed. Your settings stay in $config/Foxy. Downloaded mods stay in the folders you chose."
}

bundle_id() {
    /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist" 2>/dev/null || true
}

macos_running() {
    command -v pgrep >/dev/null 2>&1 && pgrep -f "$1/Contents/MacOS/Foxy" >/dev/null 2>&1
}

macos_select_asset() {
    asset_name=
    case "$(uname -m)" in
        arm64 | aarch64)
            if json_has "$release_json" "Foxy-$version-macos-arm64.dmg"; then
                asset_name="Foxy-$version-macos-arm64.dmg"
                return 0
            fi
            ;;
        x86_64 | amd64)
            if json_has "$release_json" "Foxy-$version-macos-x86_64.dmg"; then
                asset_name="Foxy-$version-macos-x86_64.dmg"
                return 0
            fi
            ;;
        *)
            fail "Foxy for macOS needs an Apple Silicon or Intel Mac. This one reports '$(uname -m)'."
            ;;
    esac
    if json_has "$release_json" "Foxy-$version-macos-universal.dmg"; then
        asset_name="Foxy-$version-macos-universal.dmg"
        return 0
    fi
    fail "Release $tag has no Foxy disk image for this Mac. Nothing was installed."
}

macos_install() {
    major=$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)
    if [ -n "$major" ] && [ "$major" -lt 11 ] 2>/dev/null; then
        fail "Foxy needs macOS 11 or newer. This Mac runs $(sw_vers -productVersion)."
    fi
    need ditto
    need hdiutil
    fetch_release
    macos_select_asset
    choose_asset "$asset_name"
    apps=$HOME/Applications
    app=$apps/Foxy.app
    if [ -e "$app" ] && [ "$(bundle_id "$app")" != "$BUNDLE_ID" ]; then
        fail "$app exists and is not Foxy. Move it away and run this again."
    fi
    if macos_running "$app"; then
        fail "Foxy is running. Quit Foxy and run this command again."
    fi
    existed=
    [ ! -e "$app" ] || existed=1

    if [ "${FOXY_INSTALL_DRY_RUN:-}" = 1 ]; then
        say "dry-run version=$version asset=$asset sha256=$expected app=$app"
        return 0
    fi

    download_asset
    mnt=$tmp/mnt
    mkdir -p "$mnt"
    hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$tmp/$asset" >/dev/null \
        || fail "could not open $asset."
    mounted=1
    trap 'if [ "${mounted:-}" = 1 ]; then hdiutil detach "$mnt" >/dev/null 2>&1 || true; fi; rm -rf "$tmp"' EXIT
    src=$mnt/Foxy.app
    [ -d "$src" ] || fail "$asset does not contain Foxy.app."
    [ "$(bundle_id "$src")" = "$BUNDLE_ID" ] || fail "$asset does not contain Foxy."
    xattr -dr com.apple.quarantine "$src" 2>/dev/null || true
    mkdir -p "$apps"
    if [ -e "$app" ]; then
        rm -rf "$app"
    fi
    ditto "$src" "$app" || fail "could not copy Foxy.app into $apps."
    xattr -dr com.apple.quarantine "$app" 2>/dev/null || true
    hdiutil detach "$mnt" >/dev/null 2>&1 || true
    mounted=0

    if [ -n "$existed" ]; then
        say "Foxy is updated to $version: $app"
    else
        say "Foxy $version is installed: $app"
    fi
    say "Start it from Launchpad or Spotlight, or run: open \"$app\""
    say "Run this command again to update. Add --uninstall to remove this copy."
    open "$app" >/dev/null 2>&1 || true
}

macos_uninstall() {
    app=$HOME/Applications/Foxy.app
    if [ ! -e "$app" ]; then
        say "There is no $app, so nothing was removed."
        return 0
    fi
    [ "$(bundle_id "$app")" = "$BUNDLE_ID" ] || fail "$app is not Foxy. It was left alone."
    if macos_running "$app"; then
        fail "Foxy is running. Quit Foxy and run this command again."
    fi
    rm -rf "$app"
    say "Removed $app"
    say "Foxy is removed. Your settings stay in ~/Library/Application Support/Foxy. Downloaded mods stay in the folders you chose."
}

main() {
    uninstall=
    for arg in "$@"; do
        case "$arg" in
            --uninstall) uninstall=1 ;;
            -h | --help) usage; return 0 ;;
            *) usage >&2; fail "unknown option: $arg" ;;
        esac
    done
    case "$REPO" in
        [A-Za-z0-9_.-]*/[A-Za-z0-9_.-]*) ;;
        *) fail "FOXY_INSTALL_REPO must look like owner/name. Nothing was installed." ;;
    esac
    case "$REPO" in
        */*/*) fail "FOXY_INSTALL_REPO must look like owner/name. Nothing was installed." ;;
    esac
    [ "$(id -u)" -ne 0 ] || fail "run this as your own user, not as root. Foxy installs into your home folder."
    if [ -z "${HOME:-}" ] || [ ! -d "$HOME" ]; then
        fail "HOME is not set to a folder."
    fi

    case "$(uname -s)" in
        Linux)
            if [ -n "$uninstall" ]; then linux_uninstall; else linux_install; fi
            ;;
        Darwin)
            if [ -n "$uninstall" ]; then macos_uninstall; else macos_install; fi
            ;;
        *)
            fail "this installer is for Linux and macOS. On Windows, in PowerShell: irm https://yethesamartaka-foxy.github.io/Foxy/install.ps1 | iex"
            ;;
    esac
}

main "$@"
