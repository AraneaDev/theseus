#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin/sample-cli ;; system) dest=/usr/local/bin/sample-cli ;; *) exit 2 ;; esac
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$(dirname -- "$dest")"
cp "$here/sample-cli" "$dest"
chmod 755 "$dest"
n=0
if test -f /workspace/install-count; then n=$(cat /workspace/install-count); fi
n=$((n + 1)); printf '%s\n' "$n" > /workspace/install-count
echo "installation=$n"

# Public fault input is set only on the second invocation by the test recipe.
settings=/home/theseus/.config/sample-cli/settings
case "${THESEUS_FIXTURE_FAULT:-}" in
reset_settings) printf 'colour=default\n' > "$settings" ;;
duplicate_settings) printf 'colour=blue\n' >> "$settings" ;;
change_mode) chmod 600 "$settings" ;;
add_child) printf 'unexpected\n' > /home/theseus/.config/sample-cli/unexpected ;;
remove_child) rm "$settings" ;;
reject_existing) test ! -f "$settings" || exit 9 ;;
timeout_child|cancellation) sleep 600 & echo "$!" > /workspace/child.pid; wait ;;
esac
