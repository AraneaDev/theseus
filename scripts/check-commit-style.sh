#!/usr/bin/env bash
set -euo pipefail

subject=${1-}
pattern='^(feat|fix|perf|refactor|docs|test|ci|chore|style)(\([^)]*\))?!?:[[:space:]]+[^[:space:]].*$'

if [[ ! "$subject" =~ $pattern ]]; then
  echo "Commit subject must use Conventional Commits: type(scope): summary" >&2
  exit 1
fi
