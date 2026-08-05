#!/bin/sh
set -eu

BASE_URL="${SYMBIONT_URL:-http://127.0.0.1:4317}"

if [ "$#" -eq 0 ] || [ -z "$1" ]; then
  exec curl --fail --silent --show-error --max-time 10 \
    "$BASE_URL/api/bridge/context"
fi

QUERY="$1"
PURPOSE="${2:-}"
DEPTH="${3:-normal}"
TOKEN_BUDGET="${SYMBIONT_RECALL_TOKEN_BUDGET:-6000}"

if [ -n "$PURPOSE" ]; then
  exec curl --fail --silent --show-error --max-time 15 \
    --get \
    --data-urlencode "query=$QUERY" \
    --data-urlencode "purpose=$PURPOSE" \
    --data-urlencode "depth=$DEPTH" \
    --data-urlencode "tokenBudget=$TOKEN_BUDGET" \
    "$BASE_URL/api/bridge/recall"
fi

exec curl --fail --silent --show-error --max-time 10 \
  --get \
  --data-urlencode "query=$QUERY" \
  --data-urlencode "depth=$DEPTH" \
  --data-urlencode "tokenBudget=$TOKEN_BUDGET" \
  "$BASE_URL/api/bridge/recall"
