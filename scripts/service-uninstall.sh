#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-d"
DOMAIN="gui/$(id -u)"
PLIST_PATH="$HOME/Library/LaunchAgents/$LABEL.plist"

launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
rm -f "$PLIST_PATH"

echo "Uninstalled $LABEL."
echo "PCP Runtime, PCP Console, and local data were left untouched."
