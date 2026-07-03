#!/usr/bin/env python3
"""Render a compact Gittensor impact SVG for README embedding.

The SVG is pure text and vector shapes so GitHub can render it reliably. It is
authored at 1200x700 and displayed at 600x350 for a crisp 2x card.

Usage:
    python3 scripts/gittensor/contrib_compact_svg.py stats.json out.svg --theme dark
    python3 scripts/gittensor/contrib_compact_svg.py stats.json out.svg --theme light
"""

from __future__ import annotations

import argparse
import html
import json
from pathlib import Path

W = 1200
H = 700

THEMES = {
    "dark": {
        "bg": "#101112",
        "panel": "#181a1b",
        "panel2": "#141516",
        "border": "#303234",
        "track": "#2a2d2f",
        "text": "#f5f2ef",
        "muted": "#aba7a2",
        "muted2": "#7d7974",
        "orange": "#ff6a00",
        "orange2": "#ff8a2b",
        "gray": "#85898b",
        "gray_text": "#c4c7c8",
    },
    "light": {
        "bg": "#fbfaf8",
        "panel": "#ffffff",
        "panel2": "#ffffff",
        "border": "#ded9d2",
        "track": "#e4dfd8",
        "text": "#1f2323",
        "muted": "#616363",
        "muted2": "#7b7771",
        "orange": "#f26a00",
        "orange2": "#c75300",
        "gray": "#7a8083",
        "gray_text": "#4f5557",
    },
}


def pct(a: int, b: int) -> float:
    total = a + b
    return 0.0 if total <= 0 else a / total * 100


def compact(n: int) -> str:
    sign = "-" if n < 0 else ""
    n = abs(n)
    if n >= 1_000_000:
        return f"{sign}{n / 1_000_000:.1f}M"
    if n >= 1000:
        return f"{sign}{n / 1000:.1f}k"
    return f"{sign}{n:,}"


class Svg:
    def __init__(self, colors: dict[str, str]) -> None:
        self.colors = colors
        self.parts: list[str] = []

    def rect(self, x, y, w, h, fill, rx=0, stroke=None, sw=1):
        stroke_attr = f' stroke="{stroke}" stroke-width="{sw}"' if stroke else ""
        self.parts.append(
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" '
            f'rx="{rx}" fill="{fill}"{stroke_attr}/>'
        )

    def text(self, x, y, s, size, fill=None, weight=400, anchor="start"):
        fill = fill or self.colors["text"]
        self.parts.append(
            f'<text x="{x:.1f}" y="{y:.1f}" '
            f'font-family="Inter, ui-sans-serif, system-ui, -apple-system, Segoe UI, sans-serif" '
            f'font-size="{size}" font-weight="{weight}" fill="{fill}" '
            f'text-anchor="{anchor}">{html.escape(str(s))}</text>'
        )

    def line(self, x1, y1, x2, y2, stroke):
        self.parts.append(
            f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" '
            f'stroke="{stroke}" stroke-width="1"/>'
        )

    def share_bar(self, x, y, w, h, g_val, n_val):
        c = self.colors
        self.rect(x, y, w, h, c["track"], h / 2)
        total = g_val + n_val
        if total <= 0:
            return
        gap = 5
        g_w = (w - gap) * g_val / total
        if g_w > 0:
            self.rect(x, y, g_w, h, c["orange"], h / 2)
        n_w = w - g_w - gap
        if n_w > 0:
            self.rect(x + g_w + gap, y, n_w, h, c["gray"], h / 2)

    def svg(self) -> str:
        return (
            f'<svg xmlns="http://www.w3.org/2000/svg" width="600" height="350" '
            f'viewBox="0 0 {W} {H}" role="img">\n'
            + "\n".join(self.parts)
            + "\n</svg>\n"
        )


def render(data: dict, theme: str) -> str:
    c = THEMES[theme]
    svg = Svg(c)
    g = data["groups"]["gittensor"]
    n = data["groups"]["nongittensor"]
    repo = data.get("repo", "phase-rs/phase")
    win = data.get("window", {})
    span = win.get("since") or (win.get("first_commit") or "")[:10] or "all-time"
    until = win.get("until") or "now"

    pr_total = g["prs"] + n["prs"]
    pr_share = pct(g["prs"], n["prs"])
    net_g = g["code_additions"] - g["code_deletions"]
    net_n = n["code_additions"] - n["code_deletions"]
    loc_share = pct(net_g, net_n)
    contributor_share = pct(g["contributors"], n["contributors"])

    svg.rect(0, 0, W, H, c["bg"])
    svg.text(48, 58, "phase.rs is part of the Gittensor community", 28, c["text"], 760)
    svg.text(
        48,
        92,
        "Rewarding meaningful merged contributions to tracked open-source repositories",
        19,
        c["muted"],
        450,
    )
    svg.text(1152, 58, repo, 18, c["muted"], 700, "end")
    svg.text(1152, 92, f"{span} -> {until}", 16, c["muted2"], 450, "end")
    svg.line(48, 126, 1152, 126, c["border"])

    svg.rect(48, 154, 1104, 278, c["panel"], 22, c["border"])
    svg.text(88, 202, "MERGED PR THROUGHPUT", 16, c["muted"], 760)
    svg.text(86, 304, f"{pr_share:.0f}%", 104, c["orange"], 870)
    svg.text(92, 342, "Gittensor share", 20, c["muted"], 520)

    copy_x = 456
    svg.text(copy_x, 246, f'{g["prs"]:,} of {pr_total:,} PRs', 46, c["text"], 830)
    svg.text(copy_x + 2, 286, "merged by Gittensor contributors", 25, c["text"], 650)
    svg.text(copy_x + 2, 322, "in this reporting window", 20, c["muted"], 450)
    svg.share_bar(copy_x + 2, 352, 650, 24, g["prs"], n["prs"])
    svg.text(copy_x + 2, 394, f'Gittensor {g["prs"]:,}', 18, c["orange2"], 740)
    svg.text(1106, 394, f'Non-Gittensor {n["prs"]:,}', 18, c["gray_text"], 740, "end")

    cards = [
        (
            "Net code LOC",
            loc_share,
            compact(net_g),
            compact(net_n),
            "additions minus deletions",
        ),
        (
            "Active contributors",
            contributor_share,
            f'{g["contributors"]:,}',
            f'{n["contributors"]:,}',
            "unique contributors",
        ),
    ]
    card_y = 464
    card_w = 536
    card_h = 138
    gap = 32
    for i, (label, share, g_val, n_val, sub) in enumerate(cards):
        x = 48 + i * (card_w + gap)
        svg.rect(x, card_y, card_w, card_h, c["panel2"], 18, c["border"])
        svg.text(x + 28, card_y + 36, label, 22, c["text"], 720)
        svg.text(x + 28, card_y + 88, f"{share:.0f}%", 54, c["orange"], 850)
        svg.text(x + 168, card_y + 72, "Gittensor share", 20, c["text"], 650)
        svg.text(x + 168, card_y + 101, sub, 17, c["muted"], 450)
        svg.text(x + card_w - 28, card_y + 36, f"{g_val} / {n_val}", 17, c["muted"], 680, "end")
        svg.share_bar(x + 28, card_y + 112, card_w - 56, 12, share, 100 - share)

    svg.text(
        48,
        654,
        "Orange = Gittensor contributors · Gray = non-Gittensor contributors · Source: Gittensor PR API + repository git history",
        15,
        c["muted2"],
        450,
    )
    return svg.svg()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stats", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--theme", choices=sorted(THEMES), default="dark")
    args = parser.parse_args()

    data = json.loads(args.stats.read_text())
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(render(data, args.theme))
    print(f"Wrote {args.out}")


if __name__ == "__main__":
    main()
