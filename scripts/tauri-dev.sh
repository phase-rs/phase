#!/bin/bash
set -e

TARGET=$(rustc -vV | sed -n 's/host: //p')
DEST="client/src-tauri/binaries/phase-server-${TARGET}"

if [ ! -f "$DEST" ]; then
  echo "Building phase-server sidecar for ${TARGET}..."
  cargo build -p phase-server
  mkdir -p client/src-tauri/binaries
  cp target/debug/phase-server "$DEST"
  echo "Sidecar ready at ${DEST}"
fi

cd client && pnpm tauri:dev
