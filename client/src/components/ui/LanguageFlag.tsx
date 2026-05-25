import type { SupportedLng } from "../../i18n/resources.ts";

/**
 * Tiny inline SVG flags for the language picker and the screen-chrome language
 * indicator. Mirrors the approach in `lobby/ServerFlag` (and shares its rationale):
 * Windows renders regional-indicator emoji (🇬🇧) as bare letter pairs, so emoji
 * would look broken for many players. SVGs are simplified — recognizable at chip
 * size, not heraldically exact. The svg is decorative (aria-hidden); callers
 * provide the accessible language name on the surrounding control.
 *
 * Kept separate from `ServerFlag` on purpose: that component is keyed to the
 * server-region `FlagCode` domain, which is a different concept space from UI
 * language. Flags happen to render both; the two should not be unified.
 */

const VIEW_BOX = "0 0 60 40";

function FlagEN({ className }: { className?: string }) {
  // Simplified Stars and Stripes (US): 13 stripes + blue canton with a star grid.
  // Recognizable at chip size, not heraldically exact (the star count is reduced).
  const stripeH = 40 / 13;
  const whiteStripes = [1, 3, 5, 7, 9, 11];
  const starCols = 5;
  const starRows = 4;
  return (
    <svg viewBox={VIEW_BOX} className={className} aria-hidden="true">
      <rect width="60" height="40" fill="#B22234" />
      {whiteStripes.map((i) => (
        <rect key={i} y={i * stripeH} width="60" height={stripeH} fill="#fff" />
      ))}
      <rect width="24" height={stripeH * 7} fill="#3C3B6E" />
      {Array.from({ length: starCols * starRows }, (_, k) => (
        <circle
          key={k}
          cx={3 + (k % starCols) * 4.5}
          cy={3 + Math.floor(k / starCols) * 5}
          r="1.1"
          fill="#fff"
        />
      ))}
    </svg>
  );
}

function FlagES({ className }: { className?: string }) {
  return (
    <svg viewBox={VIEW_BOX} className={className} aria-hidden="true">
      <rect width="60" height="40" fill="#AA151B" />
      <rect y="10" width="60" height="20" fill="#F1BF00" />
    </svg>
  );
}

function FlagFR({ className }: { className?: string }) {
  return (
    <svg viewBox={VIEW_BOX} className={className} aria-hidden="true">
      <rect width="20" height="40" fill="#0055A4" />
      <rect x="20" width="20" height="40" fill="#fff" />
      <rect x="40" width="20" height="40" fill="#EF4135" />
    </svg>
  );
}

function FlagDE({ className }: { className?: string }) {
  return (
    <svg viewBox={VIEW_BOX} className={className} aria-hidden="true">
      <rect width="60" height="40" fill="#000" />
      <rect y="13.33" width="60" height="13.33" fill="#DD0000" />
      <rect y="26.66" width="60" height="13.34" fill="#FFCE00" />
    </svg>
  );
}

function FlagIT({ className }: { className?: string }) {
  return (
    <svg viewBox={VIEW_BOX} className={className} aria-hidden="true">
      <rect width="20" height="40" fill="#009246" />
      <rect x="20" width="20" height="40" fill="#fff" />
      <rect x="40" width="20" height="40" fill="#CE2B37" />
    </svg>
  );
}

function FlagPT({ className }: { className?: string }) {
  // Simplified: green/red vertical 2:3 split with a small yellow disc at the seam.
  return (
    <svg viewBox={VIEW_BOX} className={className} aria-hidden="true">
      <rect width="60" height="40" fill="#FF0000" />
      <rect width="24" height="40" fill="#046A38" />
      <circle cx="24" cy="20" r="6" fill="#FFE900" stroke="#fff" strokeWidth="1" />
    </svg>
  );
}

function FlagPL({ className }: { className?: string }) {
  // White over red, horizontal halves.
  return (
    <svg viewBox={VIEW_BOX} className={className} aria-hidden="true">
      <rect width="60" height="40" fill="#fff" />
      <rect y="20" width="60" height="20" fill="#DC143C" />
    </svg>
  );
}

export function LanguageFlag({ lng, className }: { lng: SupportedLng; className?: string }) {
  // Exhaustive over SupportedLng — a new language without a flag is a compile error.
  switch (lng) {
    case "en":
      return <FlagEN className={className} />;
    case "es":
      return <FlagES className={className} />;
    case "fr":
      return <FlagFR className={className} />;
    case "de":
      return <FlagDE className={className} />;
    case "it":
      return <FlagIT className={className} />;
    case "pt":
      return <FlagPT className={className} />;
    case "pl":
      return <FlagPL className={className} />;
  }
}
