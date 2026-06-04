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
