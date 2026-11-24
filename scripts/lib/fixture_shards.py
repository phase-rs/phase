"""Shard format of the integration card fixture (`integration_cards/`).

One JSON Lines file per first letter of the lowercase card key, one complete
record per line, sorted by key:

    {"key":"abandon hope","card":{"name":"Abandon Hope",...}}

Keys that start with an ASCII letter go to `<letter>.jsonl`, other ASCII
(digits, symbols) to `_digits.jsonl`, non-ASCII to `_other.jsonl`. Each `card`
value is the compact serialization (`separators=(",", ":")`, `ensure_ascii=False`)
the fixture has always used. Different cards land on different lines, so
concurrent PRs merge without conflict. The Rust loader is
`phase_engine::database::integration_fixture`.
"""
from __future__ import annotations

import json
from pathlib import Path

SHARD_SUFFIX = ".jsonl"


def shard_name(key: str) -> str:
    first = key[:1]
    if "a" <= first <= "z":
        return first + SHARD_SUFFIX
    if first.isascii():
        return "_digits" + SHARD_SUFFIX
    return "_other" + SHARD_SUFFIX


def render_record(key: str, card: object) -> str:
    key_json = json.dumps(key, ensure_ascii=False)
    card_json = json.dumps(card, separators=(",", ":"), ensure_ascii=False)
    return '{"key":' + key_json + ',"card":' + card_json + "}"


def render_shards(fixture: dict[str, object]) -> dict[str, bytes]:
    """Map shard file name -> exact file bytes. Empty shards are omitted."""
    lines: dict[str, list[str]] = {}
    for key in sorted(fixture):
        lines.setdefault(shard_name(key), []).append(render_record(key, fixture[key]))
    return {
        name: ("\n".join(rows) + "\n").encode("utf-8") for name, rows in sorted(lines.items())
    }


def read_shards(directory: Path) -> dict[str, bytes]:
    """Every `*.jsonl` file in `directory` (other files are not shards), by name."""
    return {
        path.name: path.read_bytes()
        for path in sorted(directory.iterdir())
        if path.is_file() and path.name.endswith(SHARD_SUFFIX)
    }


def parse_shards(shards: dict[str, bytes]) -> dict[str, object]:
    """Inverse of `render_shards` (does not validate the layout)."""
    merged: dict[str, object] = {}
    for name in sorted(shards):
        for line in shards[name].decode("utf-8").split("\n"):
            if not line:
                continue
            record = json.loads(line)
            if record["key"] in merged:
                raise ValueError(f"{name}: duplicate key {record['key']!r}")
            merged[record["key"]] = record["card"]
    return dict(sorted(merged.items()))


def write_shards(directory: Path, shards: dict[str, bytes]) -> None:
    """Replace the shard set in `directory` (stale shard files are removed)."""
    directory.mkdir(parents=True, exist_ok=True)
    for path in directory.iterdir():
        if path.is_file() and path.name.endswith(SHARD_SUFFIX) and path.name not in shards:
            path.unlink()
    for name, data in shards.items():
        (directory / name).write_bytes(data)
