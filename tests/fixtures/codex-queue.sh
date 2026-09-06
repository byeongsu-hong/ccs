#!/bin/sh
# A queue client whose outcome and captured arguments belong to its test home.
if [ -e "$CODEX_HOME/reject" ]; then
    exit 1
fi
if [ "$2" = --help ]; then
    printf '%s\n' '--thread --message'
    exit 0
fi
printf '%s\n' "$@" > "$CODEX_HOME/queued"
