# Tabletop 3D asset bootstrap

The repository does not bundle the third-party frame, font, or mana-symbol
files used by the tabletop 3D card compositor. A fresh checkout can fetch them
into the gitignored `client/public/tabletop3d/` directory:

```bash
cd client
pnpm install --frozen-lockfile
pnpm tabletop3d:assets
```

The bootstrap script retrieves:

| Input | Source | Local output |
| --- | --- | --- |
| M15 frame and stat-box images | Pinned [Card Conjurer](https://github.com/fiahdrgn473/CardConjurer) revision | `frames/m15/` |
| Beleren and MPlantin font files | The same pinned Card Conjurer revision | `fonts/` |
| Preview stat-box image | Pinned [cardconjurer fork](https://github.com/Investigamer/cardconjurer) revision | `frames/card-conjurer/` |
| Mana symbol SVGs | [Scryfall symbol CDN](https://svgs.scryfall.io/) | Rasterized into `pips/` |
| Mana glyph font | The lockfile-pinned [`mana-font`](https://www.npmjs.com/package/mana-font) package | `fonts/mana.woff2` |

The script pins repository inputs to commit hashes, skips files already present,
and uses the lockfile-pinned `sharp` dependency to rasterize SVG symbols. Run it
again after deleting `client/public/tabletop3d/` to rebuild the complete local
asset set.

These files remain subject to their respective owners' terms. The bootstrap
command is a development convenience and does not grant redistribution rights.
The base 3D play surface is code-only and requires no downloaded texture.

Card art is not part of this bootstrap. It continues to come from the existing
Scryfall image URLs at runtime. Canvas/WebGL composition uses the fixed
same-origin `/card-image-art` and `/card-image-back` routes supplied by Vite
during local development.
