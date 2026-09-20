#!/usr/bin/env python3
"""Fill locale image maps from another localized printing of the same card.

The Scryfall bulk file is a multi-gigabyte JSON array.  Keep only one card in
memory while collecting the best localized printing for each supported locale
and card identity.  Existing exact-printing entries always win.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import tempfile
from pathlib import Path
from typing import Any, Iterator


IMAGE_SIZES = ("small", "normal", "art_crop")
PLACEHOLDER_URL = "https://errors.scryfall.com/soon.jpg"
MAP_NAME = re.compile(r"^scryfall-images\.(?P<schema>[^.]+)\.(?P<locale>[^.]+)\.json$")


class JsonArrayError(ValueError):
    """The bulk input is not a complete JSON array."""


def _trailing_whitespace(source: Any, buffer: str, position: int, chunk_size: int) -> None:
    """Require only JSON whitespace after the closing array bracket."""
    remainder = buffer[position:]
    if remainder.strip(" \t\r\n"):
        raise JsonArrayError("Trailing input after Scryfall JSON array")
    while True:
        chunk = source.read(chunk_size)
        if not chunk:
            return
        if chunk.strip(" \t\r\n"):
            raise JsonArrayError("Trailing input after Scryfall JSON array")


def stream_json_array(path: Path, *, chunk_size: int = 1 << 20) -> Iterator[Any]:
    """Yield values from one JSON array without materializing the array.

    The separator state is explicit: a comma is required between values, a
    trailing comma is rejected, and bytes after the closing bracket must be
    whitespace only.  Each decoded value may still be as large as one card.
    """
    decoder = json.JSONDecoder()
    with path.open("r", encoding="utf-8") as source:
        buffer = ""
        position = 0
        eof = False
        opened = False
        saw_value = False
        need_value = True

        def fill() -> bool:
            nonlocal buffer, position, eof
            if eof:
                return False
            if position:
                buffer = buffer[position:]
                position = 0
            chunk = source.read(chunk_size)
            if not chunk:
                eof = True
                return False
            buffer += chunk
            return True

        fill()
        while True:
            while True:
                while position < len(buffer) and buffer[position] in " \t\r\n":
                    position += 1
                if position < len(buffer):
                    break
                if not fill():
                    raise JsonArrayError("Incomplete Scryfall JSON array")

            if not opened:
                if buffer[position] != "[":
                    raise JsonArrayError("Expected a Scryfall JSON array")
                position += 1
                opened = True
                need_value = True
                continue

            if need_value:
                if buffer[position] == "]":
                    if saw_value:
                        raise JsonArrayError("Trailing comma in Scryfall JSON array")
                    position += 1
                    _trailing_whitespace(source, buffer, position, chunk_size)
                    return
                if buffer[position] != "{":
                    raise JsonArrayError("Expected a card object in Scryfall JSON array")
                while True:
                    try:
                        value, end = decoder.raw_decode(buffer, position)
                    except json.JSONDecodeError:
                        if not fill():
                            raise JsonArrayError("Incomplete Scryfall JSON array") from None
                        continue
                    if not isinstance(value, dict) or value.get("object") != "card":
                        raise JsonArrayError("Expected a Scryfall card object in JSON array")
                    yield value
                    position = end
                    saw_value = True
                    need_value = False
                    break
                continue

            if buffer[position] == ",":
                position += 1
                need_value = True
                continue
            if buffer[position] == "]":
                position += 1
                _trailing_whitespace(source, buffer, position, chunk_size)
                return
            raise JsonArrayError("Expected ',' or ']' in Scryfall JSON array")


def card_identity(card: dict[str, Any]) -> tuple[tuple[str, ...], str, tuple[str, ...]] | None:
    """Return the identity that must match across printings."""
    oracle_id = card.get("oracle_id")
    layout = card.get("layout")
    faces = card.get("card_faces")
    if not isinstance(layout, str):
        return None
    if isinstance(faces, list):
        names = tuple(face.get("name") for face in faces if isinstance(face, dict))
        if len(names) != len(faces) or not all(isinstance(name, str) for name in names):
            return None
    else:
        name = card.get("name")
        if not isinstance(name, str):
            return None
        names = (name,)
    if layout == "reversible_card":
        if not isinstance(faces, list) or not faces:
            return None
        oracle_ids = tuple(face.get("oracle_id") for face in faces)
        if not all(isinstance(value, str) and value for value in oracle_ids):
            return None
    else:
        if not isinstance(oracle_id, str) or not oracle_id:
            return None
        oracle_ids = (oracle_id,)
    return oracle_ids, layout, names


def exact_faces(card: dict[str, Any]) -> list[dict[str, str]] | None:
    """Extract only URLs present on the localized Scryfall card itself."""
    # Adventure and other split layouts may include card_faces for text while
    # their actual artwork lives at the root. Preserve that one exact URI set.
    root_uris = card.get("image_uris")
    raw_faces = card.get("card_faces")
    faces = [card] if isinstance(root_uris, dict) else raw_faces if isinstance(raw_faces, list) else [card]
    if not faces:
        return None
    result: list[dict[str, str]] = []
    for face in faces:
        if not isinstance(face, dict):
            return None
        uris = face.get("image_uris")
        if not isinstance(uris, dict):
            return None
        values = {size: uris.get(size) for size in IMAGE_SIZES}
        if any(
            not isinstance(value, str) or not value or value == PLACEHOLDER_URL
            for value in values.values()
        ):
            return None
        result.append(values)  # type: ignore[arg-type]
    return result


def _candidate_rank(card: dict[str, Any]) -> tuple[bool, bool, bool, str, str]:
    """Prefer non-promo paper, then the newest printing and its id."""
    games = card.get("games")
    paper = isinstance(games, list) and "paper" in games
    nonpromo = not bool(card.get("promo", False))
    return (nonpromo and paper, nonpromo, paper, str(card.get("released_at", "")), str(card["id"]))


def _maps(output: Path, schema_version: str) -> dict[str, tuple[Path, dict[str, Any]]]:
    maps: dict[str, tuple[Path, dict[str, Any]]] = {}
    pattern = f"scryfall-images.{schema_version}."
    for path in sorted(output.glob(f"{pattern}*.json")):
        match = MAP_NAME.match(path.name)
        if not match or match.group("schema") != schema_version:
            continue
        locale = match.group("locale")
        if locale == "fallback":
            continue
        value = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(value, dict):
            raise ValueError(f"Locale image map is not an object: {path}")
        maps[locale] = (path, value)
    if not maps:
        raise ValueError(f"No locale image maps found in {output}")
    return maps


def _atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as destination:
            json.dump(value, destination, ensure_ascii=False, separators=(",", ":"))
            destination.write("\n")
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def augment_maps(bulk: Path, output: Path, *, schema_version: str = "v2") -> dict[str, int]:
    maps = _maps(output, schema_version)
    locales = tuple(sorted(maps))
    identities: dict[tuple[tuple[str, ...], str, tuple[str, ...]], dict[str, set[str]]] = {}
    best: dict[tuple[str, tuple[tuple[str, ...], str, tuple[str, ...]]], tuple[tuple[bool, bool, bool, str, str], dict[str, Any]]] = {}

    for card in stream_json_array(bulk):
        card_id = card.get("id")
        identity = card_identity(card)
        if not isinstance(card_id, str) or identity is None:
            continue
        language = card.get("lang")
        if language == "en":
            absent = {locale for locale, (_path, image_map) in maps.items() if card_id not in image_map}
            if absent:
                identities.setdefault(identity, {}).setdefault(card_id, set()).update(absent)
            continue
        if language not in maps or card.get("image_status") in {"missing", "placeholder"}:
            continue
        faces = exact_faces(card)
        if faces is None:
            continue
        key = (language, identity)
        rank = _candidate_rank(card)
        current = best.get(key)
        if current is None or rank > current[0]:
            best[key] = (rank, {"id": card_id, "faces": faces})

    added = {locale: 0 for locale in locales}
    for identity, printing_ids in identities.items():
        for printing_id, absent in printing_ids.items():
            for locale in absent:
                candidate = best.get((locale, identity))
                if candidate is None:
                    continue
                maps[locale][1][printing_id] = candidate[1]
                added[locale] += 1

    for path, image_map in maps.values():
        _atomic_json(path, image_map)

    marker = output / f"scryfall-images.{schema_version}.fallback.json"
    _atomic_json(marker, {
        "schema_version": schema_version,
        "algorithm": "same-card-localized-printing-v1",
        "locales": list(locales),
    })
    return added


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bulk", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--schema-version", default="v2")
    args = parser.parse_args()
    try:
        added = augment_maps(args.bulk, args.output, schema_version=args.schema_version)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        parser.error(str(error))
    print(json.dumps({"fallback_entries": added}, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
