#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
data=/home/theseus/.local/share/sample-cli
fault=${THESEUS_FIXTURE_FAULT:-}
case "$fault" in timeout-rollback|cancel-rollback) sleep 600 ;; noop-rollback) exit 0 ;; esac
cp /inputs/base/sample-cli "$dest/sample-cli"
chmod 755 "$dest/sample-cli"
if test "$2" = restore-data; then
    test -f /workspace/rollback-backup/saved/records
    rm -rf "$data"
    cp -a /workspace/rollback-backup/saved "$data"
fi
case "$fault" in
    missing-child) rm "$data/nested/child" ;;
    added-child) printf extra > "$data/extra" ;;
    wrong-mode) chmod 600 "$data/records" ;;
    wrong-owner) chown 0:0 "$data/records" ;;
    wrong-link) rm "$data/current"; ln -s nested/child "$data/current" ;;
    wrong-reader) touch /workspace/wrong-reader ;;
    wrong-runtime) touch /workspace/wrong-runtime ;;
    restore-predicate) test -f "$data/missing" ;;
esac
test -f "$data/records"
echo 'rollback completed'
