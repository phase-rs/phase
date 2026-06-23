#!/usr/bin/env python3
import re
import pathlib

root = pathlib.Path(__file__).resolve().parents[1] / "crates" / "engine"
marker = "CastingPermission::PlayFromExile {"
insert_lines = (
    "cast_cost_raise: None,\n"
    "land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,\n"
)

for path in sorted(root.rglob("*.rs")):
    text = path.read_text(encoding="utf-8")
    if marker not in text:
        continue
    orig = text
    out = []
    i = 0
    while True:
        idx = text.find(marker, i)
        if idx == -1:
            out.append(text[i:])
            break
        out.append(text[i:idx])
        start = idx + len(marker)
        depth = 1
        j = start
        while j < len(text) and depth:
            ch = text[j]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
            j += 1
        block = text[idx:j]
        inner = block[len(marker) : -1]
        if "cast_cost_raise" not in block and ".." not in inner:
            indent = "            "
            for line in block.splitlines():
                m = re.match(r"^(\s*)single_use:", line)
                if m:
                    indent = m.group(1)
                    break
            insert = indent + insert_lines.replace("\n", "\n" + indent)
            if re.search(r"\n\s*single_use:", block):
                block = re.sub(
                    r"(\n\s*single_use: (?:true|false),)",
                    r"\1\n" + insert.rstrip("\n"),
                    block,
                    count=1,
                )
            else:
                block = block[:-1] + insert + block[-1:]
        out.append(block)
        i = j
    new_text = "".join(out)
    if new_text != orig:
        path.write_text(new_text, encoding="utf-8")
        print(path.relative_to(root.parents[1]))
