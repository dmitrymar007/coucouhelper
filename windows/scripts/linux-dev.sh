#!/bin/bash
# Runs a command (default: `npm run tauri dev`) without the environment the
# snap version of VS Code hands its terminals: GTK_PATH, GIO_MODULE_DIR,
# LOCPATH and friends point GTK at the snap's own libraries, and Coucou then
# dies with a `symbol lookup error` in /snap/core20; GDK_BACKEND=x11 keeps it
# off Wayland.
#
#   windows/scripts/linux-dev.sh                 # npm run tauri dev
#   windows/scripts/linux-dev.sh ./target/debug/coucou
set -e
cd "$(dirname "$0")/.."
# The VS Code snap may run itself through X11 and pass GDK_BACKEND=x11 on to
# everything started from its terminal. That is VS Code's choice, not ours:
# drop it, so Coucou picks its backend itself (native Wayland with the GNOME
# extension). Started from any other terminal, an explicit GDK_BACKEND stays.
if [ "$SNAP_NAME" = "code" ]; then
  unset GDK_BACKEND
fi
if [ -n "$XDG_DATA_DIRS_VSCODE_SNAP_ORIG" ]; then
  export XDG_DATA_DIRS="$XDG_DATA_DIRS_VSCODE_SNAP_ORIG"
fi
if [ -n "$XDG_CONFIG_DIRS_VSCODE_SNAP_ORIG" ]; then
  export XDG_CONFIG_DIRS="$XDG_CONFIG_DIRS_VSCODE_SNAP_ORIG"
fi
unset GTK_PATH GTK_EXE_PREFIX GTK_IM_MODULE_FILE GDK_PIXBUF_MODULE_FILE GDK_PIXBUF_MODULEDIR \
  GIO_MODULE_DIR GSETTINGS_SCHEMA_DIR LOCPATH GIO_LAUNCHED_DESKTOP_FILE \
  XDG_DATA_DIRS_VSCODE_SNAP_ORIG XDG_CONFIG_DIRS_VSCODE_SNAP_ORIG
for v in $(env | grep -o '^SNAP[A-Z_]*'); do unset "$v"; done
if [ $# -eq 0 ]; then
  exec npm run tauri dev
fi
exec "$@"
