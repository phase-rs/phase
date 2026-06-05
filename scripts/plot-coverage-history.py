#!/usr/bin/env python3
"""Render a coverage-over-time chart from coverage-history.sh output.

Reads the JSON data file produced by scripts/coverage-history.sh — a list of
{created, sha, supported, delta, ...} records — and draws a dual-axis chart:

  * a line (+ markers) of the absolute supported-card count   [left axis]
  * bars of each build's net delta, green up / red down       [right axis]

The chart is emitted as SVG using only the Python standard library (no
matplotlib), mirroring the repo's existing SVG-badge approach. If the requested
output is a raster format (.png/.jpg) it is rasterized with the first available
of rsvg-convert / magick / inkscape; otherwise the SVG is written as-is.

Usage:
    plot-coverage-history.py <data.json> [out.png]
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# ── Canvas geometry ──────────────────────────────────────────────────────────
W, H = 1280, 640
M_TOP, M_BOTTOM, M_LEFT, M_RIGHT = 70, 90, 90, 90
PLOT_W = W - M_LEFT - M_RIGHT
PLOT_H = H - M_TOP - M_BOTTOM

BG = "#0d1117"
FG = "#c9d1d9"
GRID = "#21262d"
LINE = "#58a6ff"
POS = "#3fb950"
NEG = "#f85149"
AXIS = "#8b949e"


def esc(s: str) -> str:
    return (
        s.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def nice_bounds(lo: float, hi: float, pad_frac: float = 0.08) -> tuple[float, float]:
    """Pad a [lo, hi] range so the data doesn't touch the plot edges."""
    if hi == lo:
        hi = lo + 1
    span = hi - lo
    return lo - span * pad_frac, hi + span * pad_frac


def build_svg(rows: list[dict]) -> str:
    n = len(rows)
    supported = [r["supported"] for r in rows]
    deltas = [r["delta"] for r in rows]

    s_lo, s_hi = nice_bounds(min(supported), max(supported))
    # Delta axis is symmetric around zero so the baseline sits mid-plot.
    d_max = max((abs(d) for d in deltas), default=1) or 1
    d_lo, d_hi = -d_max * 1.15, d_max * 1.15

    def x_at(i: int) -> float:
        if n == 1:
            return M_LEFT + PLOT_W / 2
        return M_LEFT + PLOT_W * i / (n - 1)

    def y_supported(v: float) -> float:
        return M_TOP + PLOT_H * (s_hi - v) / (s_hi - s_lo)

    def y_delta(v: float) -> float:
        return M_TOP + PLOT_H * (d_hi - v) / (d_hi - d_lo)

    parts: list[str] = []
    parts.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" '
        f'viewBox="0 0 {W} {H}" font-family="ui-sans-serif,Segoe UI,Helvetica,Arial,sans-serif">'
    )
    parts.append(f'<rect width="{W}" height="{H}" fill="{BG}"/>')

    # Title + subtitle.
    first_date = rows[0]["created"][:10]
    last_date = rows[-1]["created"][:10]
    parts.append(
        f'<text x="{M_LEFT}" y="32" fill="{FG}" font-size="22" font-weight="700">'
        f"Card-support coverage over time</text>"
    )
    parts.append(
        f'<text x="{M_LEFT}" y="52" fill="{AXIS}" font-size="13">'
        f"main CI runs  ·  {esc(first_date)} → {esc(last_date)}  ·  {n} builds</text>"
    )

    # Horizontal gridlines + left (supported) axis ticks.
    ticks = 6
    for t in range(ticks + 1):
        val = s_lo + (s_hi - s_lo) * t / ticks
        y = y_supported(val)
        parts.append(
            f'<line x1="{M_LEFT}" y1="{y:.1f}" x2="{M_LEFT + PLOT_W}" y2="{y:.1f}" '
            f'stroke="{GRID}" stroke-width="1"/>'
        )
        parts.append(
            f'<text x="{M_LEFT - 10}" y="{y + 4:.1f}" fill="{LINE}" font-size="12" '
            f'text-anchor="end">{int(round(val)):,}</text>'
        )

    # Right (delta) axis ticks, including the emphasized zero baseline.
    for t in range(ticks + 1):
        val = d_lo + (d_hi - d_lo) * t / ticks
        y = y_delta(val)
        parts.append(
            f'<text x="{M_LEFT + PLOT_W + 10}" y="{y + 4:.1f}" fill="{AXIS}" font-size="12" '
            f'text-anchor="start">{val:+.0f}</text>'
        )
    y0 = y_delta(0)
    parts.append(
        f'<line x1="{M_LEFT}" y1="{y0:.1f}" x2="{M_LEFT + PLOT_W}" y2="{y0:.1f}" '
        f'stroke="{AXIS}" stroke-width="1" stroke-dasharray="4 3"/>'
    )

    # Delta bars.
    bar_w = max(2.0, min(18.0, PLOT_W / max(n, 1) * 0.6))
    for i, d in enumerate(deltas):
        x = x_at(i)
        y = y_delta(d)
        top = min(y, y0)
        height = abs(y - y0)
        color = POS if d >= 0 else NEG
        parts.append(
            f'<rect x="{x - bar_w / 2:.1f}" y="{top:.1f}" width="{bar_w:.1f}" '
            f'height="{height:.1f}" fill="{color}" opacity="0.65"/>'
        )

    # Supported line + markers.
    pts = " ".join(f"{x_at(i):.1f},{y_supported(v):.1f}" for i, v in enumerate(supported))
    parts.append(f'<polyline points="{pts}" fill="none" stroke="{LINE}" stroke-width="2.5"/>')
    for i, v in enumerate(supported):
        parts.append(
            f'<circle cx="{x_at(i):.1f}" cy="{y_supported(v):.1f}" r="3" fill="{LINE}"/>'
        )

    # Annotate the single largest drop (the "coverage honesty" moment).
    min_i = min(range(n), key=lambda i: deltas[i])
    if deltas[min_i] < 0:
        x = x_at(min_i)
        y = y_supported(supported[min_i])
        label = f'{rows[min_i]["sha"][:9]}  net {deltas[min_i]:+,}'
        anchor = "start" if x < M_LEFT + PLOT_W * 0.6 else "end"
        dx = 8 if anchor == "start" else -8
        parts.append(
            f'<circle cx="{x:.1f}" cy="{y:.1f}" r="5" fill="none" stroke="{NEG}" stroke-width="2"/>'
        )
        parts.append(
            f'<text x="{x + dx:.1f}" y="{y - 12:.1f}" fill="{NEG}" font-size="12" '
            f'font-weight="600" text-anchor="{anchor}">{esc(label)}</text>'
        )

    # X-axis date labels (thinned to ~10 to avoid overlap).
    step = max(1, n // 10)
    for i in range(0, n, step):
        x = x_at(i)
        parts.append(
            f'<text x="{x:.1f}" y="{M_TOP + PLOT_H + 22:.1f}" fill="{AXIS}" font-size="11" '
            f'text-anchor="end" transform="rotate(-40 {x:.1f} {M_TOP + PLOT_H + 22:.1f})">'
            f'{esc(rows[i]["created"][:10])}</text>'
        )

    # Plot frame + axis legends.
    parts.append(
        f'<rect x="{M_LEFT}" y="{M_TOP}" width="{PLOT_W}" height="{PLOT_H}" '
        f'fill="none" stroke="{AXIS}" stroke-width="1"/>'
    )
    parts.append(
        f'<text x="{M_LEFT}" y="{H - 24}" fill="{LINE}" font-size="12">'
        f"● supported cards (left)</text>"
    )
    parts.append(
        f'<text x="{M_LEFT + 200}" y="{H - 24}" fill="{POS}" font-size="12">'
        f"■ net delta &#8805; 0</text>"
    )
    parts.append(
        f'<text x="{M_LEFT + 330}" y="{H - 24}" fill="{NEG}" font-size="12">'
        f"■ net delta &lt; 0 (right)</text>"
    )

    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def rasterize(svg: str, out: Path) -> None:
    """Write `out`; rasterize from SVG when out is not an .svg file."""
    if out.suffix.lower() == ".svg":
        out.write_text(svg)
        print(f"Wrote {out}", file=sys.stderr)
        return

    with tempfile.NamedTemporaryFile("w", suffix=".svg", delete=False) as tf:
        tf.write(svg)
        svg_path = Path(tf.name)

    try:
        if shutil.which("rsvg-convert"):
            cmd = ["rsvg-convert", "-o", str(out), str(svg_path)]
        elif shutil.which("magick"):
            cmd = ["magick", str(svg_path), str(out)]
        elif shutil.which("inkscape"):
            cmd = ["inkscape", str(svg_path), "--export-filename", str(out)]
        else:
            fallback = out.with_suffix(".svg")
            fallback.write_text(svg)
            print(
                f"No SVG rasterizer found (rsvg-convert/magick/inkscape); "
                f"wrote {fallback} instead.",
                file=sys.stderr,
            )
            return
        subprocess.run(cmd, check=True)
        print(f"Wrote {out}", file=sys.stderr)
    finally:
        svg_path.unlink(missing_ok=True)


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    data_path = Path(argv[1])
    out_path = Path(argv[2]) if len(argv) > 2 else data_path.with_suffix(".png")

    rows = json.loads(data_path.read_text())
    if not rows:
        print("No data points to plot.", file=sys.stderr)
        return 1
    rows.sort(key=lambda r: r["created"])

    rasterize(build_svg(rows), out_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
