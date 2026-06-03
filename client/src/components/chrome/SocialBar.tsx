import { SOCIAL_LINKS, social } from "./socialLinks";

/**
 * Shell-level social strip pinned to the top-left, the left-hand counterpart to
 * the top-right ChromeControls cluster. Below 820px it sits in the screen's
 * top-left corner; at ≥820px it clears the fixed 92px rail and aligns with the
 * ChromeControls' top edge. AppShell nudges page content down on both axes so
 * page titles clear this row.
 */
export function SocialBar() {
  return (
    <div className="fixed left-[calc(env(safe-area-inset-left)+0.5rem)] top-[calc(env(safe-area-inset-top)+0.5rem)] z-40 flex items-center gap-0.5 rounded-full border border-hairline bg-black/45 px-1.5 py-1 backdrop-blur-md min-[820px]:left-[calc(92px+1rem)] min-[820px]:top-[calc(env(safe-area-inset-top)+1rem)]">
      {SOCIAL_LINKS.map(({ key, url, label, Glyph, hover }) => (
        <a
          key={key}
          href={url}
          onClick={social(url)}
          aria-label={label}
          title={label}
          className={`flex h-7 w-7 items-center justify-center rounded-full text-fg-meta transition-colors ${hover}`}
        >
          <Glyph />
        </a>
      ))}
    </div>
  );
}
