#!/bin/sh
# Installs sysmon: the program, its app-menu launcher and its icon.
#
#   ./install.sh               install for the current user (~/.local), no sudo
#   sudo ./install.sh --system install for everyone (/usr/local)
#   ./install.sh --uninstall   remove it again (add --system for /usr/local)
#
# Run it from an unpacked release download (which already contains the
# `sysmon` program) or from a source checkout (it builds with cargo first).
set -eu

cd "$(dirname "$0")"
PREFIX="$HOME/.local"
ACTION=install
for arg in "$@"; do
    case "$arg" in
        --system) PREFIX=/usr/local ;;
        --uninstall) ACTION=uninstall ;;
        -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install.sh: unknown option '$arg' (try --help)" >&2; exit 2 ;;
    esac
done

BIN="$PREFIX/bin/sysmon"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor"
META="$PREFIX/share/metainfo"
METAFILE=io.github.blissnathan1305.sysmon.metainfo.xml

refresh() {
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$APPS" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$ICONS" 2>/dev/null || true
}

if [ "$ACTION" = uninstall ]; then
    rm -f "$BIN" "$APPS/sysmon.desktop" "$ICONS/scalable/apps/sysmon.svg" "$META/$METAFILE"
    refresh
    echo "sysmon removed from $PREFIX. Your settings are kept in ~/.config/sysmon (delete that folder to remove them)."
    exit 0
fi

if [ -x ./sysmon ] && [ ! -f Cargo.toml ]; then
    SRC=./sysmon
else
    if ! command -v cargo >/dev/null 2>&1; then
        echo "Rust is needed to build sysmon from source. Install it from https://rustup.rs and run this again," >&2
        echo "or download a ready-made release from https://github.com/BlissNathan1305/sysmon/releases" >&2
        exit 1
    fi
    cargo build --release --locked
    SRC=target/release/sysmon
fi

mkdir -p "$PREFIX/bin" "$APPS" "$ICONS/scalable/apps" "$META"
install -m 755 "$SRC" "$BIN"
install -m 644 packaging/sysmon.svg "$ICONS/scalable/apps/sysmon.svg"
install -m 644 "packaging/$METAFILE" "$META/$METAFILE"
# Point the launcher at the installed program, in case $PREFIX/bin is not on PATH.
sed "s|^Exec=sysmon|Exec=$BIN|" packaging/sysmon.desktop > "$APPS/sysmon.desktop"
chmod 644 "$APPS/sysmon.desktop"
refresh

echo "sysmon installed to $BIN"
case ":$PATH:" in
    *":$PREFIX/bin:"*) echo "Start it with 'sysmon', or find 'System Monitor (sysmon)' in your app menu." ;;
    *) echo "Find 'System Monitor (sysmon)' in your app menu. To start it by typing 'sysmon', add $PREFIX/bin to your PATH." ;;
esac
