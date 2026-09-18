#!/usr/bin/env python3
"""Every desktop platform the shell release publishes must have an engine mapping.

`shell-release.yml`'s `build-shell` matrix is the set of desktop platforms a tag
publishes. `native_engine.rs`'s `ServerPlatform` is how each of those desktops
resolves which engine binary to fetch. A platform published without a variant
ships a desktop that cannot acquire an engine at all.

`native_engine.rs`'s own unit test pins the pairs it expects, which is what makes
it discriminating against the mapping being deleted or broken. What it cannot see
is the matrix growing past them: nothing there refers to the workflow. This gate
is that tie, and it runs on every pull request rather than at tag time.

Nothing here soft-fails to an empty set: every way of failing to read a file
raises, because an empty matrix is a subset of any mapping and a gate that
understood nothing would otherwise print a pass. The mapping is read three times
over -- `ALL`, `os_arch`, `target_triple` -- and the three must name the same
variants. `ALL` is the only thing `from_os_arch` iterates, so a variant absent
from it resolves for no platform at runtime while both matches stay exhaustive
and every population still reads as expected. Each arm block's `Self::`
receivers are compared as a set against the arms read out of it as well, because
an arm rustfmt broke across lines still carries its receiver and would otherwise
be dropped in silence. Two arms naming one platform, or resolving one triple,
refuse for the same reason: a variant nothing can reach.

Every comparison here is between sets of fully-qualified names rather than their
sizes, because a size holds while its members are substituted underneath it: two
variants collapsing onto one platform, a variant absent from `ALL`, a binary
attached under a name no desktop derives. A set equality has the opposite blind
spot -- an extra member completes it instead of breaking it -- so each block is
also held against something a name this gate invented cannot satisfy. For the
arm blocks that is the receiver-against-arm comparison. For `ALL`, whose entries
carry no arms, it is the `[Self; N]` length rustc checks against those entries,
read out of the same match as the entries themselves so that no other occurrence
of that shape can supply it.

Non-code text is removed once, where the source is read, so nothing downstream
has raw text in scope to read by mistake. It is removed against each language's
own grammar rather than the spelling some defect happened to use: Rust's two
comment forms, nested to any depth, and neither of them opening inside a string
literal of either kind. The release side takes two
cuts, because that step's shell carries prose comments and its asset list is
data inside a quoted heredoc, where a `#` is neither a comment nor a path the
release attaches. The list is read out of that heredoc alone, and every line of
it must match an `artifacts/<dir>/<asset>` path whole -- a pattern that matched
only a line's tail read either kind of commented name as an attached one.

The other direction is the same defect pointed the other way. Every variant
resolves the asset name a desktop on that platform downloads -- its triple plus
the `.exe` a `windows` variant's `executable_suffix` appends -- and `release.yml`
is what publishes those assets. A name resolved but never published is a desktop
requesting a URL that 404s. That side is read twice too, from the signing loop
and from the attached asset list, because each proves what the other cannot: the
loop's `test -s` fails the release when the binary was never built, and the asset
list is what the release actually carries. Two readings that disagree describe no
published set, so they refuse rather than pick one. Their agreement is compared
on triples, which is all the loop names; the suffix axis is held against the
mapping instead, where getting it wrong is a silent 404 rather than a release
that fails its own `test -s`.

The published set is allowed to be a superset. It quantifies over the release's
slim servers, not over desktop platforms, so a triple published for a consumer
that is not a desktop strands nothing and is not counted.

The mapping is read by regex over `ServerPlatform`'s own blocks, each anchored on
its name, so renaming a method or reshaping its arms breaks this gate loudly
rather than silently. The expected populations are checked last and carry their
own exit code: a readable set that moved is neither a coverage gap nor a failure
to read, and checking it first would let a grown matrix hide the very desktop
that cannot fetch an engine.
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

try:
    import yaml
except ModuleNotFoundError:
    # Exit 2, not 1: `main` reserves 1 for "a published platform has no mapping",
    # 2 for "I could not read this", and 3 for "a population I read perfectly has
    # moved". A missing parser is the second kind, and collapsing them would make
    # the refusal unreadable from the exit code.
    print("REFUSED: check_shell_platform_mapping: PyYAML is required and was "
          "not found; refusing to check with a weaker method", file=sys.stderr)
    sys.exit(2)

ROOT = Path(os.environ.get("SHELL_PLATFORM_MAPPING_ROOT")
            or Path(__file__).resolve().parent.parent).resolve()

MAPPING_SOURCE = "client/src-tauri/src/native_engine.rs"
SHELL_RELEASE = ".github/workflows/shell-release.yml"
BUILD_JOB = "build-shell"
RELEASE_WORKFLOW = ".github/workflows/release.yml"
RELEASE_JOB = "release"
SIGN_STEP = "sign-release-artifacts"
ASSET_STEP = "release-assets"

#: The preview channel provisions its own servers, and `native_engine.rs` reaches
#: them by a different route than a release: `ResolvedArtifact` for a Preview key
#: looks the host's `target_triple()` up in the signed manifest's `binaries` map,
#: so a triple absent from that map is a desktop that resolves nothing. Nothing
#: above reads this workflow, and the two populations it declares are spelled four
#: separate times inside it -- the build matrix, the four artifact downloads, the
#: shell array that signs and uploads, and the jq object that writes the manifest.
#: Any one of them can be edited alone, which is exactly the drift this gate
#: exists to refuse; the release half is held to the same standard two files over.
#: The publish job's steps carry no `id`, so its signing step is named rather than
#: identified. Adding an `id` would be an edit to a publishing workflow made to
#: suit its own observer, and `.github/workflows/**` is a hard stop besides.
PREVIEW_WORKFLOW = ".github/workflows/preview-server.yml"
PREVIEW_BUILD_JOB = "build"
PREVIEW_PUBLISH_JOB = "publish"
PREVIEW_SIGN_STEP = "Sign binaries, publish manifest, and garbage-collect old pairs"
PREVIEW_ARTIFACT_PREFIX = "preview-server-"

#: `ALL` is the variant list `from_os_arch` iterates, and the two methods are the
#: arms it resolves them through. Each is anchored on its own name and bounded by
#: the closing brace at its indentation, so none of the three can be read as
#: another's contents -- the module also holds a free `target_triple()` function,
#: whose body carries no arms at all and so would read as an empty mapping rather
#: than as a wrong one.
#: `ALL`'s declared length is captured here rather than by a second pattern, so
#: the figure and the entries it is held against come out of one match. Read
#: separately it was satisfiable by any `[Self; N]` elsewhere in the file, which
#: is the whole forgery this cross-check exists to refuse.
MAPPING_ALL_BLOCK = re.compile(
    r"const ALL:\s*\[Self;\s*(\d+)\]\s*=\s*\[(.*?)\n    \];", re.S)
MAPPING_BLOCK = re.compile(r"fn os_arch\b[^{]*\{(.*?)\n    \}", re.S)
MAPPING_TRIPLE_BLOCK = re.compile(
    r"fn target_triple\(self\)[^{]*\{(.*?)\n    \}", re.S)

#: Every arm is read through its `Self::` receiver, which is what joins the three
#: blocks into one record per variant. The receivers are also compared as a set
#: against the arms read beside them: an arm rustfmt broke across lines still
#: carries its receiver but matches neither value pattern below, so a receiver
#: with no arm is how a dropped arm announces itself, by name.
MAPPING_RECEIVER = re.compile(r"Self::(\w+)")
MAPPING_ENTRY = re.compile(
    r'Self::(\w+)\s*=>\s*\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)')
MAPPING_TRIPLE_ARM = re.compile(r'Self::(\w+)\s*=>\s*"([^"]+)"')

SLIM_PREFIX = "phase-server-slim-"

#: The release job's two spellings of its published set: the `for triple in ...`
#: loop it signs, and the `phase-server-slim-*` paths it attaches. The asset
#: pattern spans the whole line, `artifacts/<dir>/` included, so the directory
#: half of each path -- which repeats the asset name -- is not counted a second
#: time and no line that merely *ends* in something asset-shaped can contribute a
#: name. End-anchoring alone read the tail of any line at all, which a set this
#: gate only ever grows cannot object to: a `#`-prefixed path names an asset the
#: release does not attach, and the desktop whose URL 404s stops being reported.
#: The capture takes the whole filename, `.exe` included, because that suffix is
#: part of the URL a desktop derives; the lazy name plus the trailing group is
#: what tells a binary line from its signature.
SIGN_LOOP = re.compile(r"for triple in((?:\s*\\\s*[\w.-]+)+)\s*;\s*do")
SIGN_TRIPLE = re.compile(r"[\w.-]+")
ASSET_LINE = re.compile(
    rf"^\s*artifacts/[\w.-]+/({SLIM_PREFIX}[\w.-]+?)(\.minisig)?$", re.M)
#: The attached list is the heredoc body, not the whole step, which is the other
#: half of the same cut: the pattern above decides what a line must look like,
#: and this decides where a line has to be to count at all. That step's shell
#: carries prose comments and conditional `echo`s around this list, and none of
#: it attaches an asset. Both halves are needed because adding a name is what the
#: subset check cannot object to -- `attached` is allowed to be a superset -- so a
#: name read from anywhere else reads as published and the desktop whose URL is
#: missing stops being reported. Nothing downstream can separate the two: a
#: commented path names the very asset whose absence was the finding, so the
#: population is narrowed at the read rather than compared afterwards.
ASSET_HEREDOC = re.compile(r"cat <<'EOF'\n(.*?)\n\s*EOF\b", re.S)

#: The preview publish step's two spellings, each narrowed to its own block before
#: any line is read, for the reason the heredoc above is: both sets are compared
#: by subset, so a name picked up from surrounding prose reads as provisioned and
#: silences the very desktop whose absence was the finding. The manifest keys are
#: taken from inside `binaries: {` alone -- the same step's jq also writes a
#: `data:` array of `{name, sha256, url}` objects, and a pattern matching any
#: quoted key followed by a brace would read those as platforms too.
PREVIEW_BINARIES_ARRAY = re.compile(r"binaries=\(\n(.*?)\n\s*\)", re.S)
PREVIEW_ARRAY_LINE = re.compile(
    r"^\s*artifacts/[\w.-]+/phase-server-([\w.-]+?)(\.exe)?$", re.M)
#: Anchored on the `data:` key that follows it, because every entry *inside* the
#: object also ends in `},` -- stopping at the first one would read a single
#: platform's interior as the whole map and stranding the other three would look
#: like a finding rather than like a pattern that stopped early.
PREVIEW_MANIFEST_BLOCK = re.compile(
    r"binaries:\s*\{\n(.*?)\n\s*\},\s*\n\s*data:", re.S)
PREVIEW_MANIFEST_KEY = re.compile(r'^\s*"([\w.-]+)":\s*\{\s*$', re.M)
#: A signature beside every binary: the desktop derives `sig_url` as well as
#: `url`, so a key carrying only one of them resolves an artifact it cannot
#: verify. Matched per key rather than counted, so the report names which.
#: The whole right-hand side, not its first literal: each URL is a concatenation
#: (`"...preview-server/" + $fingerprint + "/phase-server-<triple>"`), so the
#: binary's name is in the second string. A pattern stopping at the first would
#: never see the name it exists to hold the key against.
PREVIEW_MANIFEST_URL = re.compile(r"^\s*(url|sig_url):\s*(.+)$", re.M)
#: The upload path, read from the step that writes it. Every object goes to
#: `$prefix/$name`, so this assignment -- not a spelling of it kept here -- is
#: what fixes the path a manifest entry has to name. A gate holding its own copy
#: of the path passes whenever the manifest agrees with that copy, including when
#: both disagree with the upload the step performs.
PREVIEW_PREFIX_ASSIGN = re.compile(r'^\s*prefix="([^"\n]*)"\s*$', re.M)
#: jq's own binding of a shell variable to the name its program uses. The prefix
#: is shell (`$FINGERPRINT`) and the manifest URL is jq (`$fingerprint`); this
#: flag is the only thing that says the two are one value, so the pairing is read
#: from it rather than inferred from the two spellings looking alike.
PREVIEW_JQ_ARG = re.compile(r'--arg\s+(\w+)\s+"\$(\w+)"')
#: The one comparand this gate cannot derive: the step uploads into the R2 bucket
#: `phase-rs-data`, and the bucket's public hostname is Cloudflare configuration
#: that this repository does not contain. Everything after it -- prefix,
#: fingerprint variable, file name -- is read from the step itself.
PREVIEW_DATA_HOST = "https://data.phase-rs.dev/"
#: A jq concatenation split into the pieces whose identity matters. String
#: contents are captured whole, so whitespace *inside* a literal stays
#: significant -- R2 holds nothing under `preview- server/` -- while whitespace
#: *between* tokens is dropped, so reformatting the expression stays free. A
#: character matching nothing else becomes an `other` token, so a reshaped
#: expression cannot quietly tokenise into the expected sequence.
PREVIEW_JQ_TOKEN = re.compile(r'"((?:[^"\\]|\\.)*)"|\$(\w+)|([()+])|(\S)')


def _jq_tokens(expression: str) -> list[tuple[str, str]]:
    """A jq expression as typed tokens, less one optional trailing comma.

    The manifest writes `url: (...),` ahead of `sig_url:`, so the first of the
    pair carries a separator the second does not.
    """
    tokens: list[tuple[str, str]] = []
    for literal, variable, operator, other in PREVIEW_JQ_TOKEN.findall(
            expression.strip().removesuffix(",")):
        if variable:
            tokens.append(("var", variable))
        elif operator:
            tokens.append(("op", operator))
        elif other:
            tokens.append(("other", other))
        else:
            tokens.append(("str", literal))
    return tokens

#: The desktop platforms a tag publishes and the mapping entries that serve them.
#: Both are expectations, not observations: a change to either is the event this
#: gate reports, so it fails and names which side moved. There is deliberately no
#: expected count for the published slim servers -- that population may exceed the
#: desktop set without stranding a desktop.
PUBLISHED_PLATFORM_COUNT = 4
MAPPED_PLATFORM_COUNT = 4


class Refusal(Exception):
    """A file could not be read the way this gate needs to read it.

    The single authority for every degraded read. No extractor soft-fails by
    returning an empty set, because an empty set passes the subset check.
    """


def _mapping_text() -> str:
    """`native_engine.rs`'s source, or a refusal if it is not there to read."""
    path = ROOT / MAPPING_SOURCE
    if not path.is_file():
        raise Refusal(f"{MAPPING_SOURCE} does not exist; the platform mapping "
                      "is missing, so no published platform can be checked "
                      "against it")
    return _strip_rust_comments(path.read_text(encoding="utf-8"))


#: A raw string's opener, matched whole rather than found by looking at what
#: precedes an `r`. `br#"` and `cr#"` are raw strings whose `r` is preceded by an
#: identifier character, so a guard reading that character alone declines to
#: recognise them: it is the prefixed form of the very token it is guarding. The
#: file this gate reads carries six `br#"` literals today.
RAW_OPEN = re.compile(r'[bc]?r(#*)"')
#: A char literal, against the Reference's production rather than against the
#: forms this scanner happened to remember: an ordinary character, an escape, a
#: `\xNN` byte escape, or a `\u{...}` unicode escape. The two multi-character
#: escapes are why the bare `\\.` spelling was wrong -- it consumes exactly two
#: characters and then demands the closing quote, so `'\u{41}'` matched nothing,
#: its trailing quote was left loose, and that quote paired with the next one two
#: characters along and swallowed a real `"`.
#:
#: The unicode escape's bound is on hex digits, not on characters between the
#: braces: `'\u{1_F_6_0_0}'` compiles, and counting its underscores against the
#: budget refused source rustc accepts. Overlong stays refused -- seven digits is
#: not a char literal, and this gate does not read what the compiler rejects.
CHAR_LIT = re.compile(
    r"b?'(?:\\u\{_*(?:[0-9a-fA-F]_*){1,6}\}|\\x[0-9a-fA-F]{2}|\\.|[^'\\\n])'")
#: A lifetime or loop label: the only other token that opens with a quote and the
#: reason an unrecognised quote cannot simply be assumed to be a literal. It has
#: no closing quote, so it is consumed as itself.
LIFETIME = re.compile(r"'[A-Za-z_][A-Za-z0-9_]*")


def _raw_close(text: str, opener: re.Match[str]) -> int:
    """End of the raw string whose opener this match covers.

    Inside one, `\\` escapes nothing and `//` is data, so a scanner reading it as
    an ordinary literal leaves literal state open at the first `"` the body
    carries and applies every comment rule after that point to code. An
    unterminated opener runs to end of input, which preserves the remainder
    rather than dropping it; it does not compile, so no tree reaches it.
    """
    hashes = opener.group(1)
    end = text.find('"' + hashes, opener.end())
    return len(text) if end < 0 else end + 1 + len(hashes)


def _strip_rust_comments(text: str) -> str:
    """`text` with every Rust comment removed, before any pattern reads it.

    Against the comment grammar whole rather than one of its spellings: `//`
    runs to end of line, `/* */` nests to any depth, and neither opens a comment
    inside a string literal, so `"http://x"` keeps its arm. Newlines survive, so
    the one-arm-per-line shape the callers read is unchanged.

    Nesting is why this scans rather than substitutes: `/\\*.*?\\*/` closes the
    outer comment at the inner `*/` and leaves the remainder of it standing as
    code. A line-oriented rule cannot serve either, which is where this parts
    company with `source_census::code` -- a block comment spans lines, and
    whether the quotes left of a `//` on one line are balanced is a different
    question from whether that `//` stands inside a literal. Literal state is
    tracked instead, so this needs no claim about which blocks carry literals;
    `ALL`'s entries carry none.

    A name written inside a string literal is not a comment and is still read.
    It refuses rather than passing -- as a receiver written twice in an arm
    block, or against `ALL`'s declared length -- because the patterns below are
    not literal-aware either: an escaped quote ends a captured value early, so a
    corrupted triple reports on the published-asset axis instead of refusing.

    Every opener in Rust's literal grammar, because this reads whole source
    files and an enumeration of them is falsified by the member it omits. Each
    omission has the same consequence: a quote that opens no literal is read as
    one, literal state is left open, and every comment rule after that point is
    applied to code. The members are the plain, byte and C-string quotes, the
    raw forms with any hash count, and the char literals -- and each is matched
    whole, at its own start, rather than recognised by the character before it,
    which is a test the prefixed forms fail by construction.

    That enumeration has been wrong five times, so it is no longer trusted to be
    complete: a quote matching no opener refuses rather than being read as code.
    The bound is the point. A member omitted from here now costs a named refusal
    a contributor can act on, instead of a file silently mis-stripped -- which is
    the only failure this gate cannot detect in itself.
    """
    out: list[str] = []
    i, n, depth = 0, len(text), 0
    while i < n:
        pair = text[i:i + 2]
        if depth:
            if pair == "/*":
                depth += 1
                i += 2
            elif pair == "*/":
                depth -= 1
                i += 2
            else:
                if text[i] == "\n":
                    out.append("\n")
                i += 1
        elif pair == "/*":
            depth += 1
            i += 2
        elif pair == "//":
            end = text.find("\n", i)
            if end < 0:
                break
            i = end
        elif ((i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_"))
              and (opener := RAW_OPEN.match(text, i)) is not None):
            close = _raw_close(text, opener)
            out.append(text[i:close])
            i = close
        elif (lit := CHAR_LIT.match(text, i)) is not None:
            out.append(lit.group())
            i = lit.end()
        elif (life := LIFETIME.match(text, i)) is not None:
            out.append(life.group())
            i = life.end()
        elif text[i] == "'":
            raise Refusal(
                f"{MAPPING_SOURCE} carries a quote at offset {i} that opens no "
                f"token this gate knows ({text[i:i + 12]!r}): it is neither a "
                "char literal nor a lifetime. Reading it as code would leave "
                "literal state open and apply every comment rule after it to "
                "code, so this refuses instead of guessing")
        elif text[i] == '"':
            out.append('"')
            i += 1
            while i < n:
                if text[i] == "\\":
                    out.append(text[i:i + 2])
                    i += 2
                    continue
                out.append(text[i])
                i += 1
                if text[i - 1] == '"':
                    break
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def _mapping_block(text: str, pattern: re.Pattern[str], what: str) -> str:
    """One block's code, out of a source every comment was already removed from.

    The single place both arm blocks are read through. Comments are gone before
    this runs, at the read: stripping per block instead left the `//` that opens
    one outside the captured group, so a comment carrying a declaration's shape
    moved the block boundary and its contents were read as code.
    """
    match = pattern.search(text)
    if match is None:
        raise Refusal(f"{MAPPING_SOURCE} has no readable `ServerPlatform::{what}`; "
                      "it was renamed or reformatted, and the mapping cannot be "
                      "read")
    return match.group(1)


def _all_entries(text: str) -> tuple[int, list[str]]:
    """`ALL`'s declared length and its entry names, out of one match.

    Two matches let the length come from somewhere the entries did not, and any
    `[Self; N]` in the file would then serve: that is how this cross-check was
    satisfiable by a sentence of prose.
    """
    match = MAPPING_ALL_BLOCK.search(text)
    if match is None:
        raise Refusal(f"{MAPPING_SOURCE} has no readable `ServerPlatform::ALL`; "
                      "it was renamed or reformatted, and the mapping cannot be "
                      "read")
    return int(match.group(1)), MAPPING_RECEIVER.findall(match.group(2))


def _refuse_phantom_all_entry(declared: int, listed: list[str]) -> None:
    """Refuse when `ALL` reads as more or fewer entries than rustc counts.

    `ALL` is compared to the two arm blocks by name, and a name this gate
    invented is what that comparison cannot object to: a phantom entry makes
    `set(listed)` complete rather than short, so the comparison passes and the
    variant genuinely missing from `ALL` goes unreported -- the way this block
    failed open, where the arm blocks' receiver-against-arm comparison already
    refused. `[Self; N]` is checked by rustc against the entries themselves, so
    it holds against a name no entry produced.
    """
    if len(listed) != declared:
        raise Refusal(
            f"{MAPPING_SOURCE}: ServerPlatform::ALL is declared `[Self; "
            f"{declared}]` but this gate read {len(listed)} entry name(s) in it: "
            f"{listed}. rustc counts the entries itself, so a name here it does "
            "not count came out of something that is not an entry -- text no "
            "comment removal took out, or a name inside a string literal. More "
            "names than rustc counts is the direction that would otherwise pass: "
            "an extra one completes the by-name comparison instead of breaking "
            "it, hiding a variant absent from ALL")


def _arms(block: str, pattern: re.Pattern[str],
          what: str) -> dict[str, tuple[str, ...]]:
    """One block's arms, keyed by the `Self::` receiver each is written against.

    Named, not counted: the receivers present and the arms parsed are compared as
    sets, so a dropped arm is reported by name. A count of either would hold while
    one arm was substituted for another.
    """
    arms = {match[0]: match[1:] for match in pattern.findall(block)}
    receivers = MAPPING_RECEIVER.findall(block)
    unread = sorted(set(receivers) - set(arms))
    repeated = sorted({name for name in receivers if receivers.count(name) > 1})
    if unread or repeated:
        raise Refusal(
            f"{MAPPING_SOURCE}: ServerPlatform::{what} has arms this gate could "
            f"not read. Receivers with no arm it could parse: {unread}. Receivers "
            f"written more than once: {repeated}. An arm rustfmt broke across "
            "lines still carries its receiver, so it is named here rather than "
            "dropped in silence; write it on one line, or teach this gate the "
            "shape it now takes")
    return arms


def _refuse_shared(owners: dict[str, object], what: str, why: str) -> None:
    """Refuse when two variants resolve to one value, naming the variants.

    The collision itself is the finding, so it is reported as the value and the
    variants claiming it. A population size cannot see this: two variants
    collapsing onto one value is exactly the substitution a count admits.
    """
    shared = {
        value: names
        for value in set(owners.values())
        if len(names := sorted(n for n, v in owners.items() if v == value)) > 1
    }
    if shared:
        raise Refusal(f"{MAPPING_SOURCE}: ServerPlatform::{what} resolves "
                      f"{sorted(shared.items(), key=repr)} -- those variants "
                      f"{why}")


def mapped_platforms() -> dict[str, tuple[str, str, str]]:
    """Each `ServerPlatform` variant as `(os, arch, triple)`. The desktop's authority."""
    text = _mapping_text()
    declared, listed = _all_entries(text)
    _refuse_phantom_all_entry(declared, listed)
    pairs = _arms(_mapping_block(text, MAPPING_BLOCK, "os_arch"),
                  MAPPING_ENTRY, "os_arch")
    triples = _arms(_mapping_block(text, MAPPING_TRIPLE_BLOCK, "target_triple"),
                    MAPPING_TRIPLE_ARM, "target_triple")

    if not set(listed) == set(pairs) == set(triples):
        raise Refusal(
            f"{MAPPING_SOURCE}: ServerPlatform::ALL lists {sorted(set(listed))} "
            f"while os_arch covers {sorted(pairs)} and target_triple covers "
            f"{sorted(triples)}. ALL is the only source from_os_arch iterates, so "
            "a variant missing from it resolves for no platform at runtime while "
            "both matches still compile. Add the variant to ALL; its declared "
            "length is not what this gate reads")

    mapped = {name: (*pairs[name], *triples[name]) for name in pairs}

    _refuse_shared({name: (os_name, arch)
                    for name, (os_name, arch, _) in mapped.items()},
                   "os_arch",
                   "claim the same (os, arch), so one of their triples is "
                   "unreachable and the published set would read as covered by a "
                   "mapping that cannot serve it")
    # Both axes, because neither collision implies the other. Two variants can
    # share a bare triple while deriving different names -- exactly one of them
    # `windows` appends `.exe` to only its own -- and two distinct triples can
    # collapse onto one derived name when they differ only by that suffix. A
    # check keyed on either alone reads the other's collision as clean.
    # The derived name first, because it is the narrower report: when both axes
    # collide, the single URL is what a desktop actually requests.
    _refuse_shared({name: slim_asset(os_name, triple)
                    for name, (os_name, _, triple) in mapped.items()},
                   "target_triple's derived asset name",
                   "derive one asset name, so both desktops request a single URL "
                   "and the name the second should have asked for is never "
                   "checked against the release at all")
    _refuse_shared({name: triple
                    for name, (_, _, triple) in mapped.items()},
                   "target_triple",
                   "resolve the same engine triple, so both desktops are served "
                   "one binary built for one of their platforms and the other "
                   "runs an engine compiled for a machine it is not. Differing "
                   "derived names do not separate them: when exactly one of the "
                   "variants is `windows`, its own `.exe` makes the two names "
                   "differ and the asset-name check above sees no collision")
    return mapped


def slim_asset(os_name: str, triple: str) -> str:
    """The release asset a desktop on this platform derives and downloads.

    `native_engine.rs` builds the name from its triple plus `executable_suffix`,
    which is `.exe` under `cfg(target_os = "windows")` -- the same `windows` this
    variant's `os_arch` reports as `std::env::consts::OS`.
    """
    return f"{SLIM_PREFIX}{triple}{'.exe' if os_name == 'windows' else ''}"


def asset_triple(asset: str) -> str:
    """The triple an attached asset name carries, suffix removed."""
    return asset.removeprefix(SLIM_PREFIX).removesuffix(".exe")


def published_platforms() -> set[tuple[str, str]]:
    """The `(os, arch)` pairs `build-shell` publishes a desktop for."""
    path = ROOT / SHELL_RELEASE
    if not path.is_file():
        raise Refusal(f"{SHELL_RELEASE} does not exist; the published platform "
                      "set cannot be read")
    try:
        workflow = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise Refusal(f"{SHELL_RELEASE} is not parseable YAML: {exc}") from exc

    jobs = (workflow or {}).get("jobs") or {}
    job = jobs.get(BUILD_JOB)
    if not isinstance(job, dict):
        raise Refusal(f"{SHELL_RELEASE}: job '{BUILD_JOB}' is absent; the job "
                      "that publishes desktop packages was renamed, and this "
                      "gate no longer knows which platforms ship")

    # The published set is anchored on one job id, so a second job publishing
    # desktops is a population this gate never walks. The axis refusal below is
    # scoped to this job alone and does not reach siblings, so a sibling's matrix
    # is read here in every shape that can produce a platform: a job runs the
    # product of its axes with each `include` entry merged in, so `os` on an axis
    # and `arch` on an entry ship a platform neither spells by itself. Reading
    # only one shape reads a subset of what publishes.
    def _publishes_desktops(name: str, other: object) -> bool:
        strategy = other.get("strategy") if isinstance(other, dict) else None
        if not isinstance(strategy, dict) or "matrix" not in strategy:
            return False
        matrix = strategy["matrix"]
        if not isinstance(matrix, dict):
            raise Refusal(f"{SHELL_RELEASE}: job '{name}' declares a "
                          f"strategy.matrix this gate cannot read ({matrix!r}), "
                          "so whether it publishes desktops is unknown. A job "
                          "that might is not one to pass over")
        axes = set(matrix) - {"include", "exclude"}
        # The same unreadability one level down, and the level a filter would
        # swallow: a string iterates characters and a mapping iterates keys, so
        # `[e for e in include if isinstance(e, dict)]` turns either into an empty
        # list, and an empty list publishes nothing. That is the empty set passing
        # a subset check, which is what this gate refuses everywhere else --
        # including on this same key when it belongs to the job being read.
        include = matrix.get("include")
        if include is not None and not (isinstance(include, list)
                                        and all(isinstance(e, dict) for e in include)):
            raise Refusal(f"{SHELL_RELEASE}: job '{name}' declares a "
                          f"strategy.matrix.include this gate cannot read "
                          f"({include!r}), so whether it publishes desktops is "
                          "unknown. A job that might is not one to pass over")
        entries = include or []
        return {"os", "arch"} <= axes or any(
            {"os", "arch"} <= (axes | e.keys()) for e in entries)

    others = sorted(name for name, other in jobs.items()
                    if name != BUILD_JOB and _publishes_desktops(name, other))
    if others:
        raise Refusal(f"{SHELL_RELEASE}: {others} also declare matrix entries "
                      f"carrying both `os` and `arch`, so they publish desktops "
                      f"too, but this gate reads only '{BUILD_JOB}'. Every "
                      "platform they ship is one no ServerPlatform variant was "
                      "checked against")

    matrix = (job.get("strategy") or {}).get("matrix", {})
    # A matrix runs its product axes as well as its `include` entries, so reading
    # `include` alone reads a subset of what publishes and the platforms an axis
    # contributes strand silently. This gate identifies a platform by an (os,
    # arch) pair on one entry, which a product has no single entry for, so an
    # axis is a shape it cannot read rather than one it reads partially.
    if isinstance(matrix, dict) and (axes := sorted(set(matrix)
                                                    - {"include", "exclude"})):
        raise Refusal(f"{SHELL_RELEASE}: {BUILD_JOB} declares matrix axes "
                      f"{axes} beside `include`; every combination of those runs "
                      "and publishes a desktop too, so the published platform "
                      "set is larger than the `include` list this gate reads")
    include = matrix.get("include") if isinstance(matrix, dict) else None
    if not isinstance(include, list):
        raise Refusal(f"{SHELL_RELEASE}: {BUILD_JOB} declares no "
                      "strategy.matrix.include list; the published platform set "
                      "cannot be read from this shape")

    platforms: set[tuple[str, str]] = set()
    for entry in include:
        if not isinstance(entry, dict) or not {"os", "arch"} <= entry.keys():
            raise Refusal(f"{SHELL_RELEASE}: {BUILD_JOB} matrix has an entry "
                          f"without both `os` and `arch`: {entry!r}. This gate "
                          "identifies a platform by that pair")
        platforms.add((str(entry["os"]), str(entry["arch"])))
    return platforms


def _workflow_job(workflow: str, job_id: str, reads: str) -> dict[str, object]:
    """One job of one workflow: the only place a workflow file is opened."""
    path = ROOT / workflow
    if not path.is_file():
        raise Refusal(f"{workflow} does not exist; {reads} cannot be read")
    try:
        parsed = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise Refusal(f"{workflow} is not parseable YAML: {exc}") from exc

    job = ((parsed or {}).get("jobs") or {}).get(job_id)
    if not isinstance(job, dict):
        raise Refusal(f"{workflow}: job '{job_id}' is absent or unreadable; the "
                      f"job that {reads} depends on was renamed or reshaped, and "
                      "a job this gate cannot find declares nothing rather than "
                      "declaring an empty set")
    return job


def _step_bodies(workflow: str, job_id: str, reads: str) -> dict[str, str]:
    """Each step's shell body, reachable by `id` and by `name`.

    Both, because the two workflows this gate reads identify their steps
    differently: the release job gives its steps ids, while the preview publish
    job names them and gives ids to neither. A duplicate key refuses rather than
    resolving to one of the two bodies -- picking either would read one step's
    shell as another's, and the set that came back would be a real set read off
    the wrong step, which no downstream subset check can tell from the right one.
    """
    steps = _workflow_job(workflow, job_id, reads).get("steps")
    if not isinstance(steps, list):
        raise Refusal(f"{workflow}: job '{job_id}' declares no steps list; the "
                      f"job that {reads} depends on was reshaped, and this gate "
                      "no longer knows which triples ship")
    bodies: dict[str, str] = {}
    for step in steps:
        if not isinstance(step, dict):
            continue
        run = str(step.get("run") or "")
        for key in (step.get("id"), step.get("name")):
            if not isinstance(key, str):
                continue
            if key in bodies and bodies[key] != run:
                raise Refusal(f"{workflow}: job '{job_id}' has two steps "
                              f"answering to '{key}', so {reads} would be read "
                              "off whichever this gate happened to keep")
            bodies[key] = run
    return bodies


def _step_body(bodies: dict[str, str], step: str, workflow: str, job_id: str,
               reads: str) -> str:
    body = bodies.get(step)
    if body is None:
        raise Refusal(f"{workflow}: {job_id} has no step '{step}', so {reads} "
                      "cannot be read")
    return body


def published_assets() -> set[str]:
    """Every `phase-server-slim-*` name the release attaches, signatures included.

    A desktop derives two URLs from its platform, the binary and the signature
    beside it, so each is its own fully-qualified identity here rather than one
    identity carrying a flag. Whichever of them a release fails to attach is then
    the name that comes back missing, instead of a pair that drops out of a count.
    """
    bodies = _step_bodies(RELEASE_WORKFLOW, RELEASE_JOB,
                          "the set of published slim server assets")

    loop = SIGN_LOOP.search(
        _step_body(bodies, SIGN_STEP, RELEASE_WORKFLOW, RELEASE_JOB,
                   "the triples the release signs"))
    if loop is None:
        raise Refusal(f"{RELEASE_WORKFLOW}: {SIGN_STEP} has no readable `for "
                      "triple in ...` loop; the signed set was reshaped, and a "
                      "set this gate cannot read is not an empty one")
    signed = set(SIGN_TRIPLE.findall(loop.group(1)))
    listing = ASSET_HEREDOC.search(
        _step_body(bodies, ASSET_STEP, RELEASE_WORKFLOW, RELEASE_JOB,
                   "the assets the release attaches"))
    if listing is None:
        raise Refusal(f"{RELEASE_WORKFLOW}: {ASSET_STEP} has no readable `cat "
                      "<<'EOF'` asset list; the attached set was reshaped, and a "
                      "set this gate cannot read is not an empty one")
    lines = ASSET_LINE.findall(listing.group(1))
    attached = {f"{asset}{signature}" for asset, signature in lines}

    # Compared on triples, because a triple is all the loop names: it appends the
    # suffix itself, from its own test of which triple is Windows. That test being
    # wrong is self-announcing -- a Windows triple it failed to special-case fails
    # `test -s` and takes the release down with it -- whereas a wrong name in the
    # asset list publishes cleanly and 404s a desktop, which is why the suffix
    # axis is held against the mapping rather than against this loop.
    binaries = {asset_triple(asset) for asset, signature in lines if not signature}
    if signed != binaries:
        raise Refusal(
            f"{RELEASE_WORKFLOW}: {RELEASE_JOB} signs {sorted(signed)} and "
            f"attaches binaries for {sorted(binaries)}. Signed, never attached: "
            f"{sorted(signed - binaries)}. Attached, never signed: "
            f"{sorted(binaries - signed)}. A binary built and signed but never "
            "attached, or attached without ever being built, is published in "
            "neither sense this gate can report")
    return attached


def preview_platforms() -> dict[str, set[str]]:
    """Every triple preview provisioning declares, one set per place it says so.

    `native_engine.rs` resolves a Preview key by looking the running host's
    `target_triple()` up in the signed manifest's `binaries` map, so each of these
    is a place a platform can be dropped while the release half stays green.

    They are kept apart rather than unioned, because a union is satisfied by any
    one spelling and the drift is precisely that they disagree: a triple built and
    signed but never written into the manifest leaves a desktop resolving nothing,
    and a union would still contain it. Held as subsets for the same reason the
    release assets are -- an extra triple strands no desktop, a missing one does.
    """
    build = _workflow_job(PREVIEW_WORKFLOW, PREVIEW_BUILD_JOB,
                          "the preview server binaries built per platform")
    matrix = (build.get("strategy") or {}).get("matrix")
    if not isinstance(matrix, dict):
        raise Refusal(f"{PREVIEW_WORKFLOW}: job '{PREVIEW_BUILD_JOB}' declares a "
                      f"strategy.matrix this gate cannot read ({matrix!r}); the "
                      "set of platforms preview builds cannot be read")
    if axes := sorted(set(matrix) - {"include", "exclude"}):
        raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_BUILD_JOB} declares matrix "
                      f"axes {axes} beside `include`; every combination of those "
                      "builds a preview server too, so the built set is larger "
                      "than the `include` list this gate reads")
    include = matrix.get("include")
    if not isinstance(include, list):
        raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_BUILD_JOB} declares no "
                      "strategy.matrix.include list; the built platform set "
                      "cannot be read from this shape")
    built: set[str] = set()
    for entry in include:
        if not isinstance(entry, dict) or not isinstance(entry.get("triple"), str):
            raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_BUILD_JOB} matrix has an "
                          f"entry with no readable `triple`: {entry!r}. That "
                          "field is the whole identity of a preview binary")
        built.add(entry["triple"])

    publish = _workflow_job(PREVIEW_WORKFLOW, PREVIEW_PUBLISH_JOB,
                            "the preview binaries downloaded, signed and published")
    steps = publish.get("steps")
    if not isinstance(steps, list):
        raise Refusal(f"{PREVIEW_WORKFLOW}: job '{PREVIEW_PUBLISH_JOB}' declares "
                      "no steps list; the job that signs and publishes preview "
                      "servers was reshaped")
    downloaded: set[str] = set()
    for step in steps:
        with_ = step.get("with") if isinstance(step, dict) else None
        name = with_.get("name") if isinstance(with_, dict) else None
        if not isinstance(name, str) or not name.startswith(PREVIEW_ARTIFACT_PREFIX):
            continue
        # The build job uploads under this same prefix as `${{ matrix.triple }}`,
        # which names every platform at once and so identifies none of them. Read
        # as a literal it would contribute one nonsense triple that no mapping
        # holds, turning a superset check into a permanent failure.
        if "${{" in name:
            continue
        downloaded.add(name.removeprefix(PREVIEW_ARTIFACT_PREFIX))

    bodies = _step_bodies(PREVIEW_WORKFLOW, PREVIEW_PUBLISH_JOB,
                          "the preview binaries signed and written to the manifest")
    body = _step_body(bodies, PREVIEW_SIGN_STEP, PREVIEW_WORKFLOW,
                      PREVIEW_PUBLISH_JOB,
                      "the signed binaries and the manifest naming them")
    array = PREVIEW_BINARIES_ARRAY.search(body)
    if array is None:
        raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_SIGN_STEP} has no readable "
                      "`binaries=( ... )` array; the signed set was reshaped, and "
                      "a set this gate cannot read is not an empty one")
    # The array is the single authority for artifact file names: the step uploads
    # `basename "$binary"` taken from it, so the name in the manifest's URL has to
    # be exactly this one -- `.exe` included, which only windows carries.
    artifacts = {triple: f"phase-server-{triple}{exe}"
                 for triple, exe in PREVIEW_ARRAY_LINE.findall(array.group(1))}
    signed = set(artifacts)
    block = PREVIEW_MANIFEST_BLOCK.search(body)
    if block is None:
        raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_SIGN_STEP} has no readable "
                      "`binaries: {` object in the manifest it writes; the keys a "
                      "desktop resolves against cannot be read")
    manifest_keys = PREVIEW_MANIFEST_KEY.findall(block.group(1))
    duplicate_keys = sorted({key for key in manifest_keys
                             if manifest_keys.count(key) > 1})
    if duplicate_keys:
        raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_SIGN_STEP} writes duplicate "
                      f"manifest binary key(s) {duplicate_keys}; jq keeps the "
                      "later value, so every emitted URL pair must have one "
                      "unambiguous key")
    keys = set(manifest_keys)

    assignments = PREVIEW_PREFIX_ASSIGN.findall(body)
    if len(assignments) != 1:
        raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_SIGN_STEP} has no readable "
                      f'`prefix="..."` assignment (found {len(assignments)}); the '
                      "path every preview object is uploaded to must be "
                      "unambiguous, and a path this gate supplies itself would "
                      "check the manifest against nothing")
    prefix = assignments[0]
    shell = re.search(r"\$(\w+)", prefix)
    if shell is None:
        raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_SIGN_STEP} uploads to "
                      f"'{prefix}', which carries no variable; every fingerprint "
                      "would publish over one path, so the manifest could not "
                      "name a per-fingerprint object at all")
    bound = [jq for jq, sh in PREVIEW_JQ_ARG.findall(body)
             if sh == shell.group(1)]
    if len(bound) != 1:
        raise Refusal(f"{PREVIEW_WORKFLOW}: {PREVIEW_SIGN_STEP} binds "
                      f"${shell.group(1)} to {len(bound)} jq argument(s) "
                      f"{sorted(bound)}; exactly one is what lets the manifest's "
                      "variable be checked against the uploaded path")
    head, tail = prefix[:shell.start()], prefix[shell.end():]

    def expected(name: str) -> list[tuple[str, str]]:
        """The one URL that names the object this step uploads for `name`."""
        return [("op", "("), ("str", f"{PREVIEW_DATA_HOST}{head}"),
                ("op", "+"), ("var", bound[0]), ("op", "+"),
                ("str", f"{tail}/{name}"), ("op", ")")]

    # A desktop fetches `url` and `sig_url` verbatim, so an entry has to name the
    # object the step uploaded. Compared as tokens against a URL derived from that
    # step's own prefix, fingerprint binding and array-authorised name, because
    # every weaker rule leaves something free: a substring admits
    # `phase-server-<triple>-old`, a terminal segment admits a moved prefix, and a
    # comparison against a path spelled here admits a prefix that moves in the
    # workflow alone. Per key, so the report names which key rather than a count.
    paired: set[str] = set()
    for key in keys:
        entry = re.search(rf'"{re.escape(key)}":\s*\{{(.*?)\n\s*\}}',
                          block.group(1), re.S)
        if entry is None or key not in artifacts:
            continue
        urls = dict(PREVIEW_MANIFEST_URL.findall(entry.group(1)))
        name = artifacts[key]
        if (_jq_tokens(urls.get("url", "")) == expected(name)
                and _jq_tokens(urls.get("sig_url", ""))
                == expected(f"{name}.minisig")):
            paired.add(key)

    return {"builds": built, "downloads": downloaded, "signs": signed,
            "names in its manifest": keys,
            "gives a signed URL pair in its manifest": paired}


def main() -> int:
    try:
        mapped = mapped_platforms()
        published = published_platforms()
        attached = published_assets()
        provisioned = preview_platforms()
    except Refusal as exc:
        print(f"REFUSED: {exc}", file=sys.stderr)
        return 2

    pairs = {(os_name, arch) for os_name, arch, _ in mapped.values()}
    binaries = {slim_asset(os_name, triple)
                for os_name, _, triple in mapped.values()}
    # Both URLs, because a desktop derives both and either one 404s on its own.
    resolved = {name for binary in binaries
                for name in (binary, f"{binary}.minisig")}
    status = 0

    unmapped = sorted(published - pairs)
    if unmapped:
        print(f"{SHELL_RELEASE}'s {BUILD_JOB} publishes {len(unmapped)} "
              f"platform(s) that {MAPPING_SOURCE} cannot map to an engine "
              "target:", file=sys.stderr)
        for os_name, arch in unmapped:
            print(f"  {os_name}-{arch}", file=sys.stderr)
        print("A desktop published for a platform with no ServerPlatform "
              "variant cannot download an engine. Add the variant, or stop "
              "publishing the platform.", file=sys.stderr)
        status = 1

    unpublished = sorted(resolved - attached)
    if unpublished:
        print(f"{MAPPING_SOURCE}'s ServerPlatform resolves {len(unpublished)} "
              f"asset URL(s) that {RELEASE_WORKFLOW}'s {RELEASE_JOB} job "
              "does not publish:", file=sys.stderr)
        for asset in unpublished:
            print(f"  {asset}", file=sys.stderr)
        print("A desktop resolving one of these asks the release for an asset "
              "that is not there and its download 404s. Publish that exact "
              "name, or stop resolving it.", file=sys.stderr)
        status = 1

    triples = {triple for _, _, triple in mapped.values()}
    for what, declared in sorted(provisioned.items()):
        stranded = sorted(triples - declared)
        if not stranded:
            continue
        print(f"{MAPPING_SOURCE}'s ServerPlatform resolves {len(stranded)} "
              f"triple(s) that {PREVIEW_WORKFLOW} never {what}:", file=sys.stderr)
        for triple in stranded:
            print(f"  {triple}", file=sys.stderr)
        print("A desktop on that platform looks its triple up in the signed "
              "preview manifest and finds no binary, so Try Preview fails there "
              "while every release check stays green. Provision that triple, or "
              "stop resolving it.", file=sys.stderr)
        status = 1

    moved: list[str] = []
    if len(pairs) != MAPPED_PLATFORM_COUNT:
        moved.append(f"{MAPPING_SOURCE}: ServerPlatform::os_arch reads as "
                     f"{len(pairs)} platform(s), expected "
                     f"{MAPPED_PLATFORM_COUNT}: {sorted(pairs)}")
    if len(published) != PUBLISHED_PLATFORM_COUNT:
        moved.append(f"{SHELL_RELEASE}: {BUILD_JOB} publishes {len(published)} "
                     f"platform(s), expected {PUBLISHED_PLATFORM_COUNT}: "
                     f"{sorted(published)}")
    if moved:
        print("MOVED: a population this gate holds an expectation about has "
              "changed. Every check above is by name and ran clean, so this is "
              "the expectation to re-confirm, not a hole:", file=sys.stderr)
        for line in moved:
            print(f"  {line}", file=sys.stderr)
        status = status or 3

    if status:
        return status

    print(f"shell platform mapping OK: {len(published)} published platform(s) "
          f"({', '.join(f'{o}-{a}' for o, a in sorted(published))}) all mapped "
          f"by ServerPlatform ({len(mapped)} variant(s))")
    print(f"engine target triples OK: {len(binaries)} resolved asset(s) "
          f"({', '.join(sorted(binaries))}), each with its signature, all "
          f"published by {RELEASE_WORKFLOW}'s {RELEASE_JOB} job "
          f"({len(attached)} slim asset URL(s))")
    print(f"preview provisioning OK: {len(triples)} engine triple(s) "
          f"({', '.join(sorted(triples))}) each built, downloaded, signed, named "
          f"in {PREVIEW_WORKFLOW}'s manifest, and given a signed URL pair there")
    return 0


if __name__ == "__main__":
    sys.exit(main())
