export interface Point {
  x: number;
  y: number;
}

export function getArcPath(from: Point, to: Point): string {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const dist = Math.sqrt(dx * dx + dy * dy);
  // Coincident endpoints (a self-target, or two anchors overlapping mid-layout):
  // the perpendicular unit vector `-dy/dist` is 0/0 = NaN, which propagates into
  // the control point (NaN * 0 is still NaN) and yields an invalid `d` attribute.
  // There is no arc to draw, so emit a degenerate line instead.
  if (dist === 0) {
    return `M ${from.x} ${from.y} L ${to.x} ${to.y}`;
  }
  // Perpendicular offset for curve — proportional to distance
  const offset = Math.min(80, dist * 0.3);
  const nx = -dy / dist;
  const ny = dx / dist;
  const cx = mx + nx * offset;
  const cy = my + ny * offset;
  return `M ${from.x} ${from.y} Q ${cx} ${cy} ${to.x} ${to.y}`;
}
