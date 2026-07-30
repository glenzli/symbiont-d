#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-d"
DOMAIN="gui/$(id -u)"

if ! DETAILS="$(launchctl print "$DOMAIN/$LABEL" 2>/dev/null)"; then
  echo "$LABEL is not installed or loaded"
  exit 1
fi

printf '%s\n' "$DETAILS" |
  awk '
    /^[[:space:]]*state = / && !state {
      sub(/^[[:space:]]*state = /, "state: ")
      print
      state = 1
    }
    /^[[:space:]]*pid = / && !pid {
      sub(/^[[:space:]]*pid = /, "pid: ")
      print
      pid = 1
    }
    /^[[:space:]]*last exit code = / && !last_exit {
      sub(/^[[:space:]]*last exit code = /, "last exit: ")
      print
      last_exit = 1
    }
  '

if curl --fail --silent --show-error --max-time 2 \
  http://127.0.0.1:4317/api/health >/dev/null; then
  echo "health: ok"
else
  echo "health: unavailable"
  exit 1
fi
