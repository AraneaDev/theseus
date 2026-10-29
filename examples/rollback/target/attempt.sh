#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
data=/home/theseus/.local/share/sample-cli
cp /inputs/target/sample-cli "$dest/sample-cli"
chmod 755 "$dest/sample-cli"
printf 'format=2\nalpha=11\nbeta=29\n' > "$data/records"
printf 'candidate child\n' > "$data/added"
if test "$(id -u)" = 0; then chown 10001:10001 "$data/added"; fi
echo 'upgrade failed after migration'
exit 9
