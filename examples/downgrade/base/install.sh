#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
printf 'downgrade uid=%s gid=%s\n' "$(id -u)" "$(id -g)"
case "$2" in
compatible)
    cp /inputs/base/sample-cli "$dest/sample-cli"
    chmod 755 "$dest/sample-cli"
    ;;
rejection)
    # Inspect the real newer data before making any installation write.
    data=/home/theseus/.local/share/sample-cli/records
    test "$(head -n 1 "$data")" = format=2
    test "$(wc -l < "$data")" -eq 3
    echo 'data format is newer than supported'
    exit 9
    ;;
*) exit 2 ;;
esac
