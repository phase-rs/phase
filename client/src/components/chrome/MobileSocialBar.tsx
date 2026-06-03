import { SOCIAL_LINKS, social } from "./socialLinks";

/**
 * Mobile-only (<820px) social strip pinned to the top-left of the scene,
 * restoring the icon row that the desktop rail carries. Hidden at ≥820px where
 * the Rail's labelled badges take over.
 */
export function MobileSocialBar() {
  return (
    <div className="fixed left-[calc(env(safe-area-inset-left)+0.5rem)] top-[calc(env(safe-area-inset-top)+0.5rem)] z-40 flex items-center gap-0.5 rounded-full border border-hairline bg-black/45 px-1.5 py-1 backdrop-blur-md min-[820px]:hidden">
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
