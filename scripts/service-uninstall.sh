#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-d"
PCP_LABEL="$LABEL.pcp"
PCP_CONSOLE_LABEL="$LABEL.pcp-console"
DOMAIN="gui/$(id -u)"
PLIST_PATH="$HOME/Library/LaunchAgents/$LABEL.plist"
PCP_PLIST_PATH="$HOME/Library/LaunchAgents/$PCP_LABEL.plist"
PCP_CONSOLE_PLIST_PATH="$HOME/Library/LaunchAgents/$PCP_CONSOLE_LABEL.plist"

launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
launchctl bootout "$DOMAIN/$PCP_CONSOLE_LABEL" >/dev/null 2>&1 || true
launchctl bootout "$DOMAIN/$PCP_LABEL" >/dev/null 2>&1 || true
rm -f "$PLIST_PATH" "$PCP_PLIST_PATH" "$PCP_CONSOLE_PLIST_PATH"

echo "Uninstalled $LABEL, PCP Console, and the PCP runtime"
echo "Local data and logs were preserved."
