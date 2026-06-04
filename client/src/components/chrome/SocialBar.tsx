import { SOCIAL_LINKS, social } from "./socialLinks";

/**
 * Social strip hosted on the left of the shell's sticky top chrome row. Rendered
 * in normal flow (not fixed), so the row reserves real layout space and page
 * content — including the deck builder's own toolbar — always clears it.
 */
export function SocialBar() {
  return (
    <div className="flex items-center gap-0.5 rounded-full border border-hairline bg-black/45 px-1.5 py-1 backdrop-blur-md">
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
