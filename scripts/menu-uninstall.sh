#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-menu"
DOMAIN="gui/$(id -u)"
APP_DIR="${SYMBIONT_APP_DIR:-$HOME/Applications}"
APP_PATH="$APP_DIR/SymbiontMenu.app"
PLIST_PATH="$HOME/Library/LaunchAgents/$LABEL.plist"

launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
rm -f "$PLIST_PATH"
rm -rf "$APP_PATH"

echo "Uninstalled $LABEL"
echo "The symbiont-d daemon and local data were preserved."
