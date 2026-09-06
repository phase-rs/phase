#!/usr/bin/env python3
"""Every packaging consumer must ship the GStreamer packages the app declares.

`media_stack.rs`'s `REQUIRED_PLUGINS` is the single authority for the plugin
libraries WebKitGTK needs to play this app's audio, and each entry names the
Debian package that ships it. Two independent consumers have to act on that
list, and neither can see the other:

  * `tauri.conf.json`'s `bundle.linux.deb.depends`, which apt resolves when the
    `.deb` is installed; and
  * `shell-release.yml`'s Linux apt step, which is the entire set
    `linuxdeploy-plugin-gstreamer` has available to copy into the AppImage --
    it bundles what is installed on the runner and nothing else.

The AppImage consumer rests on a precondition its apt list means nothing
without: `bundle.linux.appimage.bundleMediaFramework` is what runs
`linuxdeploy-plugin-gstreamer` in the first place. Flipped off, no plugin is
copied however complete the runner's install is (issue #6744), and every
assertion about that apt step becomes vacuous. So the flag is read before the
step is, and a value other than `true` is refused rather than assumed.

They are checked separately rather than unioned. The `.deb` side already lists
all four packages, so a union would report a clean tree while the AppImage
shipped no AAC decoder -- which is exactly what #8615 was.

A degraded read is refused rather than tolerated. Every extractor here reduces
a file to a set of package names, and every way of failing to understand a file
produces the empty set, which subtracts to no gap and prints a pass. So an
unrecognised declaration raises `Refusal` instead of returning what it managed
to find: a gate that inspected nothing must not be indistinguishable from a
gate that found nothing wrong.

The apt extractor is coupled to a job name and a step name on purpose.
`shell-release.yml` has two Linux apt steps: `shell-preflight`'s runs
`cargo check` and bundles nothing, while `build-shell`'s produces the AppImage.
A file-wide scan would demand runtime GStreamer plugins on a compile-check job,
so the coupling is what keeps the requirement pointed at the step that can
actually satisfy it. When either name moves, this refuses rather than guesses.

MEDIA_PACKAGING_ROOT points every extractor at a tree other than this checkout,
which is how the tests drive it against throwaway fixtures.

Usage: check_media_plugin_packaging.py
"""

from __future__ import annotations

import json
import os
import re
import shlex
import sys
from pathlib import Path
from typing import Callable, NamedTuple

try:
    import yaml
except ModuleNotFoundError:  # pragma: no cover - exercised by the runner, not tests
    # Falling back to a pattern would silently weaken the gate, which is worse
    # than not running: the result would look identical to a clean pass.
    # Exit 2, not `sys.exit(str)`'s 1: `main` reserves 1 for "a consumer is
    # missing packages" and 2 for "I could not read this", and a missing parser
    # is the second kind. Collapsing them would make the refusal contract this
    # module is built on unreadable from the exit code.
    print("REFUSED: check_media_plugin_packaging: PyYAML is required and was "
          "not found; refusing to check with a weaker method", file=sys.stderr)
    sys.exit(2)

ROOT = Path(os.environ.get("MEDIA_PACKAGING_ROOT")
            or Path(__file__).resolve().parent.parent).resolve()

MEDIA_STACK = "client/src-tauri/src/media_stack.rs"
TAURI_CONF = "client/src-tauri/tauri.conf.json"
SHELL_RELEASE = ".github/workflows/shell-release.yml"

BUILD_JOB = "build-shell"
APT_STEP = "Install system dependencies (Linux)"

# Anchored on the symbol name, not on a type spelling: `&[T]`, `[T; N]` and a
# type alias must all match, because none of them changes what the constant is.
PLUGIN_BLOCK = re.compile(
    r"pub const REQUIRED_PLUGINS\s*:[^=]*=\s*&?\[(.*?)\n\];", re.S)
ENTRY_MARKER = re.compile(r"RequiredPlugin\s*\{")
LIBRARY_FIELD = re.compile(r'library:\s*"([^"]+)"')
PACKAGE_FIELD = re.compile(r'debian_package:\s*"([^"]+)"')
# `${{ matrix.os == 'linux' }}` and the bare expression are the same condition.
LINUX_ARM = re.compile(r"""matrix\.os\s*==\s*['"]linux['"]""")
# `apt-get install` / `apt install`, as a command rather than as prose.
APT_INSTALL = re.compile(r"\bapt(?:-get)?\s+(?:-\S+\s+)*install\b")
#: Shell control flow. A gate that reads lines cannot tell which of them run.
SHELL_CONTROL_FLOW = frozenset({
    "if", "then", "elif", "else", "fi", "case", "esac",
    "while", "until", "for", "do", "done",
})
PRIVILEGE_WRAPPERS = frozenset({"sudo", "env"})
APT_COMMANDS = frozenset({"apt-get", "apt"})
#: A Debian package name, optionally multi-arch qualified. Anything else on an
#: install line is an expression this gate cannot resolve, not a package.
DEBIAN_NAME = re.compile(r"[a-z0-9][a-z0-9+.-]*(?::[a-z0-9-]+)?")
#: Shell operators that end a command's argument list.
SHELL_OPERATORS = frozenset({"&&", "||", "|", ";", "&"})
#: The characters those operators are built from, for spotting one glued to a word.
OPERATOR_CHARS = frozenset("&|;")


class Refusal(Exception):
    """A file could not be read the way this gate needs to read it.

    The single authority for every degraded read. No extractor decides on its
    own to soft-fail by returning an empty set, because an empty set is
    indistinguishable from full coverage once it reaches the subtraction.
    """


def required_packages() -> set[str]:
    """The Debian packages `REQUIRED_PLUGINS` declares. The authority."""
    path = ROOT / MEDIA_STACK
    if not path.is_file():
        raise Refusal(f"{MEDIA_STACK} does not exist; the package authority is "
                      "missing, so there is nothing to check consumers against")
    source = path.read_text(encoding="utf-8")

    block = PLUGIN_BLOCK.search(source)
    if block is None:
        raise Refusal(f"{MEDIA_STACK} has no readable `pub const "
                      "REQUIRED_PLUGINS` declaration; it was renamed or "
                      "restructured, and the authority cannot be read")
    body = block.group(1)

    entries = len(ENTRY_MARKER.findall(body))
    libraries = LIBRARY_FIELD.findall(body)
    packages = PACKAGE_FIELD.findall(body)

    if not entries:
        raise Refusal(f"{MEDIA_STACK}: REQUIRED_PLUGINS declares zero plugin "
                      "entries; an empty authority would pass every consumer")
    if not (entries == len(libraries) == len(packages)):
        raise Refusal(
            f"{MEDIA_STACK}: REQUIRED_PLUGINS reads inconsistently -- "
            f"{entries} entry marker(s), {len(libraries)} library literal(s), "
            f"{len(packages)} debian_package literal(s). A field that is not a "
            "string literal here would silently shrink the authority, so the "
            "partial read is refused")
    return set(packages)


def nested(value: object, *keys: str) -> object:
    """A key path through parsed JSON, `None` at the first thing that is not a
    mapping. A missing branch and a branch of the wrong shape are the same
    answer here: whatever the caller wanted is not declared."""
    for key in keys:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value


#: A key that is absent, told apart from a key whose value really is `None`.
MISSING = object()
#: What to call the whole document when the break is at the top level.
DOCUMENT_ROOT = "the document root"


def first_non_mapping(value: object, *keys: str) -> tuple[str, object] | None:
    """The first segment of a key path that is present but is not a mapping.

    `nested` folds "absent" and "wrong shape" into one `None`, which is the
    right answer wherever every non-matching reading is the same answer. Where
    the refusal has to say what is actually in the file, blaming the leaf for a
    branch that broke two levels up sends an operator looking for the wrong
    key, so this names the segment that actually broke the walk.
    """
    walked: list[str] = []
    for key in keys:
        if not isinstance(value, dict):
            # A key that is simply absent is not a shape problem, and the
            # downstream "depends is absent" refusal already says that truthfully.
            # Reporting it here would reintroduce the false sentence one level up.
            if value is MISSING:
                return None
            return ".".join(walked) or DOCUMENT_ROOT, value
        walked.append(key)
        value = value.get(key, MISSING)
    return None


def tauri_config(consumer: str) -> object:
    """`tauri.conf.json`, parsed. Both Linux consumers read this one file --
    the `.deb` for its depends, the AppImage for its bundling precondition --
    so the refusal names which of them the caller was checking."""
    path = ROOT / TAURI_CONF
    if not path.is_file():
        raise Refusal(f"{TAURI_CONF} does not exist; the {consumer} cannot "
                      "be checked")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise Refusal(f"{TAURI_CONF} is not parseable JSON, so the {consumer} "
                      f"cannot be checked: {exc}") from exc


def debian_package_name(entry: object) -> str:
    """The bare package name from one `depends` entry.

    `bundle.linux.deb.depends` takes ordinary Debian dependency syntax, so
    `gstreamer1.0-libav (>= 1.24)` and `gstreamer1.0-libav | gstreamer1.0-plugins-ugly`
    both name a package this gate must recognise. Comparing the raw strings
    would fail a correct tree with "is missing gstreamer1.0-libav" while the
    file visibly contains it. First alternative wins: apt installs it unless it
    is unavailable, so it is the one the runner actually gets. A multi-arch
    qualifier (`gstreamer1.0-libav:any`) names the same package too.
    """
    if not isinstance(entry, str):
        raise Refusal(f"{TAURI_CONF}: bundle.linux.deb.depends contains a "
                      f"{type(entry).__name__}, not a package name string")
    name = entry.split("|")[0].split("(")[0].split(":")[0].strip()
    if not name:
        raise Refusal(f"{TAURI_CONF}: bundle.linux.deb.depends contains an "
                      f"entry that names no package: {entry!r}")
    return name


def deb_depends() -> set[str]:
    """What apt installs alongside the `.deb`."""
    config = tauri_config(".deb consumer")

    # `nested` folds "absent" and "wrong shape" into one `None`. That is the
    # right answer for the AppImage premise below -- a tree that is not a
    # mapping is not `true` either -- but it is a false sentence here: with
    # `deb` set to a string the file does declare something, and calling
    # `depends` absent would describe a file that does not exist. Separate the
    # two so the refusal says what is actually in the JSON.
    # Walking one segment past `deb` forces `deb` itself to be a mapping, and
    # naming the segment that broke the walk keeps the refusal true at every
    # level: with `bundle` set to a string, "depends is absent" would describe
    # a file that does not exist.
    broken = first_non_mapping(config, "bundle", "linux", "deb", "depends")
    if broken is not None:
        path, value = broken
        raise Refusal(f"{TAURI_CONF}: {path} is {type(value).__name__}, not an "
                      "object, so the .deb declares no depends list this gate "
                      "can read")

    depends = nested(config, "bundle", "linux", "deb", "depends")
    if depends is None:
        raise Refusal(f"{TAURI_CONF}: bundle.linux.deb.depends is absent; the "
                      ".deb declares no runtime dependencies at all")
    if not isinstance(depends, list):
        raise Refusal(f"{TAURI_CONF}: bundle.linux.deb.depends is "
                      f"{type(depends).__name__}, not a list")
    if not depends:
        raise Refusal(f"{TAURI_CONF}: bundle.linux.deb.depends is empty; an "
                      "empty dependency list would pass this gate vacuously")
    return {debian_package_name(entry) for entry in depends}


def appimage_apt_packages() -> set[str]:
    """What is installed on the runner that builds the AppImage.

    `linuxdeploy-plugin-gstreamer` copies from this set and no other, so a
    plugin absent here is absent from the AppImage -- but only while
    `bundleMediaFramework` is what runs that plugin, which is checked first.
    """
    # The premise before the evidence. `bundleMediaFramework` is the switch that
    # runs linuxdeploy-plugin-gstreamer at all; with it off the AppImage ships
    # no GStreamer plugin regardless of the apt list, which is issue #6744 and
    # would leave this gate reporting a clean tree over a silent bundle.
    framework = nested(tauri_config("AppImage consumer's bundleMediaFramework "
                                    "premise"),
                       "bundle", "linux", "appimage", "bundleMediaFramework")
    if framework is not True:
        raise Refusal(
            f"{TAURI_CONF}: bundle.linux.appimage.bundleMediaFramework is "
            f"{json.dumps(framework)}, not true. That flag is what runs "
            "linuxdeploy-plugin-gstreamer, so with it off the AppImage bundles "
            "no GStreamer plugin at all and the apt step below stops meaning "
            "anything -- the premise this consumer rests on is gone (#6744)")

    path = ROOT / SHELL_RELEASE
    if not path.is_file():
        raise Refusal(f"{SHELL_RELEASE} does not exist; the AppImage consumer "
                      "cannot be checked")
    try:
        workflow = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise Refusal(f"{SHELL_RELEASE} is not parseable YAML: {exc}") from exc

    jobs = (workflow or {}).get("jobs") or {}
    job = jobs.get(BUILD_JOB)
    if not isinstance(job, dict):
        raise Refusal(f"{SHELL_RELEASE}: job '{BUILD_JOB}' is absent; the job "
                      "that builds the AppImage was renamed, and this gate no "
                      "longer knows which apt step bundles plugins")

    steps = job.get("steps") or []
    step = next((s for s in steps
                 if isinstance(s, dict) and s.get("name") == APT_STEP), None)
    if step is None:
        raise Refusal(f"{SHELL_RELEASE}: job '{BUILD_JOB}' has no step named "
                      f"'{APT_STEP}'; the step this gate reads was renamed or "
                      "removed")

    condition = str(step.get("if", ""))
    if not LINUX_ARM.search(condition):
        raise Refusal(f"{SHELL_RELEASE}: step '{APT_STEP}' is guarded by "
                      f"{condition!r}, which is no longer the Linux matrix arm. "
                      "Packages declared on a step that does not run on the "
                      "AppImage build are not declared")

    # Join backslash continuations first: a package list wrapped across lines is
    # one command, and tokenizing the halves separately loses the tail.
    body = re.sub(r"\\\s*\n\s*", " ", str(step.get("run", "")))
    # Commented text is not an install, wherever on the line it starts. The
    # tokenizer strips it below (`comments=True`), which is the only filter
    # that holds: a leading-`#` test sees neither a trailing
    # `... libgtk-3-dev  # TODO: restore apt-get install gstreamer1.0-libav`
    # nor a continuation folded onto a live line by the join above, and
    # `tokens.index("install")` then drops the `#` along with everything before
    # the verb -- so those packages would enter the covered set, fail-open in
    # the one direction this gate must be closed.
    install_lines = [line for line in body.splitlines()
                     if APT_INSTALL.search(line)]
    # Line-scoped reading cannot tell which lines execute. An install wrapped
    # in `if false; then ... fi` across three lines has an `apt-get` head token
    # on the middle one and would be credited, which is #8615 with a green
    # check: a maintainer guarding the GStreamer install behind a condition
    # that does not hold on the release runner would ship a silent AppImage.
    # This step is two plain commands; anything needing shell control flow is
    # beyond what this gate can read, so it refuses rather than guesses.
    for line in body.splitlines():
        try:
            tokens = shlex.split(line, comments=True)
        except ValueError:
            continue  # An untokenizable line is refused below, with its message.
        # Only in command position. `shlex` strips quotes, so a whole-token
        # match would refuse `echo "done"` and any sentence containing a bare
        # `for` -- a false refusal on a tree that is entirely correct. A
        # conventionally formatted block leads its lines with the keyword.
        command_position = True
        for token in tokens:
            if command_position and token in SHELL_CONTROL_FLOW:
                raise Refusal(
                    f"{SHELL_RELEASE}: step '{APT_STEP}' uses shell control "
                    f"flow: {line.strip()!r}. This gate reads install lines "
                    "one at a time and cannot tell which of them run, so it "
                    "will not credit packages from a conditional step")
            # A redirection into a file is not an install either, and its body
            # lines look exactly like one to a line-scoped reader.
            if token.startswith("<<"):
                raise Refusal(
                    f"{SHELL_RELEASE}: step '{APT_STEP}' writes a heredoc: "
                    f"{line.strip()!r}. Lines inside it are text, not commands "
                    "this step runs, and this gate cannot tell the two apart")
            command_position = (token in SHELL_CONTROL_FLOW
                                or token.endswith(";")
                                or token in SHELL_OPERATORS)

    if not install_lines:
        raise Refusal(f"{SHELL_RELEASE}: step '{APT_STEP}' runs no "
                      "`apt-get install`; it no longer installs anything this "
                      "gate can read")

    packages: set[str] = set()
    for line in install_lines:
        try:
            tokens = shlex.split(line, comments=True)
        except ValueError as exc:
            raise Refusal(f"{SHELL_RELEASE}: step '{APT_STEP}' has an "
                          f"untokenizable install line: {exc}") from exc
        # A line that was entirely a comment tokenizes to nothing. It installs
        # nothing and says nothing about what does, so it contributes no
        # packages and is not a line this gate has to understand.
        if not tokens:
            continue
        # A line that merely *mentions* an install is not one. `echo apt-get
        # install gstreamer1.0-libav` and `if false; then apt-get install ...`
        # both contain the verb, and crediting what follows it would count
        # packages the runner never receives -- fail-open in the one direction
        # this gate must be closed. Refuse rather than guess what a line runs.
        # Strip everything that can legitimately precede the apt command:
        # repeated `VAR=value` assignments, `sudo`/`env`, and a wrapper's own
        # options. `sudo -E apt-get install ...` and
        # `sudo DEBIAN_FRONTEND=noninteractive apt-get install ...` are both
        # ordinary shapes, and refusing them would block a correct tree with a
        # message saying the line does not run an install when it does. Only a
        # wrapper licenses skipping a flag, so a bare `-x apt-get` still stops
        # at a token that is not an apt command.
        # A line is a sequence of commands joined by operators, and each has
        # to be judged on its own. Searching the whole line for `install`
        # credits a later segment's arguments to an earlier segment's verdict:
        # `apt-get update && echo apt-get install <packages>` passed the head
        # check on `apt-get update`, then found the `echo`'s `install` and
        # credited everything after it -- a step that installs nothing reading
        # as full coverage, which is the fail-open this gate exists to prevent.
        for segment in split_on_operators(tokens):
            # Only a command that claims to be an apt install is held to that
            # standard. `pip install pyyaml` and `echo install complete` name a
            # verb this gate does not own, and refusing them would block a line
            # that does run a real apt install alongside. `echo apt-get install
            # x` still matches here, and still has to pass the head check.
            if not APT_INSTALL.search(" ".join(segment)):
                continue

            head, wrapped = 0, False
            while head < len(segment):
                token = segment[head]
                if "=" in token and not token.startswith("-"):
                    head += 1
                elif token in PRIVILEGE_WRAPPERS:
                    head, wrapped = head + 1, True
                elif wrapped and token.startswith("-"):
                    head += 1
                else:
                    break
            if head >= len(segment) or segment[head] not in APT_COMMANDS:
                raise Refusal(f"{SHELL_RELEASE}: step '{APT_STEP}' has a "
                              f"command that names `apt-get install` without "
                              f"running it: {' '.join(segment)!r}. This gate "
                              "will not guess which packages such a command "
                              "installs")
            # `APT_INSTALL` matches `install\b`, so a segment can satisfy the
            # regex and the head check while carrying no bare `install` token
            # -- `apt-get install-foo -y x`. Slicing on a missing token would
            # raise, and an uncaught traceback exits 1, the code reserved for
            # "a consumer is missing packages". An unreadable verb is a
            # degraded read, so it refuses at 2 like every other one.
            if "install" not in segment:
                raise Refusal(f"{SHELL_RELEASE}: step '{APT_STEP}' has a "
                              f"command this gate cannot read as an install: "
                              f"{' '.join(segment)!r}")
            arguments = segment[segment.index("install") + 1:]

            for token in arguments:
                if token.startswith("-"):
                    continue
                # An unexpanded `$PACKAGES` or `${{ env.X }}` is not a package
                # this gate can resolve. Crediting it as a literal name reports
                # a correct tree as *missing* the four packages the step
                # installs -- blocking CI while telling the maintainer to add
                # what is already there. The deb side already refuses names it
                # cannot read.
                if not DEBIAN_NAME.fullmatch(token):
                    raise Refusal(f"{SHELL_RELEASE}: step '{APT_STEP}' installs "
                                  f"{token!r}, which is not a literal package "
                                  "name this gate can resolve -- most likely a "
                                  "shell or workflow expression it cannot expand")
                # Normalized through the same helper as the `.deb` side, so
                # `gstreamer1.0-libav:amd64` matches the authority on both
                # consumers rather than on only one of them.
                packages.add(debian_package_name(token))

    if not packages:
        raise Refusal(f"{SHELL_RELEASE}: step '{APT_STEP}' tokenizes to zero "
                      "package names; an empty install list would pass this "
                      "gate vacuously")
    return packages


def split_on_operators(tokens: list[str]) -> list[list[str]]:
    """One token list per command, split on the shell operators joining them.

    `apt-get update && echo apt-get install x` is two commands, and only the
    second mentions an install. Judging the line as a whole lets the first
    command satisfy the "is this really an apt install" check while the second
    supplies the packages.
    """
    segments: list[list[str]] = [[]]
    for token in tokens:
        # `shlex` splits on whitespace, not on shell metacharacters, so
        # `update&&echo` survives as one token and hides a command boundary
        # between the head this gate checks and the `install` it searches for.
        # No package name, flag, or path contains one, so a token that carries
        # an operator without being one is a shape this gate cannot read.
        # A single trailing `;` is an ordinary separator and is handled below;
        # strip it before looking for an operator buried inside a word.
        body = token[:-1] if token.endswith(";") else token
        if token not in SHELL_OPERATORS and OPERATOR_CHARS.intersection(body):
            raise Refusal(f"{SHELL_RELEASE}: step '{APT_STEP}' has {token!r}, "
                          "which joins a shell operator to a word. This gate "
                          "splits a line into commands on whitespace-delimited "
                          "operators and cannot see the boundary inside that "
                          "token; put spaces around the operator")
        if token in SHELL_OPERATORS:
            segments.append([])
        elif token.endswith(";"):
            segments[-1].append(token[:-1])
            segments.append([])
        else:
            segments[-1].append(token)
    return [segment for segment in segments if segment]


class Consumer(NamedTuple):
    """One place a required package has to be named to actually be shipped."""

    name: str
    description: str
    extract: Callable[[], set[str]]


# Adding a third packaging consumer is appending a record here, not a new branch.
CONSUMERS = (
    Consumer(
        name=".deb dependencies",
        description=f"{TAURI_CONF} -> bundle.linux.deb.depends",
        extract=deb_depends,
    ),
    Consumer(
        name="AppImage build runner",
        description=f"{SHELL_RELEASE} -> {BUILD_JOB} -> '{APT_STEP}'",
        extract=appimage_apt_packages,
    ),
)


def main() -> int:
    try:
        required = required_packages()
    except Refusal as exc:
        print(f"REFUSED: {exc}", file=sys.stderr)
        return 2

    refused = False
    gaps = False
    covered_counts: list[str] = []
    for consumer in CONSUMERS:
        try:
            covered = consumer.extract()
        except Refusal as exc:
            print(f"REFUSED: {exc}", file=sys.stderr)
            refused = True
            continue

        # Per consumer, never a union: the .deb already covers every package, so
        # a unioned check passes a tree whose AppImage ships none of them.
        missing = sorted(required - covered)
        if missing:
            print(f"{consumer.name} ({consumer.description}) is missing "
                  f"{len(missing)} package(s) declared by REQUIRED_PLUGINS:",
                  file=sys.stderr)
            for package in missing:
                print(f"  {package}", file=sys.stderr)
            gaps = True
        else:
            covered_counts.append(f"{consumer.name}: {len(covered)} package(s)")

    if gaps:
        print(f"Add each missing package to that consumer, or drop it from "
              f"REQUIRED_PLUGINS in {MEDIA_STACK} if the app no longer needs "
              "it.", file=sys.stderr)
    # A refusal outranks a gap: "I could not read this" is a different answer
    # from "I read it and it is short", and an operator needs to tell them apart.
    if refused:
        return 2
    if gaps:
        return 1

    print(f"media plugin packaging OK: {len(required)} package(s) required by "
          f"REQUIRED_PLUGINS ({', '.join(sorted(required))}), covered by "
          f"{len(CONSUMERS)} consumer(s) [{'; '.join(covered_counts)}]")
    return 0


if __name__ == "__main__":
    sys.exit(main())
