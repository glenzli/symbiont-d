#!/bin/sh
set -eu

BASE_URL="${SYMBIONT_URL:-http://127.0.0.1:4317}"

if [ "$#" -eq 0 ] || [ -z "$1" ]; then
  echo "usage: expand.sh <topic-id> [message-limit]" >&2
  exit 2
fi

TOPIC_ID="$1"
LIMIT="${2:-80}"

exec curl --fail --silent --show-error --max-time 15 \
  --get \
  --data-urlencode "topicId=$TOPIC_ID" \
  --data-urlencode "limit=$LIMIT" \
  "$BASE_URL/api/bridge/expand"
