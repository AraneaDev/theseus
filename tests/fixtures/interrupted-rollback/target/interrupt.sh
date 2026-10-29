#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
data=/home/theseus/.local/share/sample-cli
fault=${THESEUS_FIXTURE_FAULT:-}
case "$fault" in early-0) exit 0 ;; early-9) exit 9 ;; never-ready|timeout-interrupt|cancel-interrupt) sleep 600 ;; esac
if test "$fault" != no-mutation && test "$fault" != no-mutation-after-backup-check; then
    cp /inputs/target/sample-cli "$dest/sample-cli"
    chmod 755 "$dest/sample-cli"
    sed 's/format=1/format=2/' "$data/records" > "$data/records.next"
    chmod 640 "$data/records.next"
    if test "$(id -u)" = 0; then chown 10001:10001 "$data/records.next"; fi
    mv "$data/records.next" "$data/records"
    printf 'candidate child\n' > "$data/added"
    if test "$(id -u)" = 0; then chown 10001:10001 "$data/added"; fi
fi
case "$fault" in
    corrupt-backup) printf 'corrupt\n' > /workspace/rollback-backup/saved/records ;;
    stale) printf 'stale\n' > "$(dirname "$THESEUS_INTERRUPT_BARRIER")/ready/receipt"; sleep 600 ;;
    symlink) ln -s /etc/passwd "$(dirname "$THESEUS_INTERRUPT_BARRIER")/ready/receipt"; sleep 600 ;;
esac
printf x > /workspace/writer
(while :; do printf x >> /workspace/writer; sleep 0.01; done) &
/bin/sh "$THESEUS_INTERRUPT_BARRIER"
printf 'forbidden continuation\n' > /workspace/continued
exit 41
