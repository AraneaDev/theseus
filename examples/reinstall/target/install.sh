#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin/sample-cli ;; system) dest=/usr/local/bin/sample-cli ;; *) exit 2 ;; esac
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$(dirname -- "$dest")"
cp "$here/sample-cli" "$dest"
chmod 755 "$dest"
