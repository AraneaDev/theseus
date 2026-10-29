#!/bin/sh
set -eu
rm -rf /workspace/rollback-backup
test ! -e /workspace/rollback-backup
echo 'backup removed'
