#!/bin/sh
set -eu

DOMAIN="gui/$(id -u)"
PLIST_DIR="$HOME/Library/LaunchAgents"
BACKUP_ROOT="${SYMBIONT_MIGRATION_BACKUP_DIR:-$HOME/Library/Application Support/Symbiont/migration-backups}"
LEGACY_LABELS="
com.glenzli.symbiont-d.pcp
com.glenzli.symbiont-d.pcp-console
"

backup_dir=''

for legacy_label in $LEGACY_LABELS; do
  launchctl bootout "$DOMAIN/$legacy_label" >/dev/null 2>&1 || true
  launchctl disable "$DOMAIN/$legacy_label"

  retire_attempt=0
  while launchctl print "$DOMAIN/$legacy_label" >/dev/null 2>&1; do
    retire_attempt=$((retire_attempt + 1))
    if [ "$retire_attempt" -ge 10 ]; then
      echo "Could not retire $legacy_label after 10 seconds" >&2
      exit 1
    fi
    sleep 1
  done

  legacy_plist="$PLIST_DIR/$legacy_label.plist"
  if [ -e "$legacy_plist" ]; then
    if [ -z "$backup_dir" ]; then
      migration_timestamp="$(date -u '+%Y%m%dT%H%M%SZ')"
      backup_dir="$BACKUP_ROOT/legacy-pcp-launchagents-$migration_timestamp"
      mkdir -p "$backup_dir"
      chmod 700 "$backup_dir"
    fi
    mv "$legacy_plist" "$backup_dir/$legacy_label.plist"
    chmod 600 "$backup_dir/$legacy_label.plist"
  fi
done

if [ -n "$backup_dir" ]; then
  echo "Retired legacy Symbiont PCP LaunchAgents; backup: $backup_dir"
else
  echo "Legacy Symbiont PCP LaunchAgents are retired."
fi
