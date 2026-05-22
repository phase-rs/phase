#!/bin/sh
set -eu

: "${PHASE_DATA_DIR:=/var/lib/phase-server}"
export PHASE_DATA_DIR

mkdir -p "$PHASE_DATA_DIR"
chown phase:phase "$PHASE_DATA_DIR"
cp /usr/share/phase-server/card-data.json "$PHASE_DATA_DIR/card-data.json"
cp /usr/share/phase-server/draft-pools.json "$PHASE_DATA_DIR/draft-pools.json"
chown phase:phase "$PHASE_DATA_DIR/card-data.json" "$PHASE_DATA_DIR/draft-pools.json"

if [ $# -eq 0 ]; then
    set -- phase-server
elif [ "${1#-}" != "$1" ]; then
    set -- phase-server "$@"
fi

exec gosu phase "$@"
