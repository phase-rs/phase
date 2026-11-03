import type { Preview } from "@storybook/react-vite";

// The app's own boot order: webfonts, then the mana/loyalty glyph font, then
// Tailwind and the design tokens (`--card-w`, `--surface-card`, the dark body
// gradient) every component here is styled against. Mirrors src/main.tsx so a
// story renders on the same foundation the app does.
import "@fontsource-variable/newsreader";
import "@fontsource-variable/jetbrains-mono";
import "mana-font/css/mana.css";
import "../src/index.css";
// i18next resources are eager and synchronous, so importing the app's setup is
// all that is needed — every `useTranslation()` resolves on first render.
import "../src/i18n";

/**
 * Storybook reads this file by convention and takes its configuration from the
 * default export, so the repo's usual named-export rule does not apply here or
 * in a `*.stories.tsx` meta.
 */
const preview: Preview = {
  parameters: {
    // `src/index.css` paints the app's dark gradient onto `body`. The
    // backgrounds addon would cover it with a flat swatch and misrepresent
    // every surface that is tuned against that gradient.
    backgrounds: { disable: true },
    controls: { matchers: { color: /(background|color)$/i } },
    layout: "centered",
  },
  // Prop tables are generated from each component's own types, which is what
  // keeps the catalog honest as those types change.
  tags: ["autodocs"],
};

export default preview;
