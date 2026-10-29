#!/bin/sh
set -eu
case "${THESEUS_FIXTURE_FAULT:-}" in
    timeout-cleanup|cancel-cleanup) sleep 600 ;;
    cleanup-fails) exit 7 ;;
esac
rm -rf /workspace/rollback-backup
test ! -e /workspace/rollback-backup
echo 'backup removed'
