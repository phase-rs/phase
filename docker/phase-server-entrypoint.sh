#!/bin/sh
set -eu

: "${PHASE_DATA_DIR:=/var/lib/phase-server}"
export PHASE_DATA_DIR

mkdir -p "$PHASE_DATA_DIR"
chown phase:phase "$PHASE_DATA_DIR"

if [ $# -eq 0 ]; then
    set -- phase-server
elif [ "${1#-}" != "$1" ]; then
    set -- phase-server "$@"
fi

exec gosu phase "$@"
