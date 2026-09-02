#!/usr/bin/env python3
"""Static gate: the Cloud Agent bootstrap installs tilt only from a
digest-verified archive.

`.cursor/environment.json` runs `.cursor/install.sh` automatically, and that
script `sudo install`s the tilt binary to /usr/local/bin. A tampered or
substituted release would therefore become root-level code on every new
environment. The single defense is a pinned SHA-256 checked before install
(mirroring `.github/actions/binaryen`).

This gate fails closed if that defense is ever weakened. It does not merely look
for a `sha256sum -c` somewhere — it binds the whole chain to ONE archive
variable and requires the correct order:

    curl ... -o "$archive"              download the release into a file
    ... "$TILT_SHA256" ... "$archive" | sha256sum -c   verify THAT file
    tar -x... "$archive"                extract THAT file
    sudo install .../tilt /usr/local/bin/tilt          install the binary

so it rejects (a) decoy verification of some unrelated file while the real
install still trusts an unverified download, and (b) `curl | tar` pipelines
(including multiline, backslash-continued ones) that never touch a file at all.

Run:  python3 scripts/check_tilt_pin.py --check
Tests: python3 scripts/check_tilt_pin_tests.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

DEFAULT_TARGET = ".cursor/install.sh"


def logical_lines(text: str) -> list[tuple[int, str]]:
    """Join backslash-continued physical lines into logical lines.

    Returns `(first_physical_line_number, joined_text)` pairs so a multiline
    `curl \\ | tar` pipeline is examined as one string and cannot smuggle the
    `| tar` past a per-physical-line scan.
    """
    out: list[tuple[int, str]] = []
    buf: list[str] = []
    start = 0
    for n, raw in enumerate(text.splitlines(), start=1):
        if not buf:
            start = n
        stripped = raw.rstrip()
        if stripped.endswith("\\"):
            buf.append(stripped[:-1])
            continue
        buf.append(stripped)
        out.append((start, " ".join(buf)))
        buf = []
    if buf:
        out.append((start, " ".join(buf)))
    return out


def _refs_var(text: str, var: str) -> bool:
    """True if `text` references shell variable `var` as `$var` or `${var}`."""
    return re.search(r"\$\{?" + re.escape(var) + r"\}?\b", text) is not None


def check_install_script(text: str) -> list[str]:
    """Return a list of failure messages; empty means the gate passes."""
    failures: list[str] = []
    lines = logical_lines(text)

    def find(pattern: str) -> tuple[int, int, str] | None:
        """First `(index, physical_line, text)` whose text matches `pattern`."""
        rx = re.compile(pattern)
        for i, (phys, t) in enumerate(lines):
            if rx.search(t):
                return (i, phys, t)
        return None

    # 1. Version and a 64-hex-char digest must both be pinned.
    if not any(re.match(r'TILT_VERSION="[0-9]+\.[0-9]+\.[0-9]+"', t) for _, t in lines):
        failures.append("TILT_VERSION is not pinned to an x.y.z release")
    if not any(re.match(r'TILT_SHA256="[0-9a-f]{64}"', t) for _, t in lines):
        failures.append("TILT_SHA256 is not pinned to a 64-hex-char digest")

    # 2. No curl may pipe straight into tar — checked on JOINED logical lines so
    #    a backslash-continued `curl \ | tar` is caught too.
    for _, phys, t in ((i, p, t) for i, (p, t) in enumerate(lines)):
        if re.search(r"\bcurl\b", t) and re.search(r"\|\s*tar\b", t):
            failures.append(
                f"line {phys}: curl is piped into tar; download to a file and "
                "verify the digest before extracting"
            )
            break

    # 3. The download must write the release to a file via `curl ... -o <var>`;
    #    capture that archive variable so every later step is bound to it.
    download = find(r"\bcurl\b.*\s-o\s+\"?\$\{?\w+\}?\"?")
    archive_var: str | None = None
    if download is None:
        failures.append("no `curl ... -o <archive>` download-to-file step found")
    else:
        m = re.search(r"-o\s+\"?\$\{?(\w+)\}?\"?", download[2])
        archive_var = m.group(1) if m else None

    # 4. Verification must run `sha256sum -c` against the pinned digest AND the
    #    SAME archive variable (rejects decoy verification of an unrelated file).
    verify = None
    for i, (phys, t) in enumerate(lines):
        if not re.search(r"\bsha256sum\s+-c\b", t):
            continue
        if not _refs_var(t, "TILT_SHA256"):
            continue
        if archive_var is not None and not _refs_var(t, archive_var):
            continue
        verify = (i, phys, t)
        break
    if verify is None:
        failures.append(
            "no `sha256sum -c` verification binding $TILT_SHA256 to the "
            "downloaded archive was found"
        )

    # 5. Extraction must operate on the SAME archive variable (rejects `tar -xzf -`
    #    reading from a pipe, and rejects extracting some other file).
    extract = None
    if archive_var is not None:
        for i, (phys, t) in enumerate(lines):
            if re.search(r"\btar\b.*-x", t) and _refs_var(t, archive_var):
                extract = (i, phys, t)
                break
        if extract is None:
            failures.append(
                f"no `tar -x ... ${archive_var}` extraction of the verified "
                "archive was found"
            )

    # 6. The verified binary must be installed via `sudo install ... tilt`.
    install = find(r"\bsudo install\b.*\btilt\b")
    if install is None:
        failures.append("no `sudo install ... tilt` step found")

    # 7. Order: download -> verify -> extract -> install.
    stages = [
        ("download", download),
        ("verify", verify),
        ("extract", extract),
        ("install", install),
    ]
    present = [(name, s[0]) for name, s in stages if s is not None]
    for (a_name, a_idx), (b_name, b_idx) in zip(present, present[1:]):
        if a_idx >= b_idx:
            failures.append(
                f"{a_name} must come before {b_name} "
                f"(found {a_name} at/after {b_name})"
            )

    # 8. TOCTOU: nothing may reassign or rewrite the archive between the bound
    #    verification and the extraction. Otherwise the verified file can be
    #    swapped for an unverified payload (a second download, a redirect, a
    #    `cp`/`mv`/`tee`, or a plain `archive=` reassignment) before `tar` and
    #    `sudo install` ever see it. The only reference to the archive allowed in
    #    that window is the extraction itself, so any other touch fails closed.
    if archive_var is not None and verify is not None and extract is not None:
        assign_rx = re.compile(r"(?:^|[;&|]|\s)" + re.escape(archive_var) + r"=")
        for i in range(verify[0] + 1, extract[0]):
            phys, t = lines[i]
            if assign_rx.search(t) or _refs_var(t, archive_var):
                failures.append(
                    f"line {phys}: ${archive_var} is reassigned or rewritten "
                    "between verification and extraction (TOCTOU)"
                )
                break

    return failures


def _command_strings(node: object) -> list[str]:
    """Every `command` string value anywhere in the parsed environment.json.

    Only executable command fields are constrained — free-text `description`
    fields legitimately mention `tilt up -- server` etc. and must not be scanned.
    """
    out: list[str] = []
    if isinstance(node, dict):
        cmd = node.get("command")
        if isinstance(cmd, str):
            out.append(cmd)
        for v in node.values():
            out.extend(_command_strings(v))
    elif isinstance(node, list):
        for v in node:
            out.extend(_command_strings(v))
    return out


def check_environment_json(text: str) -> list[str]:
    """A dev-loop terminal `command` must invoke the absolute /usr/local/bin/tilt,
    never a bare `tilt` resolved through PATH (which a stale/substituted earlier
    entry could hijack)."""
    import json

    failures: list[str] = []
    try:
        commands = _command_strings(json.loads(text))
    except (json.JSONDecodeError, ValueError):
        # environment.json permits comments/trailing content in some tooling;
        # fall back to a line scan of the raw text if JSON parsing fails.
        commands = [text]

    for cmd in commands:
        # A `tilt up` not immediately preceded by the pinned absolute path is a
        # bare PATH lookup.
        if re.search(r"(?<![\w/])tilt\s+up\b", cmd):
            failures.append(
                f"environment.json command invokes a bare `tilt up` via PATH; "
                f"use /usr/local/bin/tilt: {cmd!r}"
            )
        elif "/usr/local/bin/tilt up" in cmd:
            continue  # explicit pinned invocation — good
    return failures


def main(argv: list[str]) -> int:
    root = Path(__file__).resolve().parent.parent
    install_path = root / DEFAULT_TARGET
    env_path = root / ".cursor/environment.json"

    failures: list[str] = []
    if not install_path.is_file():
        failures.append(f"{DEFAULT_TARGET} not found")
    else:
        failures.extend(check_install_script(install_path.read_text()))
    if env_path.is_file():
        failures.extend(check_environment_json(env_path.read_text()))

    if failures:
        print("check-tilt-pin FAILED:", file=sys.stderr)
        for f in failures:
            print(f"  {f}", file=sys.stderr)
        return 1
    print("check-tilt-pin PASS (tilt archive is digest-verified before install)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
