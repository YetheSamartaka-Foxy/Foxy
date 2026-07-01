#!/bin/bash
# Foxy Installer for Linux
# This is a self-extracting installer. The tarball is appended after the __ARCHIVE__ marker.
set -e

# Defaults
PREFIX="/opt/foxy"
SILENT=0
UNINSTALL=0
DESKTOP_FILE=1

APP_NAME="Foxy"
BIN_NAME="Foxy"
DESKTOP_NAME="foxy.desktop"
PREFIX_MARKER=".foxy_install_prefix"

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

# Parse arguments
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

# Uninstall mode
if [ "$UNINSTALL" -eq 1 ]; then
    echo "Uninstalling Foxy..."

    # Kill running instance
    if kill_running_instances; then
        echo "Stopping running Foxy instance..."
        sleep 2
    fi

    # Remove installation directory
    if [ -d "$PREFIX" ]; then
        if [ -w "$PREFIX" ] || [ "$(id -u)" -eq 0 ]; then
            rm -rf "$PREFIX"
        else
            sudo rm -rf "$PREFIX"
        fi
        echo "Removed: $PREFIX"
    fi

    # Remove symlink
    for link_dir in /usr/local/bin "$HOME/.local/bin"; do
        if [ -L "$link_dir/foxy" ]; then
            if [ -w "$link_dir" ] || [ "$(id -u)" -eq 0 ]; then
                rm -f "$link_dir/foxy"
            else
                sudo rm -f "$link_dir/foxy"
            fi
            echo "Removed symlink: $link_dir/foxy"
        fi
    done

    # Remove desktop files
    for desktop_path in "$HOME/.local/share/applications/$DESKTOP_NAME" "/usr/share/applications/$DESKTOP_NAME"; do
        if [ -f "$desktop_path" ]; then
            if path_needs_sudo "$desktop_path"; then
                sudo rm -f "$desktop_path"
                echo "Removed: $desktop_path"
                refresh_desktop_database "$(dirname "$desktop_path")" 1
            else
                rm -f "$desktop_path"
                echo "Removed: $desktop_path"
                refresh_desktop_database "$(dirname "$desktop_path")" 0
            fi
        fi
    done

    marker_path="$PREFIX/$PREFIX_MARKER"
    if [ -f "$marker_path" ] && [ ! -d "$PREFIX" ]; then
        if path_needs_sudo "$marker_path"; then
            sudo rm -f "$marker_path"
        else
            rm -f "$marker_path"
        fi
    fi

    echo "Foxy has been uninstalled."
    exit 0
fi

# --- Install mode ---

echo "Foxy Installer"
echo "Install directory: $PREFIX"
echo ""

# Stop running instance if present (check both Foxy and foxy process names)
if kill_running_instances; then
    echo "Stopping running Foxy instance..."
    # Wait up to 10 seconds for graceful exit
    if ! wait_for_exit 10; then
        # Force kill if still running
        for pid in $(target_pids); do
            kill -9 "$pid" 2>/dev/null || true
        done
        sleep 1
    fi
fi

# Determine if we need sudo for the prefix and files that will be replaced.
NEED_SUDO=0
if path_needs_sudo "$PREFIX" || path_needs_sudo "$PREFIX/$BIN_NAME" || path_needs_sudo "$PREFIX/$PREFIX_MARKER"; then
    NEED_SUDO=1
fi

if [ "$NEED_SUDO" -eq 1 ] && [ "$SILENT" -eq 1 ]; then
    if ! sudo -n true 2>/dev/null; then
        echo "Error: Updating $PREFIX requires sudo, but silent mode cannot prompt for a password." >&2
        exit 73
    fi
fi

run_cmd() {
    if [ "$NEED_SUDO" -eq 1 ]; then
        sudo "$@"
    else
        "$@"
    fi
}

# Create install directory
run_cmd mkdir -p "$PREFIX"

# Find the archive marker and extract
ARCHIVE_LINE=$(awk '/^__ARCHIVE__$/{print NR + 1; exit 0;}' "$0")
if [ -z "$ARCHIVE_LINE" ]; then
    echo "Error: Archive marker not found. The installer may be corrupted."
    exit 1
fi

echo "Extracting files..."
tail -n +"$ARCHIVE_LINE" "$0" | run_cmd tar xzf - -C "$PREFIX"

# Set executable permission
run_cmd chmod +x "$PREFIX/$BIN_NAME"
run_cmd touch "$PREFIX/.installed_by_foxy_installer"
printf '%s\n' "$PREFIX" > "/tmp/foxy_install_prefix.$$"
run_cmd install -m 0644 "/tmp/foxy_install_prefix.$$" "$PREFIX/$PREFIX_MARKER"
rm -f "/tmp/foxy_install_prefix.$$"

echo "Installed $BIN_NAME to $PREFIX/"

# Create symlink
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

# Install .desktop file
if [ "$DESKTOP_FILE" -eq 1 ] && [ -f "$PREFIX/$DESKTOP_NAME" ]; then
    # Detect where an existing .desktop file is installed
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

    # Use awk instead of sed to avoid regex metacharacter issues in PREFIX.
    if [ "$DESKTOP_FILE" -eq 1 ]; then
    prefix_escaped="$(printf '%s' "$PREFIX" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g')"
    tmp_desktop="$(mktemp)"
    awk -v prefix="$PREFIX" -v prefix_escaped="$prefix_escaped" \
        '{gsub(/@PREFIX_ESCAPED@/, prefix_escaped); gsub(/@PREFIX@/, prefix); print}' \
        "$PREFIX/$DESKTOP_NAME" > "$tmp_desktop"
    if [ "$NEED_DESKTOP_SUDO" -eq 1 ]; then
        sudo install -m 0644 "$tmp_desktop" "$desktop_dir/$DESKTOP_NAME"
    else
        install -m 0644 "$tmp_desktop" "$desktop_dir/$DESKTOP_NAME"
    fi
    rm -f "$tmp_desktop"
    echo "Desktop entry installed: $desktop_dir/$DESKTOP_NAME"
    fi

    # Clean up duplicate entry if we updated the system-wide location
    if [ "$DESKTOP_FILE" -eq 1 ] && [ "$desktop_dir" = "/usr/share/applications" ] && [ -f "$HOME/.local/share/applications/$DESKTOP_NAME" ]; then
        rm -f "$HOME/.local/share/applications/$DESKTOP_NAME"
        echo "Removed stale per-user desktop entry."
    fi

    # Update desktop database if available
    if [ "$DESKTOP_FILE" -eq 1 ]; then
        refresh_desktop_database "$desktop_dir" "$NEED_DESKTOP_SUDO"
    fi
fi

echo ""
echo "Foxy has been installed successfully!"
echo "  Binary:  $PREFIX/$BIN_NAME"
echo "  Command: foxy"
echo ""

# Relaunch if silent (auto-update mode)
if [ "$SILENT" -eq 1 ]; then
    echo "Relaunching Foxy..."
    nohup "$PREFIX/$BIN_NAME" > /dev/null 2>&1 &
fi

exit 0

# The tarball is appended below this marker by build-installer.sh
__ARCHIVE__
