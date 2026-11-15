#!/usr/bin/env bash
# Fake ast-grep that is present but cannot run — fail-closed, never a table-only block.
echo "ast-grep: unsupported language" >&2
exit 1
