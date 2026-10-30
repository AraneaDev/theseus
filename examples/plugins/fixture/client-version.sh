#!/bin/sh
set -eu
export CLAUDE_CONFIG_DIR=/home/theseus/plugin-config
export DISABLE_AUTOUPDATER=1
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
export CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1
export LC_ALL=C
cd /workspace/plugin-project
# File capture keeps trailing newlines observable and bounds memory. Reject
# unexpected output; never sanitize an unknown version into a claimed one.
version_file=$CLAUDE_CONFIG_DIR/client-version.capture
/inputs/target/client/claude --version > "$version_file"
bytes=$(wc -c < "$version_file")
test "$bytes" -ge 1
test "$bytes" -le 129
# Check the original bytes before shell substitution (which can discard NUL).
# A final newline is the only permitted control byte.
invalid=$(tr -d 'A-Za-z0-9. ()_\n-' < "$version_file" | wc -c)
test "$invalid" -eq 0
lines=$(wc -l < "$version_file")
test "$lines" -le 1
version=$(cat "$version_file")
test "${#version}" -ge 1
test "${#version}" -le 128
case "$version" in *[!A-Za-z0-9.\ \(\)_-]*) exit 65 ;; esac
# One optional final newline is allowed; embedded/trailing extra lines are not.
if test "$lines" -eq 1; then test "$bytes" -eq $((${#version} + 1)); fi
printf '{"version":"%s"}\n' "$version"
