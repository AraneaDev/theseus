#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
rm -f "$dest/sample-cli"
test ! -e "$dest/sample-cli"
echo 'uninstalled'
