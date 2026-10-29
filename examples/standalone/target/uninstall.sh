#!/bin/sh
set -eu
case "$1" in user) rm -f /home/theseus/.local/bin/sample-cli ;; system) rm -f /usr/local/bin/sample-cli ;; *) exit 2 ;; esac
rm -rf /workspace/stale
case "${THESEUS_FIXTURE_FAULT:-}" in
leftovers) echo unexpected > /home/theseus/unwanted ;;
lost_data) rm -f /home/theseus/.local/share/sample-cli/notes ;;
uninstall) exit 8 ;;
esac
