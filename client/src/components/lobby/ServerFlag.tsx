import type { FlagCode } from "../../services/serverDetection";

/**
 * Tiny inline SVG region flags for the server picker and lobby status chip.
 *
 * Inline SVG rather than emoji (🇺🇸): Windows renders regional-indicator emoji
 * as bare letter pairs ("US") instead of flags, so emoji would look broken for a
 * large share of players. The SVG is simplified — recognizable at chip size, not
 * heraldically exact (the canton shows a star field, not all 50 stars).
 */

function FlagUS({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 39 26"
      className={className}
      role="img"
      aria-label="United States"
    >
      <rect width="39" height="26" fill="#B22234" />
      <g fill="#fff">
        <rect y="2" width="39" height="2" />
        <rect y="6" width="39" height="2" />
        <rect y="10" width="39" height="2" />
        <rect y="14" width="39" height="2" />
        <rect y="18" width="39" height="2" />
        <rect y="22" width="39" height="2" />
      </g>
      <rect width="16" height="14" fill="#3C3B6E" />
      <g fill="#fff">
        <circle cx="3" cy="3" r="0.9" />
        <circle cx="8" cy="3" r="0.9" />
        <circle cx="13" cy="3" r="0.9" />
        <circle cx="5.5" cy="7" r="0.9" />
        <circle cx="10.5" cy="7" r="0.9" />
        <circle cx="3" cy="11" r="0.9" />
        <circle cx="8" cy="11" r="0.9" />
        <circle cx="13" cy="11" r="0.9" />
      </g>
    </svg>
  );
}

export function ServerFlag({
  flag,
  className,
}: {
  flag: FlagCode;
  className?: string;
}) {
  // Exhaustive over FlagCode — a new region's flag is a compile error here.
  switch (flag) {
    case "us":
      return <FlagUS className={className} />;
  }
}
