#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-menu"
DOMAIN="gui/$(id -u)"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
SOURCE_APP="$PROJECT_ROOT/macos/SymbiontMenu/build/SymbiontMenu.app"
APP_DIR="${SYMBIONT_APP_DIR:-$HOME/Applications}"
APP_PATH="$APP_DIR/SymbiontMenu.app"
TEMPLATE="$PROJECT_ROOT/packaging/launchd/$LABEL.plist.in"
PLIST_DIR="$HOME/Library/LaunchAgents"
PLIST_PATH="$PLIST_DIR/$LABEL.plist"
LOG_DIR="$PROJECT_ROOT/data/logs"
MAKE_BIN="${MAKE_BIN:-$(command -v make || true)}"

if [ -z "$MAKE_BIN" ]; then
  echo "make was not found; set MAKE_BIN to its absolute path" >&2
  exit 1
fi

xml_escape() {
  printf '%s' "$1" |
    sed \
      -e 's/&/\&amp;/g' \
      -e 's/</\&lt;/g' \
      -e 's/>/\&gt;/g' \
      -e 's/"/\&quot;/g' \
      -e "s/'/\&apos;/g"
}

sed_replacement() {
  xml_escape "$1" | sed -e 's/[|&]/\\&/g'
}

echo "Building SymbiontMenu..."
"$MAKE_BIN" -C "$PROJECT_ROOT/macos/SymbiontMenu" verify

mkdir -p "$APP_DIR" "$PLIST_DIR" "$LOG_DIR"
launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
unload_attempt=0
while launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; do
  unload_attempt=$((unload_attempt + 1))
  if [ "$unload_attempt" -ge 10 ]; then
    echo "Could not unload $LABEL after 10 seconds" >&2
    exit 1
  fi
  sleep 1
done
rm -rf "$APP_PATH"
ditto "$SOURCE_APP" "$APP_PATH"

TEMP_PLIST="$(mktemp "$PLIST_DIR/$LABEL.XXXXXX")"
trap 'rm -f "$TEMP_PLIST"' EXIT

sed \
  -e "s|@BINARY@|$(sed_replacement "$APP_PATH/Contents/MacOS/SymbiontMenu")|g" \
  -e "s|@LOG_DIR@|$(sed_replacement "$LOG_DIR")|g" \
  "$TEMPLATE" >"$TEMP_PLIST"

plutil -lint "$TEMP_PLIST" >/dev/null
chmod 0644 "$TEMP_PLIST"
mv "$TEMP_PLIST" "$PLIST_PATH"
trap - EXIT

bootstrap_attempt=0
while ! launchctl bootstrap "$DOMAIN" "$PLIST_PATH"; do
  if launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then
    break
  fi
  bootstrap_attempt=$((bootstrap_attempt + 1))
  if [ "$bootstrap_attempt" -ge 5 ]; then
    echo "Could not load $LABEL after 5 attempts" >&2
    exit 1
  fi
  sleep 1
done
launchctl enable "$DOMAIN/$LABEL"
launchctl kickstart -k "$DOMAIN/$LABEL"

attempt=0
while [ "$attempt" -lt 15 ]; do
  if launchctl print "$DOMAIN/$LABEL" 2>/dev/null |
    grep -q "state = running"; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 1
done

if [ "$attempt" -eq 15 ]; then
  echo "$LABEL was loaded but did not remain running" >&2
  echo "Inspect $LOG_DIR/menu-stderr.log" >&2
  exit 1
fi

echo "Installed $LABEL"
echo "App:    $APP_PATH"
echo "Status: $PROJECT_ROOT/scripts/menu-status.sh"
echo "Logs:   $LOG_DIR/menu-stdout.log and $LOG_DIR/menu-stderr.log"
