#!/usr/bin/env python3
"""Build the Simplified-Chinese card-content sidecar from the magic-cards-zhs export.

The sidecar (`card-data.zh.json`) is the Chinese sibling of the per-locale
sidecars `oracle-gen --sidecar-dir` emits from MTGJSON `foreignData`. It has the
same shape — a map of lowercased English card-face name to
`{name?, oracle_text?, type_line?}` — because the frontend overlay
(`ensureCardLocale` in `client/src/services/engineRuntime.ts`) looks localized
faces up by exactly that key and falls back per-field to English.

WHY THIS SOURCE AND NOT MTGJSON
-------------------------------
MTGJSON `foreignData` also carries Simplified Chinese, but it is *card-level*:
a multi-face card exposes one combined `"A // B"` name and one combined `text`
with no reliable per-face split, which is why `collect_localized` in
`oracle-gen` can only emit single-faced entries. The magic-cards-zhs export is
*oracle-level* — one record per card face, each carrying the English name
alongside its translation — so it needs no join at all and no combined-name
heuristic. Coverage is also materially deeper (see DATA QUALITY below).

THE SOURCE IS JSONL, AND IT IS NOT QUITE VALID JSON
---------------------------------------------------
`zhs_oracle.json` is one JSON object per line, and the exporter double-escapes
escaped quotes: a literal `"` inside a value is written as the three raw bytes
`5c 5c 22` (`\\"`) where JSON requires `5c 22` (`\"`). A strict parser reads the
`\\` as an escaped backslash and then hits a bare `"` that terminates the string
early — measured at 5,283 of 39,963 lines in the 2026-10-11 export (13%).
Repairing that single byte sequence makes 100% of the file parse.

Newlines are double-escaped the same way: a rules-text line break is the four
raw bytes `5c 5c 5c 5c 6e` (`\\\\n`), which JSON decodes to the two-character
string `\\n`. Real Oracle text separates abilities with a REAL newline (verified
against MTGJSON `AtomicCards`), so the parsed value is rewritten `\\n` -> `\\n`.

Both rewrites are done on the raw bytes, before decoding, so the repair cannot
be confused by a legitimate `\n` that the exporter happened to emit correctly.

DATA QUALITY
------------
Every record carries maturity stages (`name_stage`, `text_stage`) and a
`text_source`. Names and type lines are complete (39,963/39,963); rules text is
present on 36,932. Records whose text is missing still carry a translated name
and type line, so they are kept — the overlay is per-field, leaving just the
rules text in English for those cards rather than dropping the whole entry.

Usage:
    magic_cards_zhs.py --source <zhs_oracle.json> --output <card-data.zh.json>
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import sys
import tempfile
from pathlib import Path
from typing import Any, Iterable, Iterator

# The exporter's double-escaped quote bug, as raw bytes: `\` `\` `"`.
_DOUBLE_ESCAPED_QUOTE = b'\\\\"'
_SINGLE_ESCAPED_QUOTE = b'\\"'

# `\` `\` `\` `\` `n` (a JSON-escaped `\\n`) -> `\` `\` `n` (a JSON-escaped
# newline). Applied post-decode as a plain string replace, where the needle is
# the two-character sequence backslash-backslash + `n`.
_DOUBLE_ESCAPED_NEWLINE = "\\\\n"
_REAL_NEWLINE = "\n"

# The engine keys localized faces by lowercased English face name. A name
# containing the multi-face separator can never be one of those keys, and
# splitting it would let a back face collide with the standalone card that
# shares its name — the exact trap `insert_face` guards in `oracle-gen`. The
# 2026-10-11 export has 3 such records out of 39,963; they are skipped.
_MULTIFACE_SEPARATOR = "//"


class ExportError(RuntimeError):
    """The source export could not be turned into a sidecar."""


def repair_export_bytes(raw: bytes) -> bytes:
    """Undo the exporter's double-escaped quotes.

    Order matters: this must run on raw bytes BEFORE decoding. Collapsing
    `\\\\"` -> `\\"` first means the decoder sees a correctly escaped quote, while
    a decode-first implementation would already have terminated the string.
    """
    return raw.replace(_DOUBLE_ESCAPED_QUOTE, _SINGLE_ESCAPED_QUOTE)


def iter_records(text: str) -> Iterator[dict[str, Any]]:
    """Yield each JSON object in a JSONL document, skipping blank lines.

    Decodes object-by-object with `raw_decode` rather than splitting on newlines,
    because a repaired record's *values* can contain real newlines once the
    JSONL line boundaries stop being a safe proxy for record boundaries. A line
    that still fails to parse is a hard error: silently skipping it would ship a
    sidecar with unexplained holes.
    """
    decoder = json.JSONDecoder()
    index, length = 0, len(text)
    while index < length:
        while index < length and text[index] in " \t\r\n":
            index += 1
        if index >= length:
            return
        try:
            record, index = decoder.raw_decode(text, index)
        except json.JSONDecodeError as error:  # pragma: no cover - defensive
            line = text.count("\n", 0, index) + 1
            raise ExportError(
                f"unparsable record near line {line}: {error}"
            ) from error
        if not isinstance(record, dict):
            raise ExportError(f"expected a JSON object, got {type(record).__name__}")
        yield record


def normalize_text(value: Any) -> str:
    """Collapse the exporter's double-escaped newlines into real ones.

    Also normalises CRLF, which would otherwise render as a visible blank line
    inside a card's rules text.
    """
    if not isinstance(value, str):
        return ""
    return value.replace(_DOUBLE_ESCAPED_NEWLINE, _REAL_NEWLINE).replace("\r\n", "\n").strip()


def clean(value: Any) -> str | None:
    """A trimmed string, or None when there is nothing to ship.

    Returning None rather than "" is load-bearing: the sidecar omits absent
    fields so the frontend's per-field `?? result.x` fallback sees `undefined`
    and keeps the English text. An empty string would render as a blank.
    """
    if not isinstance(value, str):
        return None
    trimmed = value.strip()
    return trimmed or None


def build_sidecar(records: Iterable[dict[str, Any]]) -> tuple[dict[str, dict[str, str]], dict[str, int]]:
    """Fold oracle-level records into the name-keyed sidecar map.

    Duplicate English names are real: 39,963 records cover 37,099 distinct names
    because a card reprinted across sets appears once per printing. The overlay
    is name-keyed and therefore printing-blind, so exactly one record may win.
    The winner is chosen deterministically — most complete text first, then the
    richest record — so two runs over the same export cannot produce different
    files. (MTGJSON's `or_insert`-style first-wins has the same printing-blind
    outcome but makes the winner depend on set-file iteration order.)
    """
    sidecar: dict[str, dict[str, str]] = {}
    stats: collections.Counter[str] = collections.Counter()
    # Rank of the record currently occupying each key, for the tie-break above.
    ranks: dict[str, tuple[int, int, int]] = {}

    for record in records:
        stats["records"] += 1

        english_name = clean(record.get("name"))
        if english_name is None:
            stats["skipped_missing_name"] += 1
            continue
        if _MULTIFACE_SEPARATOR in english_name:
            stats["skipped_multiface"] += 1
            continue

        translated_name = clean(record.get("translated_name"))
        translated_type = clean(record.get("translated_type"))
        translated_text = clean(normalize_text(record.get("translated_text")))

        if translated_name is None and translated_type is None and translated_text is None:
            stats["skipped_untranslated"] += 1
            continue

        entry: dict[str, str] = {}
        if translated_name is not None:
            entry["name"] = translated_name
        if translated_text is not None:
            entry["oracle_text"] = translated_text
        if translated_type is not None:
            entry["type_line"] = translated_type

        key = english_name.lower()
        # More complete text wins; then the longer text; then the longer name.
        # All three are properties of the record, never of iteration order.
        rank = (
            1 if translated_text is not None else 0,
            len(translated_text or ""),
            len(translated_name or ""),
        )
        if key in ranks and ranks[key] >= rank:
            stats["skipped_duplicate"] += 1
            continue
        if key in ranks:
            stats["replaced_duplicate"] += 1
        ranks[key] = rank
        sidecar[key] = entry

    return sidecar, dict(stats)


def write_sidecar_atomically(path: Path, sidecar: dict[str, dict[str, str]]) -> None:
    """Write the sidecar via tmp + rename.

    `gen-card-data.sh` and Tilt's card-data resource can run concurrently; a
    reader must never observe a half-written file. Sorted keys keep the output
    byte-stable across runs, which is what makes it reviewable in a diff.
    """
    payload = json.dumps(sidecar, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, tmp_name = tempfile.mkstemp(dir=str(path.parent), prefix=path.name + ".", suffix=".tmp")
    tmp_path = Path(tmp_name)
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as stream:
            # Literal CJK, never \uXXXX escapes: the catalogs' encoding gate
            # applies the same rule to the i18n JSON, and escaped output would
            # make every diff of this file unreadable.
            stream.write(payload)
            stream.write("\n")
        os.replace(tmp_path, path)
    except BaseException:
        tmp_path.unlink(missing_ok=True)
        raise


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--source", required=True, type=Path, help="path to the extracted zhs_oracle.json")
    parser.add_argument("--output", required=True, type=Path, help="path to write card-data.zh.json")
    args = parser.parse_args(argv)

    if not args.source.is_file():
        print(f"error: source not found: {args.source}", file=sys.stderr)
        return 1

    raw = args.source.read_bytes()
    if not raw.strip():
        print(f"error: source is empty: {args.source}", file=sys.stderr)
        return 1

    repaired = repair_export_bytes(raw)
    text = repaired.decode("utf-8")
    sidecar, stats = build_sidecar(iter_records(text))

    if not sidecar:
        print("error: produced an empty sidecar; refusing to publish", file=sys.stderr)
        return 1

    write_sidecar_atomically(args.output, sidecar)

    with_text = sum(1 for entry in sidecar.values() if "oracle_text" in entry)
    print(f"  records read        {stats.get('records', 0):>8,}")
    print(f"  skipped (multiface) {stats.get('skipped_multiface', 0):>8,}")
    print(f"  skipped (no name)   {stats.get('skipped_missing_name', 0):>8,}")
    print(f"  skipped (untrans.)  {stats.get('skipped_untranslated', 0):>8,}")
    print(f"  duplicate printings {stats.get('skipped_duplicate', 0):>8,}")
    print(f"  entries written     {len(sidecar):>8,}  ({with_text:,} with rules text)")
    print(f"  -> {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
