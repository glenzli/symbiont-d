#!/bin/sh
set -eu

LABEL="com.glenzli.symbiont-menu"
DOMAIN="gui/$(id -u)"
APP_DIR="${SYMBIONT_APP_DIR:-$HOME/Applications}"
APP_PATH="$APP_DIR/SymbiontMenu.app"

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

if [ -x "$APP_PATH/Contents/MacOS/SymbiontMenu" ]; then
  echo "app: $APP_PATH"
else
  echo "app: missing"
  exit 1
fi
