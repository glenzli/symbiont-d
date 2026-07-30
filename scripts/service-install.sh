#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-d"
DOMAIN="gui/$(id -u)"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
TEMPLATE="$PROJECT_ROOT/packaging/launchd/$LABEL.plist.in"
PLIST_DIR="$HOME/Library/LaunchAgents"
PLIST_PATH="$PLIST_DIR/$LABEL.plist"
LOG_DIR="$PROJECT_ROOT/data/logs"
BINARY="$PROJECT_ROOT/target/release/symbiont-d"

CODEX_BIN="${CODEX_BIN:-$(command -v codex || true)}"
CARGO_BIN="${CARGO_BIN:-$(command -v cargo || true)}"

if [ -z "$CODEX_BIN" ]; then
  echo "codex was not found; set CODEX_BIN to its absolute path" >&2
  exit 1
fi

if [ -z "$CARGO_BIN" ]; then
  echo "cargo was not found; set CARGO_BIN to its absolute path" >&2
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

echo "Building symbiont-d..."
"$CARGO_BIN" build --release --manifest-path "$PROJECT_ROOT/Cargo.toml"

mkdir -p "$PLIST_DIR" "$LOG_DIR"
TEMP_PLIST="$(mktemp "$PLIST_DIR/$LABEL.XXXXXX")"
trap 'rm -f "$TEMP_PLIST"' EXIT

sed \
  -e "s|@BINARY@|$(sed_replacement "$BINARY")|g" \
  -e "s|@PROJECT_ROOT@|$(sed_replacement "$PROJECT_ROOT")|g" \
  -e "s|@CODEX_BIN@|$(sed_replacement "$CODEX_BIN")|g" \
  -e "s|@HOME@|$(sed_replacement "$HOME")|g" \
  -e "s|@PATH@|$(sed_replacement "${PATH:-/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin}")|g" \
  -e "s|@LOG_DIR@|$(sed_replacement "$LOG_DIR")|g" \
  "$TEMPLATE" >"$TEMP_PLIST"

plutil -lint "$TEMP_PLIST" >/dev/null
chmod 0644 "$TEMP_PLIST"
mv "$TEMP_PLIST" "$PLIST_PATH"
trap - EXIT

launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
launchctl bootstrap "$DOMAIN" "$PLIST_PATH"
launchctl enable "$DOMAIN/$LABEL"
launchctl kickstart -k "$DOMAIN/$LABEL"

attempt=0
while [ "$attempt" -lt 30 ]; do
  if curl --fail --silent --max-time 1 \
    http://127.0.0.1:4317/api/health >/dev/null 2>&1; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 1
done

if [ "$attempt" -eq 30 ]; then
  echo "$LABEL was loaded but did not become healthy within 30 seconds" >&2
  echo "Inspect $LOG_DIR/stdout.log and $LOG_DIR/stderr.log" >&2
  exit 1
fi

echo "Installed $LABEL"
echo "UI:     http://127.0.0.1:4317/"
echo "Status: $PROJECT_ROOT/scripts/service-status.sh"
echo "Logs:   $LOG_DIR"
