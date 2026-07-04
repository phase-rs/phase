#!/usr/bin/env python3
"""Render the gittensor contributor-impact infographic from contrib-stats.json.

Emits a self-contained dark-mode SVG (stdlib only, mirroring
plot-coverage-history.py) and rasterizes it to PNG with the first available of
rsvg-convert / magick / inkscape. Designed as a weekly/monthly share to the
gittensor subnet owners.

Layout follows the repo's dataviz method:
  * headline metrics are STAT TILES (each metric normalized to its own gittensor-
    vs-non-gittensor share bar) — never one axis across different-scale measures;
  * the area distribution is the one true bar chart (all values in code-lines),
    drawn as 100%-stacked share bars so "where does each cohort work" reads
    directly.
Two CVD-validated categorical hues carry identity (blue = gittensor, aqua =
non-gittensor); every mark is direct-labeled since a static image has no hover.

Usage:
    python3 scripts/gittensor/contrib_infographic.py [contrib-stats.json] [out.png]
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

# ── Palette (dark; validated: worst adjacent CVD ΔE 15.7, both ≥3:1 on surface) ─
SURFACE = "#1a1a19"
PLANE = "#0d0d0d"
INK = "#ffffff"
INK2 = "#c3c2b7"
MUTED = "#898781"
GRID = "#2c2c2a"
BORDER = "rgba(255,255,255,0.10)"
GITTENSOR = "#3987e5"   # categorical slot 1 (blue)
NONGT = "#199e70"       # categorical slot 2 (aqua)
FONT = "system-ui, -apple-system, &quot;Segoe UI&quot;, sans-serif"

W = 1200
MARGIN = 60


def esc(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def commas(n: int) -> str:
    return f"{n:,}"


def compact(n: int) -> str:
    """Abbreviated form (1.7M, 30.3k) so large LOC values fit a stat tile."""
    a = abs(n)
    if a < 1000:
        return str(n)
    if a < 1_000_000:
        return f"{n / 1e3:.1f}k"
    return f"{n / 1e6:.1f}M"


class Svg:
    def __init__(self) -> None:
        self.parts: list[str] = []

    def rect(self, x, y, w, h, fill, rx=0, stroke=None, sw=1):
        s = f' stroke="{stroke}" stroke-width="{sw}"' if stroke else ""
        self.parts.append(
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" '
            f'rx="{rx}" fill="{fill}"{s}/>')

    def text(self, x, y, s, size, fill=INK, weight=400, anchor="start",
             tabular=False):
        tn = ' font-variant-numeric="tabular-nums"' if tabular else ""
        self.parts.append(
            f'<text x="{x:.1f}" y="{y:.1f}" font-family="{FONT}" '
            f'font-size="{size}" font-weight="{weight}" fill="{fill}" '
            f'text-anchor="{anchor}"{tn}>{esc(s)}</text>')

    def svg(self, h: int) -> str:
        body = "\n".join(self.parts)
        return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" '
                f'height="{h}" viewBox="0 0 {W} {h}">\n'
                f'<rect width="{W}" height="{h}" fill="{PLANE}"/>\n{body}\n</svg>\n')


def share_bar(svg: Svg, x, y, w, h, g_val, n_val):
    """A single 100%-stacked bar: gittensor (blue) | 2px gap | non-gittensor (aqua)."""
    total = g_val + n_val
    if total <= 0:
        svg.rect(x, y, w, h, GRID, rx=h / 2)
        return 0.0
    frac = g_val / total
    gap = 2
    gw = max(0.0, (w - gap) * frac)
    nw = max(0.0, (w - gap) * (1 - frac))
    svg.rect(x, y, w, h, GRID, rx=h / 2)                       # track
    if gw > 0:
        svg.rect(x, y, gw, h, GITTENSOR, rx=h / 2)
    if nw > 0:
        svg.rect(x + gw + gap, y, nw, h, NONGT, rx=h / 2)
    return frac


def stat_tile(svg: Svg, x, y, w, h, label, g_val, n_val, fmt=commas):
    svg.rect(x, y, w, h, SURFACE, rx=10, stroke=BORDER)
    pad = 18
    svg.text(x + pad, y + 26, label.upper(), 12, MUTED, weight=600)
    svg.text(x + pad, y + 62, fmt(g_val), 30, GITTENSOR, weight=700, tabular=True)
    svg.text(x + pad, y + 86, "gittensor", 12, INK2)
    svg.text(x + w - pad, y + 62, fmt(n_val), 30, NONGT, weight=700,
             anchor="end", tabular=True)
    svg.text(x + w - pad, y + 86, "non-gittensor", 12, INK2, anchor="end")
    frac = share_bar(svg, x + pad, y + h - 34, w - 2 * pad, 10, g_val, n_val)
    svg.text(x + pad, y + h - 12, f"{frac * 100:.0f}% gittensor", 11, MUTED)


def render(d: dict) -> str:
    g = d["groups"]["gittensor"]
    n = d["groups"]["nongittensor"]
    svg = Svg()

    # Header
    svg.text(MARGIN, 58, "Gittensor Contributor Impact", 34, INK, weight=700)
    win = d.get("window", {})
    span = win.get("since") or (win.get("first_commit") or "")[:10] or "all-time"
    until = win.get("until") or "now"
    sub = (f'{d.get("repo", "")}  ·  {span} → {until}  ·  '
           f'{commas(d.get("total_commits", 0))} commits  ·  '
           f'generated {d.get("generated_at", "")[:10]}')
    svg.text(MARGIN, 86, sub, 14, MUTED)

    # Legend
    lx = W - MARGIN - 320
    svg.rect(lx, 44, 12, 12, GITTENSOR, rx=3)
    svg.text(lx + 20, 55, "gittensor", 13, INK2)
    svg.rect(lx + 120, 44, 12, 12, NONGT, rx=3)
    svg.text(lx + 140, 55, "non-gittensor", 13, INK2)

    # Stat tiles
    tiles = [
        ("Contributors", g["contributors"], n["contributors"], commas),
        ("Merged PRs", g["prs"], n["prs"], commas),
        ("Code churn", g["code_churn"], n["code_churn"], compact),
        ("Code net LOC", g["code_additions"] - g["code_deletions"],
         n["code_additions"] - n["code_deletions"], compact),
    ]
    ty = 112
    th = 150
    gap = 24
    tw = (W - 2 * MARGIN - gap * (len(tiles) - 1)) / len(tiles)
    for i, (label, gv, nv, fmt) in enumerate(tiles):
        stat_tile(svg, MARGIN + i * (tw + gap), ty, tw, th, label, gv, nv, fmt)

    # Area distribution — 100%-stacked share bars, sorted by gittensor share
    ay = ty + th + 48
    svg.text(MARGIN, ay, "Where each cohort works", 20, INK, weight=700)
    svg.text(MARGIN, ay + 22, "Share of code-only lines changed per repo area "
             "(excludes generated & card data)", 13, MUTED)
    areas = sorted(set(g["areas"]) | set(n["areas"]),
                   key=lambda a: -(g["areas"].get(a, 0) + n["areas"].get(a, 0)))
    label_col = 130
    pct_col = 128
    bar_x = MARGIN + label_col
    bar_w = W - MARGIN - bar_x - pct_col
    row_h = 30
    by = ay + 48
    for a in areas[:9]:
        gv, nv = g["areas"].get(a, 0), n["areas"].get(a, 0)
        cy = by + row_h / 2
        svg.text(bar_x - 14, cy + 4, a, 13, INK2, anchor="end")
        frac = share_bar(svg, bar_x, by + 5, bar_w, row_h - 12, gv, nv)
        svg.text(W - MARGIN, cy + 4, f"{frac * 100:.0f}%  ·  {commas(gv + nv)} ln",
                 12, MUTED, anchor="end", tabular=True)
        by += row_h

    # Footer
    fy = by + 34
    svg.rect(MARGIN, fy - 20, W - 2 * MARGIN, 1, GRID)
    ref = f'{d.get("ref", "")} @ {d.get("ref_sha", "")[:10]}'
    note = "" if not d.get("pr_limit_hit") else "  ·  ⚠ PR limit hit"
    svg.text(MARGIN, fy, f"ref {ref}{note}", 12, MUTED)
    unclassified = len(d.get("unclassified", []))
    if unclassified:
        svg.text(W - MARGIN, fy, f"{unclassified} unclassified contributor(s) — "
                 f"run contrib_stats.py --triage", 12, MUTED, anchor="end")
    return svg.svg(int(fy + 30))


def rasterize(svg_path: Path, out: Path) -> bool:
    if rc := shutil.which("rsvg-convert"):
        subprocess.run([rc, "-o", str(out), str(svg_path)], check=True)
    elif mg := shutil.which("magick"):
        subprocess.run([mg, str(svg_path), str(out)], check=True)
    elif ik := shutil.which("inkscape"):
        subprocess.run([ik, str(svg_path), "--export-filename", str(out)], check=True)
    else:
        return False
    return True


def main() -> None:
    src = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("contrib-stats.json")
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("contrib-infographic.png")
    data = json.loads(src.read_text())
    svg = render(data)

    svg_path = out.with_suffix(".svg")
    svg_path.write_text(svg)
    if out.suffix.lower() == ".svg":
        print(f"Wrote {svg_path}")
        return
    if rasterize(svg_path, out):
        print(f"Wrote {out} (and {svg_path})")
    else:
        print(f"Wrote {svg_path} (no rasterizer found for {out.suffix})")


if __name__ == "__main__":
    main()
