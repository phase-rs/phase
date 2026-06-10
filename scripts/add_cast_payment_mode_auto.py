#!/usr/bin/env python3
"""Add payment_mode: CastPaymentMode::Auto to GameAction cast struct literals (issue #572)."""
from __future__ import annotations

import re
import sys
from pathlib import Path

VARIANTS = [
    "CastSpell",
    "CastSpellAsSneak",
    "CastSpellAsWebSlinging",
    "CastSpellForFree",
    "CastSpellAsMiracle",
    "CastSpellAsMadness",
]

INSERT_LINE = "            payment_mode: CastPaymentMode::Auto,"


def is_match_pattern_arm(text: str, close_brace_idx: int) -> bool:
    """True when this struct block is a match-arm pattern (… ) =>), not a value literal."""
    after = text[close_brace_idx + 1 : close_brace_idx + 80]
    return bool(re.search(r"\)\s*=>", after))


def patch_file(path: Path) -> int:
    text = path.read_text()
    original = text
    patches = 0

    for variant in VARIANTS:
        needle = f"GameAction::{variant} {{"
        start = 0
        while True:
            i = text.find(needle, start)
            if i < 0:
                break
            brace = text.find("{", i)
            depth = 0
            k = brace
            while k < len(text):
                ch = text[k]
                if ch == "{":
                    depth += 1
                elif ch == "}":
                    depth -= 1
                    if depth == 0:
                        block = text[i : k + 1]
                        if ".." in block:
                            start = k + 1
                            break
                        if "payment_mode" in block:
                            start = k + 1
                            break
                        if is_match_pattern_arm(text, k):
                            start = k + 1
                            break
                        inner_close = k
                        # Insert before closing brace with same indent as last field
                        insert_at = inner_close
                        # find last non-whitespace before }
                        j = insert_at - 1
                        while j > brace and text[j].isspace():
                            j -= 1
                        line_start = text.rfind("\n", i, j) + 1
                        indent = re.match(r"[ \t]*", text[line_start:j + 1]).group(0)
                        if not indent:
                            indent = "            "
                        line = f"\n{indent}payment_mode: CastPaymentMode::Auto,"
                        text = text[:insert_at] + line + text[insert_at:]
                        patches += 1
                        start = insert_at + len(line) + 1
                        break
                k += 1
            else:
                break

    if text != original:
        path.write_text(text)
    return patches


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    total = 0
    for path in sorted(root.rglob("*.rs")):
        if "target" in path.parts:
            continue
        n = patch_file(path)
        if n:
            print(f"{path.relative_to(root)}: {n}")
            total += n
    print(f"Total patches: {total}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
