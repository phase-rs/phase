#!/usr/bin/env bash
# Fake ast-grep: 6 matches, version 0.45.0 — the "tool moved AND the number moved" arm.
case "$1" in
  --version) echo "ast-grep 0.45.0" ;;
  run) for i in 1 2 3 4 5 6; do echo "{\"text\":\"HIT-$i\"}"; done ;;
  *) exit 64 ;;
esac
