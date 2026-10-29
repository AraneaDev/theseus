#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
mkdir -p "$dest"
cp /inputs/base/sample-cli "$dest/sample-cli"
chmod 755 "$dest/sample-cli"
