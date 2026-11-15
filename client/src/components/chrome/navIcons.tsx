/**
 * Custom section icons (white-on-transparent PNG art from the design system) for
 * the app-shell navigation — rail + tab bar. An <img> can't inherit currentColor,
 * so callers carry the active/idle state via opacity (the ember tick + label
 * supply the color cue). For larger, color-tintable contexts prefer an SVG glyph.
 */
interface NavIconProps {
  className?: string;
}

function sectionIcon(file: string, label: string) {
  function SectionIcon({ className }: NavIconProps) {
    return (
      <img
        src={`/icons/sections/${file}.png`}
        alt=""
        aria-hidden="true"
        draggable={false}
        className={className ?? "h-7 w-7"}
      />
    );
  }
  SectionIcon.displayName = `SectionIcon(${label})`;
  return SectionIcon;
}

export const HomeIcon = sectionIcon("home", "Home");
export const PlayNavIcon = sectionIcon("play", "Play");
export const OnlineNavIcon = sectionIcon("online", "Online");
export const DraftNavIcon = sectionIcon("draft", "Draft");
export const DecksNavIcon = sectionIcon("decks", "Decks");

/**
 * Trophy glyph for the Tournaments destination.
 *
 * An inline SVG rather than `sectionIcon("tournament", …)` because there is no
 * `/icons/sections/tournament.png` in `client/public/` — the design-system set
 * ships `coverage, decks, draft, home, metagame, online, play, resume,
 * settings` and nothing else. A dangling `<img src>` would be invisible to
 * every test (happy-dom never fetches an image) and broken only in production.
 * Follows `SparkleIcon.tsx`, an SVG glyph already rendered side by side with
 * these PNGs in both `Rail.tsx` and `TabBar.tsx` under the same `className`
 * opacity treatment.
 */
export function TournamentNavIcon({ className }: NavIconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden="true"
      className={className ?? "h-7 w-7"}
    >
      <path d="M6 3h12v1h3v3a4 4 0 0 1-3.4 3.95A6.01 6.01 0 0 1 13 14.92V18h3v3H8v-3h3v-3.08a6.01 6.01 0 0 1-4.6-3.97A4 4 0 0 1 3 7V4h3V3zm0 3H5v1a2 2 0 0 0 1 1.73V6zm12 0v2.73A2 2 0 0 0 19 7V6h-1z" />
    </svg>
  );
}
