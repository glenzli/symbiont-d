#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
SOURCE="$PROJECT_ROOT/integrations/codex-skill/symbiont"
SKILLS_ROOT="${CODEX_SKILLS_ROOT:-$HOME/.agents/skills}"
TARGET="$SKILLS_ROOT/symbiont"

mkdir -p "$SKILLS_ROOT"

if [ -L "$TARGET" ] && [ "$(readlink "$TARGET")" = "$SOURCE" ]; then
  echo "The symbiont skill is already installed at $TARGET"
  exit 0
fi

if [ -e "$TARGET" ] || [ -L "$TARGET" ]; then
  echo "$TARGET already exists; remove or relocate it before installing" >&2
  exit 1
fi

ln -s "$SOURCE" "$TARGET"
echo "Installed the symbiont skill at $TARGET"
echo "Invoke it in Codex with: \$symbiont <request>"
