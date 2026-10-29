#!/bin/sh
set -eu
case "$1" in
user) rm -f /home/theseus/.local/bin/sample-cli ;;
system) rm -f /usr/local/bin/sample-cli ;;
*) exit 2 ;;
esac
# User configuration and data are retained by this explicit uninstall contract.
