#!/usr/bin/env python3
"""Mutation tests for the tilt pin/verify gate (`check_tilt_pin`).

Each mutation models a way the digest defense could be silently weakened. The
gate must reject every one of them, and must accept the real committed
`.cursor/install.sh`. A gate that a decoy `sha256sum -c` or a multiline
`curl | tar` can walk past is worse than none — it certifies a hole.

Run:  python3 scripts/check_tilt_pin_tests.py
"""

from __future__ import annotations

import unittest
from pathlib import Path

from check_tilt_pin import check_install_script

GOOD = """\
TILT_VERSION="0.37.7"
TILT_SHA256="b695193fab68def8310cb971fa60bbe47ba0a782e24f54ebad287c13316a61b0"
if ! command -v tilt >/dev/null 2>&1; then
  tmp="$(mktemp -d)"
  archive="$tmp/tilt.${TILT_VERSION}.linux.x86_64.tar.gz"
  curl -fsSL --retry 3 -o "$archive" \\
    "https://github.com/tilt-dev/tilt/releases/download/v${TILT_VERSION}/tilt.tar.gz"
  echo "${TILT_SHA256}  ${archive}" | sha256sum -c -
  tar -xzf "$archive" -C "$tmp" tilt
  sudo install -m 0755 "$tmp/tilt" /usr/local/bin/tilt
fi
"""


class CheckTiltPinTests(unittest.TestCase):
    def test_committed_install_script_passes(self) -> None:
        root = Path(__file__).resolve().parent.parent
        text = (root / ".cursor/install.sh").read_text()
        self.assertEqual(check_install_script(text), [])

    def test_good_fixture_passes(self) -> None:
        self.assertEqual(check_install_script(GOOD), [])

    def test_missing_digest_fails(self) -> None:
        mutated = "\n".join(
            l for l in GOOD.splitlines() if not l.startswith("TILT_SHA256=")
        )
        self.assertTrue(check_install_script(mutated))

    def test_decoy_verification_of_unrelated_file_fails(self) -> None:
        # sha256sum -c references the pinned digest but a DIFFERENT file, while
        # the real $archive is installed unverified.
        mutated = GOOD.replace(
            'echo "${TILT_SHA256}  ${archive}" | sha256sum -c -',
            'echo "${TILT_SHA256}  /tmp/decoy.tar.gz" | sha256sum -c -',
        )
        self.assertTrue(check_install_script(mutated))

    def test_single_line_curl_pipe_tar_fails(self) -> None:
        mutated = GOOD.replace(
            '  curl -fsSL --retry 3 -o "$archive" \\\n'
            '    "https://github.com/tilt-dev/tilt/releases/download/v${TILT_VERSION}/tilt.tar.gz"\n'
            '  echo "${TILT_SHA256}  ${archive}" | sha256sum -c -\n'
            '  tar -xzf "$archive" -C "$tmp" tilt\n',
            '  curl -fsSL "https://github.com/tilt-dev/tilt/releases/download/v${TILT_VERSION}/tilt.tar.gz" | tar -xzf - -C "$tmp"\n',
        )
        self.assertTrue(check_install_script(mutated))

    def test_multiline_curl_pipe_tar_fails(self) -> None:
        # The `| tar` hides on a backslash-continued line; joining logical lines
        # must still catch it.
        mutated = GOOD.replace(
            '  curl -fsSL --retry 3 -o "$archive" \\\n'
            '    "https://github.com/tilt-dev/tilt/releases/download/v${TILT_VERSION}/tilt.tar.gz"\n'
            '  echo "${TILT_SHA256}  ${archive}" | sha256sum -c -\n'
            '  tar -xzf "$archive" -C "$tmp" tilt\n',
            '  curl -fsSL \\\n'
            '    "https://github.com/tilt-dev/tilt/releases/download/v${TILT_VERSION}/tilt.tar.gz" \\\n'
            '    | tar -xzf - -C "$tmp"\n',
        )
        self.assertTrue(check_install_script(mutated))

    def test_verify_after_install_fails(self) -> None:
        mutated = GOOD.replace(
            '  echo "${TILT_SHA256}  ${archive}" | sha256sum -c -\n'
            '  tar -xzf "$archive" -C "$tmp" tilt\n'
            '  sudo install -m 0755 "$tmp/tilt" /usr/local/bin/tilt\n',
            '  tar -xzf "$archive" -C "$tmp" tilt\n'
            '  sudo install -m 0755 "$tmp/tilt" /usr/local/bin/tilt\n'
            '  echo "${TILT_SHA256}  ${archive}" | sha256sum -c -\n',
        )
        self.assertTrue(check_install_script(mutated))


if __name__ == "__main__":
    unittest.main()
