#!/bin/sh
set -eu
data=/home/theseus/.local/share/sample-cli
backup=/workspace/rollback-backup
fault=${THESEUS_FIXTURE_FAULT:-}
case "$fault" in timeout-backup|cancel-backup) sleep 600 ;; esac
mkdir -p "$backup"
cp -a "$data" "$backup/saved"
test -f "$backup/saved/records"
case "$fault" in
    destructive-backup) rm "$data/nested/child" ;;
    partial-backup) rm "$backup/saved/nested/child"; exit 7 ;;
    bad-backup-copy) rm "$backup/saved/nested/child" ;;
    backup-predicate) test -f "$backup/missing" ;;
esac
echo 'backup completed'
