#!/bin/sh
set -eu
data=/home/theseus/.local/share/sample-cli
backup=/workspace/rollback-backup
mkdir -p "$backup"
cp -a "$data" "$backup/saved"
test -f "$backup/saved/records"
echo 'backup completed'
