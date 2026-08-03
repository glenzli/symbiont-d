#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-d"
PCP_LABEL="$LABEL.pcp"
PCP_CONSOLE_LABEL="$LABEL.pcp-console"
DOMAIN="gui/$(id -u)"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
PCP_PROJECT_ROOT="${PCP_PROJECT_ROOT:-$PROJECT_ROOT/../paged-context-protocol}"
PCP_CLI="$PCP_PROJECT_ROOT/target/release/pcp"
PCP_SOCKET="$PROJECT_ROOT/data/run/pcp-symbiont.sock"

print_service() {
  service_label="$1"
  service_name="$2"
  if ! service_details="$(launchctl print "$DOMAIN/$service_label" 2>/dev/null)"; then
    echo "$service_name: not installed or loaded"
    return 1
  fi

  echo "$service_name:"
  printf '%s\n' "$service_details" |
    awk '
    /^[[:space:]]*state = / && !state {
      sub(/^[[:space:]]*state = /, "  state: ")
      print
      state = 1
    }
    /^[[:space:]]*pid = / && !pid {
      sub(/^[[:space:]]*pid = /, "  pid: ")
      print
      pid = 1
    }
    /^[[:space:]]*last exit code = / && !last_exit {
      sub(/^[[:space:]]*last exit code = /, "  last exit: ")
      print
      last_exit = 1
    }
  '
}

status=0
print_service "$PCP_LABEL" "PCP runtime" || status=1
print_service "$PCP_CONSOLE_LABEL" "PCP Console" || status=1
print_service "$LABEL" "symbiont-d" || status=1

if [ -x "$PCP_CLI" ] && PCP_RUNTIME_SOCKET="$PCP_SOCKET" \
  PCP_CLIENT_ID="host:symbiont-d" "$PCP_CLI" doctor >/dev/null 2>&1; then
  echo "PCP health: ok"
else
  echo "PCP health: unavailable"
  status=1
fi

if curl --fail --silent --show-error --max-time 2 \
  http://127.0.0.1:4318/api/health >/dev/null; then
  echo "PCP Console health: ok"
else
  echo "PCP Console health: unavailable"
  status=1
fi

if curl --fail --silent --show-error --max-time 2 \
  http://127.0.0.1:4317/api/health >/dev/null; then
  echo "symbiont health: ok"
else
  echo "symbiont health: unavailable"
  status=1
fi

exit "$status"
