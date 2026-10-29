#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
fault=${THESEUS_FIXTURE_FAULT:-}
data=/home/theseus/.local/share/sample-cli
settings=/home/theseus/.config/sample-cli/settings
# Every fault starts after the healthy newer configure/exercise and reference.
printf 'downgrade uid=%s gid=%s\n' "$(id -u)" "$(id -g)"
case "$fault" in timeout|cancel) sleep 600 ;; esac
if test "$2" = compatible; then
    cp /inputs/base/sample-cli "$dest/sample-cli"
    chmod 755 "$dest/sample-cli"
    case "$fault" in
        broken-reader) touch /workspace/broken-reader ;;
        silent-reader-drop) touch /workspace/silent-reader-drop ;;
        silent-drop) printf 'format=2\nalpha=11\n' > "$data/records" ;;
        reset-config) printf 'colour=red\n' > "$settings" ;;
    esac
    exit 0
fi
test "$2" = rejection
# The refusal is derived from actual newer data, before any normal install write.
test "$(head -n 1 "$data/records")" = format=2
test "$(wc -l < "$data/records")" -eq 3
case "$fault" in
    binary-change) cp /inputs/base/sample-cli "$dest/sample-cli"; chmod 755 "$dest/sample-cli" ;;
    mode-change) chmod 700 "$dest/sample-cli" ;;
    link-change) rm "$dest/sample-cli"; ln -s /inputs/target/sample-cli "$dest/sample-cli" ;;
    add-child) printf 'unexpected\n' > "$data/extra" ;;
    remove-child) rm "$data/records" ;;
    reset-config) printf 'colour=red\n' > "$settings" ;;
    exercise-fails) touch /workspace/fail-exercise ;;
    wrong-marker) echo 'unrelated installer failure'; exit 9 ;;
    missing-marker) exit 9 ;;
esac
echo 'data format is newer than supported'
case "$fault" in
    wrong-status) exit 8 ;;
    unexpected-zero) exit 0 ;;
    truncated) dd if=/dev/zero bs=8192 count=1 2>/dev/null ;;
esac
exit 9
