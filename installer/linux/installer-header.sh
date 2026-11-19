#!/bin/bash
# Foxy Installer for Linux
# This is a self-extracting installer. The tarball is appended after the __ARCHIVE__ marker.
set -e

PREFIX="/opt/foxy"
SILENT=0
UNINSTALL=0
DESKTOP_FILE=1

APP_NAME="Foxy"
BIN_NAME="Foxy"
DESKTOP_NAME="foxy.desktop"
PREFIX_MARKER=".foxy_install_prefix"
INSTALL_MARKER=".installed_by_foxy_installer"
NEED_SUDO=0

usage() {
    echo "Foxy Installer"
    echo ""
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  --silent          Run without prompts (for auto-updates)"
    echo "  --prefix=PATH     Install directory (default: /opt/foxy)"
    echo "  --no-desktop      Skip .desktop file installation"
    echo "  --uninstall       Remove Foxy installation"
    echo "  --help            Show this help"
}

for arg in "$@"; do
    case "$arg" in
        --silent)
            SILENT=1
            ;;
        --prefix=*)
            PREFIX="${arg#*=}"
            ;;
        --no-desktop)
            DESKTOP_FILE=0
            ;;
        --uninstall)
            UNINSTALL=1
            ;;
        --help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $arg"
            usage
            exit 1
            ;;
    esac
done

target_pids() {
    local target_exe
    target_exe="$(readlink -f "$PREFIX/$BIN_NAME" 2>/dev/null || printf '%s' "$PREFIX/$BIN_NAME")"
    local pid
    local proc_exe
    local proc_exe_canonical
    for proc in /proc/[0-9]*; do
        [ -e "$proc/exe" ] || continue
        pid="${proc##*/}"
        proc_exe="$(readlink "$proc/exe" 2>/dev/null || true)"
        proc_exe="${proc_exe% (deleted)}"
        proc_exe_canonical="$(readlink -f "$proc_exe" 2>/dev/null || printf '%s' "$proc_exe")"
        if [ "$proc_exe_canonical" = "$target_exe" ]; then
            echo "$pid"
        fi
    done
}

# Kill only instances running from this install prefix.
kill_running_instances() {
    local found=0
    local pid
    for pid in $(target_pids); do
        found=1
        kill "$pid" 2>/dev/null || true
    done
    if [ "$found" -eq 0 ]; then
        return 1
    fi
    return 0
}

wait_for_exit() {
    local timeout="${1:-10}"
    for i in $(seq 1 "$timeout"); do
        if [ -z "$(target_pids)" ]; then
            return 0
        fi
        sleep 1
    done
    return 1
}

path_needs_sudo() {
    local path="$1"
    local parent
    if [ "$(id -u)" -eq 0 ]; then
        return 1
    fi
    if [ -e "$path" ]; then
        [ ! -w "$path" ]
        return
    fi
    parent="$(dirname "$path")"
    while [ ! -e "$parent" ] && [ "$parent" != "/" ]; do
        parent="$(dirname "$parent")"
    done
    [ ! -w "$parent" ]
}

refresh_desktop_database() {
    local desktop_dir="$1"
    local use_sudo="${2:-0}"
    if command -v update-desktop-database > /dev/null 2>&1; then
        if [ "$use_sudo" -eq 1 ]; then
            sudo update-desktop-database "$desktop_dir" 2>/dev/null || true
        else
            update-desktop-database "$desktop_dir" 2>/dev/null || true
        fi
    fi
}

run_cmd() {
    if [ "$NEED_SUDO" -eq 1 ]; then
        sudo "$@"
    else
        "$@"
    fi
}

render_desktop_entry() {
    local prefix="$1"
    local template="$2"
    local prefix_escaped
    prefix_escaped="$(printf '%s' "$prefix" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g')"
    awk -v prefix="$prefix" -v prefix_escaped="$prefix_escaped" \
        '{gsub(/@PREFIX_ESCAPED@/, prefix_escaped); gsub(/@PREFIX@/, prefix); print}' \
        "$template"
}

if [ "$UNINSTALL" -eq 1 ]; then
    PREFIX="$(realpath -e -- "$PREFIX")"
    if [ "$PREFIX" = / ] || [ ! -f "$PREFIX/$PREFIX_MARKER" ] || [ -L "$PREFIX/$PREFIX_MARKER" ] || \
        [ ! -f "$PREFIX/$INSTALL_MARKER" ] || [ -L "$PREFIX/$INSTALL_MARKER" ]; then
        echo "Error: This directory is not a Foxy installation." >&2
        exit 1
    fi
    recorded_prefix="$(cat -- "$PREFIX/$PREFIX_MARKER")"
    # Legacy installers recorded relative prefixes without their working directory.
    if [ -z "$recorded_prefix" ] || \
        { [[ "$recorded_prefix" = /* ]] && [ "$(realpath -e -- "$recorded_prefix")" != "$PREFIX" ]; }; then
        echo "Error: The Foxy installation marker does not match the requested prefix." >&2
        exit 1
    fi

    echo "Uninstalling Foxy..."
    if kill_running_instances; then
        echo "Stopping running Foxy instance..."
        if ! wait_for_exit 10; then
            echo "Error: Foxy is still running. Close it before uninstalling." >&2
            exit 1
        fi
    fi

    if path_needs_sudo "$PREFIX"; then
        NEED_SUDO=1
    fi

    target_exe="$(realpath -m -- "$PREFIX/$BIN_NAME")"
    for link_dir in /usr/local/bin "$HOME/.local/bin"; do
        link_path="$link_dir/foxy"
        if [ -L "$link_path" ] && [ "$(realpath -m -- "$link_path")" = "$target_exe" ]; then
            if path_needs_sudo "$link_dir"; then
                sudo rm -f -- "$link_path"
            else
                rm -f -- "$link_path"
            fi
            echo "Removed symlink: $link_path"
        fi
    done

    if [ -f "$PREFIX/$DESKTOP_NAME" ] && [ ! -L "$PREFIX/$DESKTOP_NAME" ]; then
        for desktop_path in "$HOME/.local/share/applications/$DESKTOP_NAME" "/usr/share/applications/$DESKTOP_NAME"; do
            if [ -f "$desktop_path" ] && [ ! -L "$desktop_path" ] && \
                cmp -s -- "$desktop_path" <(render_desktop_entry "$recorded_prefix" "$PREFIX/$DESKTOP_NAME"); then
                desktop_dir="$(dirname "$desktop_path")"
                if path_needs_sudo "$desktop_dir"; then
                    sudo rm -f -- "$desktop_path"
                    refresh_desktop_database "$desktop_dir" 1
                else
                    rm -f -- "$desktop_path"
                    refresh_desktop_database "$desktop_dir" 0
                fi
                echo "Removed: $desktop_path"
            fi
        done
    fi
    run_cmd rm -f -- "$PREFIX/$BIN_NAME" "$PREFIX/libsteam_api.so" "$PREFIX/foxy.png" \
        "$PREFIX/$DESKTOP_NAME" "$PREFIX/$INSTALL_MARKER" "$PREFIX/$PREFIX_MARKER"
    run_cmd rmdir --ignore-fail-on-non-empty -- "$PREFIX"

    echo "Foxy has been uninstalled."
    exit 0
fi

PREFIX="$(realpath -m -- "$PREFIX")"
if [ "$PREFIX" = / ]; then
    echo "Error: The install prefix cannot be the filesystem root." >&2
    exit 1
fi

echo "Foxy Installer"
echo "Install directory: $PREFIX"
echo ""

if kill_running_instances; then
    echo "Stopping running Foxy instance..."
    if ! wait_for_exit 10; then
        for pid in $(target_pids); do
            kill -9 "$pid" 2>/dev/null || true
        done
        sleep 1
    fi
fi

if path_needs_sudo "$PREFIX" || path_needs_sudo "$PREFIX/$BIN_NAME" || path_needs_sudo "$PREFIX/$PREFIX_MARKER"; then
    NEED_SUDO=1
fi

if [ "$NEED_SUDO" -eq 1 ] && [ "$SILENT" -eq 1 ]; then
    if ! sudo -n true 2>/dev/null; then
        echo "Error: Updating $PREFIX requires sudo, but silent mode cannot prompt for a password." >&2
        exit 73
    fi
fi

run_cmd mkdir -p "$PREFIX"

ARCHIVE_LINE=$(awk '/^__ARCHIVE__$/{print NR + 1; exit 0;}' "$0")
if [ -z "$ARCHIVE_LINE" ]; then
    echo "Error: Archive marker not found. The installer may be corrupted."
    exit 1
fi

echo "Extracting files..."
tail -n +"$ARCHIVE_LINE" "$0" | run_cmd tar xzf - -C "$PREFIX"

run_cmd chmod +x "$PREFIX/$BIN_NAME"
run_cmd touch "$PREFIX/$INSTALL_MARKER"
tmp_prefix="$(mktemp)"
trap 'rm -f -- "$tmp_prefix"' EXIT
printf '%s\n' "$PREFIX" > "$tmp_prefix"
run_cmd install -m 0644 "$tmp_prefix" "$PREFIX/$PREFIX_MARKER"
rm -f -- "$tmp_prefix"
trap - EXIT

echo "Installed $BIN_NAME to $PREFIX/"

LINK_CREATED=0
if [ -d "/usr/local/bin" ]; then
    if [ -w "/usr/local/bin" ] || [ "$NEED_SUDO" -eq 1 ] || [ "$(id -u)" -eq 0 ]; then
        run_cmd ln -sf "$PREFIX/$BIN_NAME" /usr/local/bin/foxy
        echo "Symlink created: /usr/local/bin/foxy"
        LINK_CREATED=1
    fi
fi

if [ "$LINK_CREATED" -eq 0 ]; then
    mkdir -p "$HOME/.local/bin"
    ln -sf "$PREFIX/$BIN_NAME" "$HOME/.local/bin/foxy"
    echo "Symlink created: $HOME/.local/bin/foxy"
fi

if [ "$DESKTOP_FILE" -eq 1 ] && [ -f "$PREFIX/$DESKTOP_NAME" ]; then
    if [ -f "/usr/share/applications/$DESKTOP_NAME" ]; then
        desktop_dir="/usr/share/applications"
    elif [ -f "$HOME/.local/share/applications/$DESKTOP_NAME" ]; then
        desktop_dir="$HOME/.local/share/applications"
    else
        desktop_dir="$HOME/.local/share/applications"
    fi

    if [ "$desktop_dir" = "/usr/share/applications" ]; then
        if [ "$NEED_SUDO" -eq 1 ] || [ "$(id -u)" -ne 0 ]; then
            NEED_DESKTOP_SUDO=1
        else
            NEED_DESKTOP_SUDO=0
        fi
    else
        NEED_DESKTOP_SUDO=0
    fi

    if [ "$NEED_DESKTOP_SUDO" -eq 1 ] && [ "$SILENT" -eq 1 ]; then
        if ! sudo -n true 2>/dev/null; then
            echo "Warning: Skipping system desktop entry update because sudo cannot prompt in silent mode." >&2
            DESKTOP_FILE=0
        fi
    fi

    if [ "$DESKTOP_FILE" -eq 1 ] && [ "$NEED_DESKTOP_SUDO" -eq 1 ]; then
        sudo mkdir -p "$desktop_dir"
    elif [ "$DESKTOP_FILE" -eq 1 ]; then
        mkdir -p "$desktop_dir"
    fi

    if [ "$DESKTOP_FILE" -eq 1 ]; then
    tmp_desktop="$(mktemp)"
    render_desktop_entry "$PREFIX" "$PREFIX/$DESKTOP_NAME" > "$tmp_desktop"
    if [ "$NEED_DESKTOP_SUDO" -eq 1 ]; then
        sudo install -m 0644 "$tmp_desktop" "$desktop_dir/$DESKTOP_NAME"
    else
        install -m 0644 "$tmp_desktop" "$desktop_dir/$DESKTOP_NAME"
    fi
    rm -f "$tmp_desktop"
    echo "Desktop entry installed: $desktop_dir/$DESKTOP_NAME"
    fi

    if [ "$DESKTOP_FILE" -eq 1 ] && [ "$desktop_dir" = "/usr/share/applications" ] && [ -f "$HOME/.local/share/applications/$DESKTOP_NAME" ]; then
        rm -f "$HOME/.local/share/applications/$DESKTOP_NAME"
        echo "Removed stale per-user desktop entry."
    fi

    if [ "$DESKTOP_FILE" -eq 1 ]; then
        refresh_desktop_database "$desktop_dir" "$NEED_DESKTOP_SUDO"
    fi
fi

echo ""
echo "Foxy has been installed successfully!"
echo "  Binary:  $PREFIX/$BIN_NAME"
echo "  Command: foxy"
echo ""

if [ "$SILENT" -eq 1 ]; then
    echo "Relaunching Foxy..."
    nohup "$PREFIX/$BIN_NAME" > /dev/null 2>&1 &
fi

exit 0

# The tarball is appended below this marker by build-installer.sh
__ARCHIVE__
