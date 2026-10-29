#!/bin/sh
set -eu
case "$1" in user) dest=/home/theseus/.local/bin ;; system) dest=/usr/local/bin ;; *) exit 2 ;; esac
fault=${THESEUS_FIXTURE_FAULT:-}
case "$2" in
interrupt)
    case "$fault" in
        early-0) exit 0 ;;
        early-9) exit 9 ;;
        never-ready|cancel-before-ready) sleep 600 ;;
        stale) printf 'stale-nonce\n' > "${THESEUS_INTERRUPT_BARRIER%/*}/ready/receipt"; sleep 600 ;;
        symlink) ln -s /etc/passwd "${THESEUS_INTERRUPT_BARRIER%/*}/ready/receipt"; sleep 600 ;;
    esac
    mkdir -p "$dest"
    cp /inputs/target/sample-cli "$dest/sample-cli.partial"
    chmod 640 "$dest/sample-cli.partial"
    (while :; do printf x >> /workspace/writer; sleep 0.02; done) &
    while ! test -s /workspace/writer; do sleep 0.01; done
    /bin/sh "$THESEUS_INTERRUPT_BARRIER"
    # The synchronous helper must never return.
    printf forbidden > /workspace/continued
    exit 41
    ;;
recover)
    printf invoked > /workspace/recovery-invoked
    test "$fault" != recovery-9 || exit 9
    test -f "$dest/sample-cli.partial"
    cmp /inputs/target/sample-cli "$dest/sample-cli.partial"
    test ! -e /workspace/continued
    if test "$fault" = leftover-partial; then
        cp "$dest/sample-cli.partial" "$dest/sample-cli"
    else
        mv "$dest/sample-cli.partial" "$dest/sample-cli"
    fi
    chmod 755 "$dest/sample-cli"
    if test "$fault" = corrupt-data; then
        printf damaged > /home/theseus/.local/share/sample-cli/notes
    fi
    ;;
*) exit 2 ;;
esac
