#!/usr/bin/env bash
# Full-tree guard for the Phase 4 resolution-frame compatibility boundary.
#
# The 39 v1 JSON input keys may exist as quoted keys only in the
# ResolutionStateWire v1 reader, its legacy wire structures/inventory, or test
# fixtures. Runtime resolution work is represented by typed ResolutionFrame
# payloads; identically named typed payload members are not wire keys. The
# frame stack permits top access, a captured adjacent-pair boundary, or
# identity-addressed access. Removing an arbitrary index, or searching the
# vector to decide what to mutate, breaks that authority.
#
# That rule is enforced by the type system, with one residual structural guard
# here. `ResolutionStack::frames` is a `FrameVec` whose backing `Vec` is
# private to `crates/engine/src/types/resolution/frame_vec.rs`, every operation
# that addresses a frame for mutation takes an opaque `FrameSlot` or an opaque
# `ChildStackDepth`, and the removal operations have no wrapper. A positional
# scan still compiles, and the one method that accepts the `usize` it yields is
# `frame_at_offset`, which hands back a frame to read and never a position to
# address. The distinction being drawn is unchanged —
# positional/adjacency-inferred access GUESSES a structural relationship the
# stack does not guarantee, while identity-addressed access asserts one, since
# ids come from a monotonic allocator that never rewinds and a stale id matches
# nothing rather than aliasing a later frame. It mirrors
# `DrawSequenceStack::frame_mut` / `active_if` / `pop`, the same access mode on
# a sibling frame stack.
#
# This script previously grepped for that rule because `frames` was private to
# a 7,000-line module and Rust privacy is module-scoped, so "private" bought
# nothing against the code beside it. Shrinking the module to ~230 lines is what
# made the privacy real. This script carries five structural checks that the
# design itself is intact: `FrameSlot` and `ChildStackDepth` must each be
# mintable only by their documented methods, the two depth-addressed doors
# must each take a `ChildStackDepth`, and `frame_at_offset` must stay the
# module's only bare-`usize` parameter — any of those breaking would reopen
# positional addressing without any compiler error to show for it.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

python3 - "$ROOT" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
resolution_path = Path("crates/engine/src/types/resolution.rs")
game_state_path = Path("crates/engine/src/types/game_state.rs")
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
    # The version discriminator maps numeric wire versions to the typed decode
    # mode before the reader match. Anchor the allowlist to that typed v1 arm,
    # which is the sole branch that consumes legacy resolution fields.
    v1_match = re.search(r"\bGameStateDecodeMode::ResolutionWireV1\s*=>\s*\{", source)
    if v1_match is None:
        raise ValueError("missing ResolutionStateWire v1 reader arm")

    inventory_match = re.search(r"\bfn\s+legacy_resolution_wire_fields\s*\(", source)
    if inventory_match is None:
        raise ValueError("missing legacy resolution key inventory")

    live_roots_match = re.search(
        r"\bconst\s+LEGACY_LIVE_ZONE_CHANGED_EVENT_ROOTS\s*:\s*&\[&str\]\s*=\s*&\[",
        source,
    )
    if live_roots_match is None:
        raise ValueError("missing legacy live ZoneChanged root census")
    live_roots_end = source.find("];", live_roots_match.end())
    if live_roots_end == -1:
        raise ValueError("unterminated legacy live ZoneChanged root census")

    spans = [
        block_span(source, v1_match),
        block_span(source, inventory_match),
        (live_roots_match.start(), live_roots_end + 2),
    ]
    legacy_struct = re.compile(r"\bstruct\s+Legacy\w+Wire\b[^\{]*\{")
    spans.extend(block_span(source, match) for match in legacy_struct.finditer(source))
    return spans


def function_span(source: str, function_name: str) -> tuple[int, int]:
    match = re.search(rf"\bfn\s+{re.escape(function_name)}\s*\(", source)
    if match is None:
        raise ValueError(f"missing {function_name} function")
    return block_span(source, match)


def rust_fn_signatures(source: str) -> list[tuple[str, str, str]]:
    """`(name, params, tail)` for every `fn` in `source`.

    `params` is the parameter list with the contents of every APPLIED group
    elided -- a bracket whose opener directly follows an identifier, i.e.
    `Fn(..)` / `fn(..)` / `Foo[..]`.  A bare tuple type `(usize, u8)` is not
    applied, so its contents are kept.  `tail` is the text between the
    parameter list and the body's `{` (or a `;`): the return type and any
    `where` clause.

    This is a text scan, not a parser: it does not see, for example, a `usize`
    behind a type alias, a method a macro generated, or a raw identifier.
    """
    out: list[tuple[str, str, str]] = []
    n = len(source)
    for match in re.finditer(r"\bfn\s+(\w+)", source):
        i = match.end()
        while i < n and source[i].isspace():
            i += 1
        if i < n and source[i] == "<":  # skip a generic parameter list
            angle = 0
            while i < n:
                if source[i] == "<":
                    angle += 1
                elif source[i] == ">" and source[i - 1] != "-":  # not the `>` of `->`
                    angle -= 1
                    if angle == 0:
                        i += 1
                        break
                i += 1
            while i < n and source[i].isspace():
                i += 1
        if i >= n or source[i] != "(":
            continue
        depth = 0
        elide_from = 0
        params: list[str] = []
        while i < n:
            char = source[i]
            if char in "([{":
                depth += 1
                if depth > 1 and not elide_from and re.match(r"\w", source[i - 1]):
                    elide_from = depth
                i += 1
                continue
            if char in ")]}":
                if elide_from == depth:
                    elide_from = 0
                depth -= 1
                i += 1
                if depth == 0:
                    break
                continue
            if not elide_from:
                params.append(char)
            i += 1
        tail: list[str] = []
        while i < n and source[i] not in "{;":
            tail.append(source[i])
            i += 1
        out.append((match.group(1), "".join(params), "".join(tail)))
    return out


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
    if path == game_state_path:
        # GameStateDecode owns the canonicalization shared by persisted-state
        # and v1 resolution-wire reads. Its firing migration may inspect the
        # v1 child continuation before ResolutionStateWire projects it into a
        # typed resolution frame.
        allowed_spans.append(function_span(source, "migrate_legacy_trigger_firing_carriers"))

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

    # The frame-search and frame-removal scans that used to run here are gone,
    # because the type system now enforces what they policed.
    # `ResolutionStack::frames` is a `FrameVec` whose backing `Vec` is private
    # to `types/resolution/frame_vec.rs`; every operation that addresses a
    # frame for mutation takes an opaque `FrameSlot` or an opaque
    # `ChildStackDepth`, and the removal operations have no wrapper at all. A
    # positional scan still compiles, and the one method that accepts the
    # `usize` it yields is `frame_at_offset`, which hands back a frame to read
    # and never a position to address.
    #
    # That argument holds only while `FrameSlot` and `ChildStackDepth` values
    # come from their minting methods, and while `frame_at_offset` stays the
    # only bare-`usize` parameter in that module. A new
    # `fn ... -> Option<FrameSlot>`, a second mint of a depth, or a new
    # bare-`usize` parameter would each reopen positional addressing with no
    # compiler error to show for it, so that -- and only that -- is what a grep
    # still has to protect: five structural checks on a ~270-line module rather
    # than a search-shape scan over 7,000 lines.
    #
    # `slot_at_captured_depth` stays on this list, and its argument is no
    # longer a bare `usize`: the captured depth has its own opaque type,
    # `ChildStackDepth`, minted only by `FrameVec::capture_depth`. The deferral
    # this comment used to record -- giving that captured depth its own type at
    # all of its origins -- has been taken, so the sanctioned `FrameSlot`
    # minting set is unchanged while the one door it names is closed at the
    # type.
    frame_vec_source = (root / "crates/engine/src/types/resolution/frame_vec.rs").read_text()
    minting = set(re.findall(r"fn\s+(\w+)\s*\([^)]*\)\s*->[^{;]*\bFrameSlot\b", frame_vec_source))
    sanctioned_minting = {"top", "below", "above", "by_id", "slot_at_captured_depth"}
    if minting != sanctioned_minting:
        added = ", ".join(sorted(minting - sanctioned_minting)) or "none"
        missing = ", ".join(sorted(sanctioned_minting - minting)) or "none"
        failures.append(
            "  crates/engine/src/types/resolution/frame_vec.rs: FrameSlot may be "
            f"minted only by {', '.join(sorted(sanctioned_minting))}; "
            f"unexpected: {added}; missing: {missing}"
        )

    # Four more structural checks, on the second opaque value this module
    # mints. (1) `ChildStackDepth` may be minted only by `capture_depth`.
    # (2) `slot_at_captured_depth` and (3) `insert_at_child_boundary` must each
    # take one. (4) `frame_at_offset` must remain the module's only
    # bare-`usize` parameter, since a new one would reopen positional mutation
    # with no compiler error to show for it -- the same hazard the `FrameSlot`
    # minting check exists for, on the parameter axis instead of the return
    # axis.
    #
    # All four read `rust_fn_signatures`, which splits a signature into its
    # top-level parameter list and its return text, so a `usize` sitting after
    # a nested `)` -- `pick: impl Fn(&ResolutionFrame) -> bool, depth: usize`
    # -- is still seen. It is a text scan, not a compiler: it does not see, for
    # example, a `usize` renamed by a type alias or a method a macro generated.
    signatures = rust_fn_signatures(frame_vec_source)

    depth_minting = {
        name for name, _params, tail in signatures
        if re.search(r"\bChildStackDepth\b", tail)
    }
    if depth_minting != {"capture_depth"}:
        added = ", ".join(sorted(depth_minting - {"capture_depth"})) or "none"
        missing = "none" if "capture_depth" in depth_minting else "capture_depth"
        failures.append(
            "  crates/engine/src/types/resolution/frame_vec.rs: ChildStackDepth may "
            f"be minted only by capture_depth; unexpected: {added}; missing: {missing}"
        )

    depth_typed = {
        name for name, params, _tail in signatures
        if re.search(r"\bChildStackDepth\b", params)
    }
    for door in ("slot_at_captured_depth", "insert_at_child_boundary"):
        if door not in depth_typed:
            failures.append(
                "  crates/engine/src/types/resolution/frame_vec.rs: "
                f"{door} must take a ChildStackDepth, not a bare usize"
            )

    usize_params = {
        name for name, params, _tail in signatures
        if re.search(r"\busize\b", params)
    }
    if usize_params != {"frame_at_offset"}:
        added = ", ".join(sorted(usize_params - {"frame_at_offset"})) or "none"
        missing = "none" if "frame_at_offset" in usize_params else "frame_at_offset"
        failures.append(
            "  crates/engine/src/types/resolution/frame_vec.rs: frame_at_offset must "
            "be the only method taking a bare usize (it returns a frame, never a "
            f"slot); unexpected: {added}; missing: {missing}"
        )

if failures:
    print("Resolution-frame boundary guard failed:", file=sys.stderr)
    print("\n".join(failures), file=sys.stderr)
    raise SystemExit(1)

print("Resolution-frame boundary guard PASS")
PY
