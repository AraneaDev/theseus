#!/bin/sh
set -eu
profile=$1
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "$profile" in user) dest=/home/theseus/.local/bin/sample-cli ;; system) dest=/usr/local/bin/sample-cli ;; *) exit 2 ;; esac
mkdir -p "$(dirname -- "$dest")"
cp "$here/sample-cli" "$dest"
chmod 755 "$dest"
fault=${THESEUS_FIXTURE_FAULT:-}
case "$fault" in
install) exit 9 ;;
permissions) chmod 700 "$dest" ;;
stale_path) mkdir -p /workspace/stale; printf '#!/bin/sh\necho sample-cli 0.0.0\n' > /workspace/stale/sample-cli; chmod 755 /workspace/stale/sample-cli ;;
config_loss) if test -e /home/theseus/.config/sample-cli/settings; then printf 'damaged\n' > /home/theseus/.config/sample-cli/settings; fi ;;
baseline_deletion) rm -rf /home/theseus/.cache ;;
timeout_child|cancellation) sleep 600 & echo "$!" > /workspace/child.pid; wait ;;
log_overflow) head -c 20000 /dev/zero; head -c 20000 /dev/zero >&2 ;;
esac
