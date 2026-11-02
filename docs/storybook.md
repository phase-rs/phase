# Storybook

A component catalog for the React client. Every entry renders a real component
against real props, so a UI change can be reviewed on its own instead of by
setting up a game that reaches the surface it touched.

```bash
cd client
pnpm storybook          # dev server on http://localhost:6006
pnpm build-storybook    # static site in client/storybook-static/ (gitignored)
```

Nothing else has to be built first. Storybook is not a Tilt resource: it is a
tool you bring up when you want it. The story files are still ordinary client
code, so the authoritative check on them is Tilt's `check-frontend` (lint and
type-check) and `test-frontend`. Run `pnpm run type-check` or `pnpm lint`
directly only when Tilt is down.

## Layout

```
client/storybook/
  main.ts                    Framework, story glob, Vite overrides
  preview.ts                 Fonts, Tailwind, design tokens, i18next
  mocks/
    useCardImage.ts          Card art without the Scryfall lookup maps
    useEngineCardData.ts     Card lookups without the WASM engine
    engineWasm.ts            Stub for @wasm/engine and @wasm/draft
client/src/**/*.stories.tsx  Stories, next to the component they document
```

Stories sit beside their component, the way `__tests__/` does. A story file
carries a default export of `meta` and one named export per story, which is
Storybook's own contract and the one place the repo's no-default-export rule
does not apply.

## What `main.ts` changes about the app's Vite config

Storybook loads `client/vite.config.ts` and inherits it, so components compile
against the same `__*_URL__` defines, the same Tailwind setup, and the same
wasm-bindgen import shims the app uses. Three things are overridden:

- **The PWA and brotli plugins are dropped.** A service worker would cache
  Storybook's own preview iframe, and compressing a dev catalog buys nothing.
- **`useCardImage` and `useEngineCardData` resolve to `storybook/mocks/`.**
- **`@wasm/engine` and `@wasm/draft` resolve to a stub that throws.**

## Why those three modules are doubled

Each reaches for a generated artifact that a fresh checkout does not have, so
without the doubles Storybook could not start until someone had run both the
card-data pipeline and wasm-pack:

| Module | Real dependency | Double |
|---|---|---|
| `useCardImage` | `client/public/scryfall-*.json`, generated and gitignored | Asks Scryfall for the image by card name |
| `useEngineCardData` | The WASM card database | Reports "nothing known" — a story that wants Oracle text passes it as a prop |
| `@wasm/engine`, `@wasm/draft` | wasm-pack output under `client/src/wasm/`, gitignored | Throws if a story ever calls in |

The two hook doubles are typed against the real modules, so changing either
hook's signature fails `pnpm type-check` rather than drifting silently. The
WASM stub throws on purpose: a story that needs game state is a story that
belongs on props instead, per the display-layer rule in `CLAUDE.md`.

The card-art double keeps the real hook's contract, which is what makes
`CardImage`'s three render branches reachable from ordinary props — art in
flight draws the skeleton, art resolved draws the `<img>`, and a name Scryfall
does not know draws `CardArtFallback`. It does need network access; offline,
every card falls back to its text tile.

Checked-in art would remove that network dependency, and it is not an option:
`DMCA.md` states that this repository bundles no card images or card art, and
that images are fetched from Scryfall at runtime. The double does what the app
does. A neutral placeholder is no substitute either — every card would render
identically, which defeats a card catalog.

## Adding a story

1. Write `Component.stories.tsx` next to the component.
2. Type `meta` with `satisfies Meta<typeof Component>` so the args are checked
   against the component's own props.
3. Give each story a doc comment saying what state it captures and why that
   state matters. The comment becomes the description on the docs page.
4. Cover the states a reviewer cannot otherwise reach: empty, disabled, closed,
   overflowing, and every variant of an enum prop.

Prefer stories that exercise a component's whole prop range over stories that
reproduce one card. The catalog is worth keeping only as long as it documents
the building block rather than a single call site.
