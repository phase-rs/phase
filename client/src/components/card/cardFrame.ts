const FRAME_COLORS: Record<string, string> = {
  W: "#F5E6C8",
  U: "#0E68AB",
  B: "#2B2B2B",
  R: "#D32029",
  G: "#00733E",
};

function getFrameColor(colors: string[]): string {
  if (colors.length === 0) return "#8E8E8E";
  if (colors.length >= 2) return "#C9B037";
  return FRAME_COLORS[colors[0]] ?? "#8E8E8E";
}

function hexToRgb(hex: string): [number, number, number] {
  const h = hex.replace("#", "");
  return [
    parseInt(h.slice(0, 2), 16),
    parseInt(h.slice(2, 4), 16),
    parseInt(h.slice(4, 6), 16),
  ];
}

function blend(color: string, target: string, amount: number): string {
  const [r1, g1, b1] = hexToRgb(color);
  const [r2, g2, b2] = hexToRgb(target);
  const r = Math.round(r1 + (r2 - r1) * amount);
  const g = Math.round(g1 + (g2 - g1) * amount);
  const b = Math.round(b1 + (b2 - b1) * amount);
  return `rgb(${r}, ${g}, ${b})`;
}

export function getBevelBorderStyle(
  colors: string[],
  borderWidth = 3,
): React.CSSProperties {
  const base = getFrameColor(colors);
  return {
    borderWidth,
    borderStyle: "solid",
    borderTopColor: blend(base, "#ffffff", 0.4),
    borderLeftColor: blend(base, "#ffffff", 0.25),
    borderBottomColor: blend(base, "#000000", 0.4),
    borderRightColor: blend(base, "#000000", 0.25),
  };
}
