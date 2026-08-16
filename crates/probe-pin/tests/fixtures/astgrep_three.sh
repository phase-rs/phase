#!/usr/bin/env bash
# Fake ast-grep: 3 matches, version 0.44.1. Makes every projection row testable with no install.
case "$1" in
  --version) echo "ast-grep 0.44.1" ;;
  run) for i in 1 2 3; do echo "{\"text\":\"HIT-$i\"}"; done ;;
  *) exit 64 ;;
esac
