import { SOCIAL_LINKS, social } from "./socialLinks";

/**
 * Social strip hosted on the left of the shell's sticky top chrome row. Rendered
 * in normal flow (not fixed), so the row reserves real layout space and page
 * content — including the deck builder's own toolbar — always clears it.
 */
export function SocialBar() {
  return (
    <div className="flex items-center gap-0.5 rounded-[10px] border border-white/[0.09] bg-[rgba(6,10,22,0.52)] px-1.5 py-1 shadow-[0_8px_22px_rgba(0,0,0,0.18)] backdrop-blur-xl">
      {SOCIAL_LINKS.map(({ key, url, label, Glyph, hover }) => (
        <a
          key={key}
          href={url}
          onClick={social(url)}
          aria-label={label}
          title={label}
          className={`flex h-7 w-7 items-center justify-center rounded-[7px] text-fg-meta transition-colors hover:bg-white/[0.05] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/35 ${hover}`}
        >
          <Glyph />
        </a>
      ))}
    </div>
  );
}
