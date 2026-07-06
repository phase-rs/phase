export interface Point {
  x: number;
  y: number;
}

export function getArcPath(from: Point, to: Point): string {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  // Guard the degenerate `from === to` case: without `|| 1`, `dist` is 0 and
  // the perpendicular normal (`-dy/dist`, `dx/dist`) becomes `0/0 = NaN`,
  // producing a malformed `M … Q NaN NaN …` path that renders nothing. (The
  // sibling arc helper in `useAttackerArrowPositions` guards this the same way.)
  const dist = Math.sqrt(dx * dx + dy * dy) || 1;
  // Perpendicular offset for curve — proportional to distance
  const offset = Math.min(80, dist * 0.3);
  const nx = -dy / dist;
  const ny = dx / dist;
  const cx = mx + nx * offset;
  const cy = my + ny * offset;
  return `M ${from.x} ${from.y} Q ${cx} ${cy} ${to.x} ${to.y}`;
}
