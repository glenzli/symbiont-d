#!/bin/sh
set -eu

BASE_URL="${SYMBIONT_URL:-http://127.0.0.1:4317}"

if [ "$#" -eq 0 ] || [ -z "$1" ]; then
  exec curl --fail --silent --show-error --max-time 10 \
    "$BASE_URL/api/bridge/context"
fi

exec curl --fail --silent --show-error --max-time 10 \
  --get \
  --data-urlencode "query=$1" \
  "$BASE_URL/api/bridge/context"
