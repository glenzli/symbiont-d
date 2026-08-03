#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-d"
PCP_LABEL="$LABEL.pcp"
PCP_CONSOLE_LABEL="$LABEL.pcp-console"
DOMAIN="gui/$(id -u)"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
PCP_PROJECT_ROOT="${PCP_PROJECT_ROOT:-$PROJECT_ROOT/../paged-context-protocol}"
TEMPLATE="$PROJECT_ROOT/packaging/launchd/$LABEL.plist.in"
PCP_TEMPLATE="$PROJECT_ROOT/packaging/launchd/$PCP_LABEL.plist.in"
PCP_CONSOLE_TEMPLATE="$PROJECT_ROOT/packaging/launchd/$PCP_CONSOLE_LABEL.plist.in"
PLIST_DIR="$HOME/Library/LaunchAgents"
PLIST_PATH="$PLIST_DIR/$LABEL.plist"
PCP_PLIST_PATH="$PLIST_DIR/$PCP_LABEL.plist"
PCP_CONSOLE_PLIST_PATH="$PLIST_DIR/$PCP_CONSOLE_LABEL.plist"
LOG_DIR="$PROJECT_ROOT/data/logs"
BINARY="$PROJECT_ROOT/target/release/symbiont-d"
PCP_BINARY="$PCP_PROJECT_ROOT/target/release/pcp-runtime"
PCP_CLI="$PCP_PROJECT_ROOT/target/release/pcp"
PCP_CONSOLE_BINARY="$PCP_PROJECT_ROOT/target/release/pcp-console"
PCP_CONFIG="$PROJECT_ROOT/packaging/pcp-runtime.toml"
PCP_STORE="$PROJECT_ROOT/data/context.sqlite3"
PCP_SOCKET="$PROJECT_ROOT/data/run/pcp-symbiont.sock"
PCP_OPERATOR_SOCKET="$PROJECT_ROOT/data/run/pcp-operator.sock"
PCP_MIGRATION_BACKUP="$PROJECT_ROOT/data/context.pre-runtime.sqlite3"
PCP_MANIFEST="$PCP_PROJECT_ROOT/Cargo.toml"

CODEX_BIN="${CODEX_BIN:-$(command -v codex || true)}"
CARGO_BIN="${CARGO_BIN:-$(command -v cargo || true)}"
SQLITE_BIN="${SQLITE_BIN:-$(command -v sqlite3 || true)}"

if [ -z "$CODEX_BIN" ]; then
  echo "codex was not found; set CODEX_BIN to its absolute path" >&2
  exit 1
fi

if [ -z "$CARGO_BIN" ]; then
  echo "cargo was not found; set CARGO_BIN to its absolute path" >&2
  exit 1
fi

if [ ! -f "$PCP_MANIFEST" ]; then
  echo "paged-context-protocol was not found at $PCP_PROJECT_ROOT" >&2
  echo "Set PCP_PROJECT_ROOT to the repository path." >&2
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

unload_label() {
  unload_label_name="$1"
  launchctl bootout "$DOMAIN/$unload_label_name" >/dev/null 2>&1 || true
  unload_attempt=0
  while launchctl print "$DOMAIN/$unload_label_name" >/dev/null 2>&1; do
    unload_attempt=$((unload_attempt + 1))
    if [ "$unload_attempt" -ge 10 ]; then
      echo "Could not unload $unload_label_name after 10 seconds" >&2
      exit 1
    fi
    sleep 1
  done
}

bootstrap_label() {
  bootstrap_label_name="$1"
  bootstrap_plist_path="$2"
  bootstrap_attempt=0
  while ! launchctl bootstrap "$DOMAIN" "$bootstrap_plist_path"; do
    if launchctl print "$DOMAIN/$bootstrap_label_name" >/dev/null 2>&1; then
      break
    fi
    bootstrap_attempt=$((bootstrap_attempt + 1))
    if [ "$bootstrap_attempt" -ge 5 ]; then
      echo "Could not load $bootstrap_label_name after 5 attempts" >&2
      exit 1
    fi
    sleep 1
  done
  launchctl enable "$DOMAIN/$bootstrap_label_name"
  launchctl kickstart -k "$DOMAIN/$bootstrap_label_name"
}

echo "Building symbiont-d..."
"$CARGO_BIN" build --release --manifest-path "$PROJECT_ROOT/Cargo.toml"
echo "Building PCP runtime and diagnostic client..."
"$CARGO_BIN" build --release --manifest-path "$PCP_MANIFEST" \
  -p pcp-runtime -p pcp-cli -p pcp-console

mkdir -p "$PLIST_DIR" "$LOG_DIR" "$(dirname -- "$PCP_SOCKET")"
TEMP_PLIST="$(mktemp "$PLIST_DIR/$LABEL.XXXXXX")"
TEMP_PCP_PLIST="$(mktemp "$PLIST_DIR/$PCP_LABEL.XXXXXX")"
TEMP_PCP_CONSOLE_PLIST="$(mktemp "$PLIST_DIR/$PCP_CONSOLE_LABEL.XXXXXX")"
trap 'rm -f "$TEMP_PLIST" "$TEMP_PCP_PLIST" "$TEMP_PCP_CONSOLE_PLIST"' EXIT

sed \
  -e "s|@BINARY@|$(sed_replacement "$BINARY")|g" \
  -e "s|@PROJECT_ROOT@|$(sed_replacement "$PROJECT_ROOT")|g" \
  -e "s|@CODEX_BIN@|$(sed_replacement "$CODEX_BIN")|g" \
  -e "s|@HOME@|$(sed_replacement "$HOME")|g" \
  -e "s|@PATH@|$(sed_replacement "${PATH:-/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin}")|g" \
  -e "s|@LOG_DIR@|$(sed_replacement "$LOG_DIR")|g" \
  -e "s|@PCP_SOCKET@|$(sed_replacement "$PCP_SOCKET")|g" \
  "$TEMPLATE" >"$TEMP_PLIST"

sed \
  -e "s|@PCP_BINARY@|$(sed_replacement "$PCP_BINARY")|g" \
  -e "s|@PCP_CONFIG@|$(sed_replacement "$PCP_CONFIG")|g" \
  -e "s|@PCP_PROJECT_ROOT@|$(sed_replacement "$PCP_PROJECT_ROOT")|g" \
  -e "s|@HOME@|$(sed_replacement "$HOME")|g" \
  -e "s|@PATH@|$(sed_replacement "${PATH:-/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin}")|g" \
  -e "s|@LOG_DIR@|$(sed_replacement "$LOG_DIR")|g" \
  "$PCP_TEMPLATE" >"$TEMP_PCP_PLIST"

sed \
  -e "s|@PCP_CONSOLE_BINARY@|$(sed_replacement "$PCP_CONSOLE_BINARY")|g" \
  -e "s|@PCP_OPERATOR_SOCKET@|$(sed_replacement "$PCP_OPERATOR_SOCKET")|g" \
  -e "s|@PCP_PROJECT_ROOT@|$(sed_replacement "$PCP_PROJECT_ROOT")|g" \
  -e "s|@HOME@|$(sed_replacement "$HOME")|g" \
  -e "s|@PATH@|$(sed_replacement "${PATH:-/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin}")|g" \
  -e "s|@LOG_DIR@|$(sed_replacement "$LOG_DIR")|g" \
  "$PCP_CONSOLE_TEMPLATE" >"$TEMP_PCP_CONSOLE_PLIST"

plutil -lint "$TEMP_PLIST" >/dev/null
plutil -lint "$TEMP_PCP_PLIST" >/dev/null
plutil -lint "$TEMP_PCP_CONSOLE_PLIST" >/dev/null
chmod 0644 "$TEMP_PLIST"
chmod 0644 "$TEMP_PCP_PLIST"
chmod 0644 "$TEMP_PCP_CONSOLE_PLIST"

unload_label "$LABEL"
unload_label "$PCP_CONSOLE_LABEL"
unload_label "$PCP_LABEL"

if [ -f "$PCP_STORE" ] && [ ! -f "$PCP_MIGRATION_BACKUP" ]; then
  if [ -z "$SQLITE_BIN" ]; then
    echo "sqlite3 is required for the one-time PCP migration backup" >&2
    exit 1
  fi
  echo "Creating one-time pre-runtime PCP backup..."
  "$SQLITE_BIN" "$PCP_STORE" ".backup '$PCP_MIGRATION_BACKUP'"
  if [ "$("$SQLITE_BIN" "$PCP_MIGRATION_BACKUP" 'PRAGMA quick_check;')" != "ok" ]; then
    rm -f "$PCP_MIGRATION_BACKUP"
    echo "The PCP migration backup failed its integrity check" >&2
    exit 1
  fi
fi

mv "$TEMP_PLIST" "$PLIST_PATH"
mv "$TEMP_PCP_PLIST" "$PCP_PLIST_PATH"
mv "$TEMP_PCP_CONSOLE_PLIST" "$PCP_CONSOLE_PLIST_PATH"
trap - EXIT

bootstrap_label "$PCP_LABEL" "$PCP_PLIST_PATH"

attempt=0
while [ "$attempt" -lt 90 ]; do
  if PCP_RUNTIME_SOCKET="$PCP_SOCKET" PCP_CLIENT_ID="host:symbiont-d" \
    "$PCP_CLI" doctor >/dev/null 2>&1; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 1
done

if [ "$attempt" -eq 90 ]; then
  echo "$PCP_LABEL was loaded but did not become healthy within 90 seconds" >&2
  echo "Inspect $LOG_DIR/pcp-runtime-stderr.log" >&2
  exit 1
fi

attempt=0
while [ "$attempt" -lt 90 ]; do
  if PCP_RUNTIME_SOCKET="$PCP_OPERATOR_SOCKET" PCP_CLIENT_ID="operator:local" \
    "$PCP_CLI" doctor >/dev/null 2>&1; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 1
done

if [ "$attempt" -eq 90 ]; then
  echo "$PCP_LABEL did not expose a healthy operator endpoint within 90 seconds" >&2
  echo "Inspect $LOG_DIR/pcp-runtime-stderr.log" >&2
  exit 1
fi

bootstrap_label "$PCP_CONSOLE_LABEL" "$PCP_CONSOLE_PLIST_PATH"

attempt=0
while [ "$attempt" -lt 90 ]; do
  if curl --fail --silent --max-time 1 \
    http://127.0.0.1:4318/api/health >/dev/null 2>&1; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 1
done

if [ "$attempt" -eq 90 ]; then
  echo "$PCP_CONSOLE_LABEL was loaded but did not become healthy within 90 seconds" >&2
  echo "Inspect $LOG_DIR/pcp-console-stderr.log" >&2
  exit 1
fi

bootstrap_label "$LABEL" "$PLIST_PATH"

attempt=0
while [ "$attempt" -lt 90 ]; do
  if curl --fail --silent --max-time 1 \
    http://127.0.0.1:4317/api/health >/dev/null 2>&1; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 1
done

if [ "$attempt" -eq 90 ]; then
  echo "$LABEL was loaded but did not become healthy within 90 seconds" >&2
  echo "Inspect $LOG_DIR/stdout.log and $LOG_DIR/stderr.log" >&2
  exit 1
fi

echo "Installed $LABEL"
echo "PCP:    $PCP_SOCKET"
echo "Console: http://127.0.0.1:4318/"
echo "UI:     http://127.0.0.1:4317/"
echo "Status: $PROJECT_ROOT/scripts/service-status.sh"
echo "Logs:   $LOG_DIR"
