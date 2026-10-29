#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
data=/home/theseus/.local/share/sample-cli
fault=${THESEUS_FIXTURE_FAULT:-}
case "$fault" in timeout-attempt|cancel-attempt) sleep 600 ;; esac
if test "$fault" != no-mutation && test "$fault" != no-mutation-after-backup-check; then
    cp /inputs/target/sample-cli "$dest/sample-cli"
    chmod 755 "$dest/sample-cli"
    printf 'format=2\nalpha=11\nbeta=29\n' > "$data/records"
    printf 'candidate child\n' > "$data/added"
    if test "$(id -u)" = 0; then chown 10001:10001 "$data/added"; fi
fi
case "$fault" in
    corrupt-backup) printf 'corrupt\n' > /workspace/rollback-backup/saved/records ;;
    wrong-marker) echo 'unrelated failure'; exit 9 ;;
    wrong-status) echo 'upgrade failed after migration'; exit 8 ;;
    unexpected-zero) echo 'upgrade failed after migration'; exit 0 ;;
esac
echo 'upgrade failed after migration'
case "$fault" in truncated) dd if=/dev/zero bs=8192 count=1 2>/dev/null ;; esac
exit 9
