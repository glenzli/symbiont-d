#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-d"
DOMAIN="gui/$(id -u)"

if ! service_details="$(launchctl print "$DOMAIN/$LABEL" 2>/dev/null)"; then
  echo "symbiont-d: not installed or loaded"
  exit 1
fi

echo "symbiont-d:"
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

if curl --fail --silent --show-error --max-time 2 \
  http://127.0.0.1:4317/api/health >/dev/null; then
  echo "symbiont health: ok"
else
  echo "symbiont health: unavailable"
  exit 1
fi
