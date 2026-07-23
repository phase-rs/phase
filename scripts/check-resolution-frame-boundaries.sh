#!/usr/bin/env bash
# Full-tree guard for the Phase 4 resolution-frame compatibility boundary.
#
# The 39 v1 JSON input keys may exist as quoted keys only in the
# ResolutionStateWire v1 reader, its legacy wire structures/inventory, or test
# fixtures. Runtime resolution work is represented by typed ResolutionFrame
# payloads; identically named typed payload members are not wire keys. The
# frame stack also permits only top access or a captured adjacent-pair boundary:
# searching the vector for a frame or removing an arbitrary index breaks that
# authority.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

python3 - "$ROOT" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
resolution_path = Path("crates/engine/src/types/resolution.rs")
legacy_keys = {
    "pending_continuation",
    "search_continuation_attach_host",
    "pending_choose_zone_trigger_context",
    "pending_repeat_iteration",
    "pending_repeat_until",
    "pending_repeated_optional_payment",
    "optional_cost_payments_this_resolution",
    "pending_change_zone_iteration",
    "devour_eligible_snapshot",
    "pending_batch_deliveries",
    "pending_mill_deliveries",
    "pending_counter_moves",
    "pending_counter_removals",
    "pending_counter_additions",
    "pending_copy_token_resolution",
    "pending_each_player_copy_chosen",
    "pending_choose_one_of",
    "pending_vote_ballot_iteration",
    "pending_per_player_zone_choice",
    "pending_per_category_zone_choice",
    "pending_optional_effect",
    "pending_optional_trigger_event",
    "pending_optional_trigger_match_count",
    "pending_coin_flip",
    "pending_proliferate_actions",
    "draw_sequences",
    "pending_multi_draw",
    "pending_connive_reentry",
    "pending_life_total_assignment",
    "pending_spell_resolution",
    "pending_mutate_merge",
    "post_replacement_drains",
    "post_replacement_effect",
    "post_replacement_resolved_effect",
    "post_replacement_continuation",
    "post_replacement_source",
    "post_replacement_applied",
    "post_replacement_event_source",
    "post_replacement_event_target",
}
# This v1 input was carried inside PendingContinuation rather than a top-level
# GameState field, so it cannot be emitted by the full-state serializer.
input_only_legacy_keys = {"search_continuation_attach_host"}
serialized_legacy_keys = legacy_keys - input_only_legacy_keys


def closing_brace(source: str, open_brace: int) -> int:
    depth = 0
    index = open_brace
    while index < len(source):
        if source.startswith("//", index):
            newline = source.find("\n", index)
            index = len(source) if newline == -1 else newline
            continue
        if source.startswith("/*", index):
            comment_depth = 1
            index += 2
            while comment_depth and index < len(source):
                if source.startswith("/*", index):
                    comment_depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    comment_depth -= 1
                    index += 2
                else:
                    index += 1
            continue

        raw_string = (
            re.match(r'r(#+)?"', source[index:]) if source[index] == "r" else None
        )
        if raw_string is not None:
            hashes = raw_string.group(1) or ""
            close = source.find(f'"{hashes}', index + raw_string.end())
            if close == -1:
                raise ValueError("unterminated Rust raw string")
            index = close + len(hashes) + 1
            continue
        if source[index] == '"':
            index += 1
            while index < len(source):
                if source[index] == "\\":
                    index += 2
                elif source[index] == '"':
                    index += 1
                    break
                else:
                    index += 1
            continue
        if (
            source[index] == "'"
            and index + 2 < len(source)
            and source[index + 2] == "'"
        ):
            index += 3
            continue
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    raise ValueError("unbalanced Rust braces")


def block_span(source: str, match: re.Match[str]) -> tuple[int, int]:
    open_brace = match.end() - 1 if source[match.end() - 1] == "{" else source.find("{", match.end())
    if open_brace == -1:
        raise ValueError("missing Rust block brace")
    return (match.start(), closing_brace(source, open_brace))


def cfg_test_module_spans(source: str) -> list[tuple[int, int]]:
    pattern = re.compile(r"#\[cfg\(test\)\]\s*(?:#\[[^\]]+\]\s*)*mod\s+\w+\s*\{")
    return [block_span(source, match) for match in pattern.finditer(source)]


def string_literals(source: str):
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            newline = source.find("\n", index)
            index = len(source) if newline == -1 else newline
            continue
        if source.startswith("/*", index):
            comment_depth = 1
            index += 2
            while comment_depth and index < len(source):
                if source.startswith("/*", index):
                    comment_depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    comment_depth -= 1
                    index += 2
                else:
                    index += 1
            continue

        raw_string = (
            re.match(r'r(#+)?"', source[index:]) if source[index] == "r" else None
        )
        if raw_string is not None:
            hashes = raw_string.group(1) or ""
            content_start = index + raw_string.end()
            close = source.find(f'"{hashes}', content_start)
            if close == -1:
                raise ValueError("unterminated Rust raw string")
            yield index, source[content_start:close]
            index = close + len(hashes) + 1
            continue
        if source[index] == '"':
            start = index
            index += 1
            content_start = index
            while index < len(source):
                if source[index] == "\\":
                    index += 2
                elif source[index] == '"':
                    yield start, source[content_start:index]
                    index += 1
                    break
                else:
                    index += 1
            else:
                raise ValueError("unterminated Rust string")
            continue
        if (
            source[index] == "'"
            and index + 2 < len(source)
            and source[index + 2] == "'"
        ):
            index += 3
            continue
        index += 1
def in_any_span(offset: int, spans: list[tuple[int, int]]) -> bool:
    return any(start <= offset < end for start, end in spans)


def line_number(source: str, offset: int) -> int:
    return source.count("\n", 0, offset) + 1


def legacy_allowlist_spans(source: str) -> list[tuple[int, int]]:
    v1_match = re.search(r"\bLEGACY_RESOLUTION_STATE_WIRE_VERSION\s*=>\s*\{", source)
    if v1_match is None:
        raise ValueError("missing ResolutionStateWire v1 reader arm")

    inventory_match = re.search(r"\bfn\s+legacy_resolution_wire_fields\s*\(", source)
    if inventory_match is None:
        raise ValueError("missing legacy resolution key inventory")

    spans = [block_span(source, v1_match), block_span(source, inventory_match)]
    legacy_struct = re.compile(r"\bstruct\s+Legacy\w+Wire\b[^\{]*\{")
    spans.extend(block_span(source, match) for match in legacy_struct.finditer(source))
    return spans


def function_span(source: str, function_name: str) -> tuple[int, int]:
    match = re.search(rf"\bfn\s+{re.escape(function_name)}\s*\(", source)
    if match is None:
        raise ValueError(f"missing {function_name} function")
    return block_span(source, match)


def fail(failures: list[str], path: Path, source: str, offset: int, message: str) -> None:
    failures.append(f"  {path}:{line_number(source, offset)}: {message}")


# Pure-Python scan (ripgrep is not installed on CI runners).
legacy_key_pattern = re.compile('"(?:' + "|".join(sorted(legacy_keys)) + ')"')
files = [
    path.relative_to(root).as_posix()
    for path in sorted((root / "crates/engine/src").rglob("*.rs"))
    if legacy_key_pattern.search(path.read_text())
]

failures: list[str] = []
resolution_source = (root / resolution_path).read_text()
if str(resolution_path) not in files:
    files.append(str(resolution_path))
allowed_legacy_spans = legacy_allowlist_spans(resolution_source)
serializer_start, serializer_end = function_span(resolution_source, "to_value")
serializer = resolution_source[serializer_start:serializer_end]
inventory_start, inventory_end = function_span(
    resolution_source, "legacy_resolution_wire_fields"
)
inventory = resolution_source[inventory_start:inventory_end]
inventory_keys = [
    key for _, key in string_literals(inventory) if key in serialized_legacy_keys
]
if (
    set(inventory_keys) != serialized_legacy_keys
    or len(inventory_keys) != len(serialized_legacy_keys)
):
    failures.append(
        "  crates/engine/src/types/resolution.rs: legacy_resolution_wire_fields "
        "must enumerate each of the 38 top-level v1 fields exactly once"
    )

remover_start, remover_end = function_span(resolution_source, "remove_resolution_wire_fields")
remover = resolution_source[remover_start:remover_end]
serialized_state = serializer.find("serde_json::to_value(&self.state)")
removal = serializer.find("remove_resolution_wire_fields(object);")
frames = serializer.find('"resolution_frames"')
if (
    serialized_state == -1
    or removal == -1
    or frames == -1
    or not serialized_state < removal < frames
    or "for field in legacy_resolution_wire_fields()" not in remover
    or "object.remove(*field);" not in remover
):
    failures.append(
        "  crates/engine/src/types/resolution.rs: ResolutionStateWire::to_value "
        "must remove the complete legacy-key census after GameState serialization "
        "and before it emits v2 frames"
    )

for file_name in files:
    path = Path(file_name)
    if path.name.endswith("_tests.rs"):
        continue

    source = (root / path).read_text()
    test_spans = cfg_test_module_spans(source)
    allowed_spans = test_spans[:]
    if path == resolution_path:
        allowed_spans.extend(allowed_legacy_spans)

    for offset, key in string_literals(source):
        if key not in legacy_keys or in_any_span(offset, allowed_spans):
            continue
        message = f"legacy resolution input key {key!r} is outside the v1 reader or a fixture"
        if path == resolution_path and serializer_start <= offset < serializer_end:
            message = (
                "ResolutionStateWire::to_value emits legacy resolution key "
                f"{key!r}"
            )
        fail(
            failures,
            path,
            source,
            offset,
            message,
        )

    if path != resolution_path:
        continue

    production_spans = test_spans
    remove_pattern = re.compile(
        r"\b(?:self\s*\.\s*)?frames\s*\.\s*"
        r"(?:remove|swap_remove|retain|drain|truncate|clear)\s*\("
    )
    search_pattern = re.compile(
        r"\b(?:self\s*\.\s*)?frames\s*\.\s*iter(?:_mut)?\s*\(\s*\)"
        r"(?:\s*\.\s*\w+\s*\([^;{}]*\))*?"
        r"\s*\.\s*(?:position|rposition|find|find_map|any|next|nth)\s*\(",
        re.DOTALL,
    )
    for pattern, message in [
        (remove_pattern, "arbitrary ResolutionStack frame removal is forbidden; use a checked top-only API"),
        (search_pattern, "generic ResolutionStack frame search is forbidden; use top or adjacent-pair access"),
    ]:
        for match in pattern.finditer(source):
            if not in_any_span(match.start(), production_spans):
                fail(failures, path, source, match.start(), message)

if failures:
    print("Resolution-frame boundary guard failed:", file=sys.stderr)
    print("\n".join(failures), file=sys.stderr)
    raise SystemExit(1)

print("Resolution-frame boundary guard PASS")
PY
