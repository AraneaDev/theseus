#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
data=/home/theseus/.local/share/sample-cli
cp /inputs/base/sample-cli "$dest/sample-cli"
chmod 755 "$dest/sample-cli"
case "$2" in
    restore-data)
        test -f /workspace/rollback-backup/saved/records
        rm -rf "$data"
        cp -a /workspace/rollback-backup/saved "$data" ;;
    binary-only) ;;
    *) exit 2 ;;
esac
test -f "$data/records"
echo 'rollback completed'
