import "@testing-library/jest-dom/vitest";

import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import common from "./i18n/locales/en/common.json";
import menu from "./i18n/locales/en/menu.json";
import game from "./i18n/locales/en/game.json";
import deckBuilder from "./i18n/locales/en/deck-builder.json";
import draft from "./i18n/locales/en/draft.json";
import settings from "./i18n/locales/en/settings.json";
import multiplayer from "./i18n/locales/en/multiplayer.json";

// Tests only ever assert against English copy, so we register a lean,
// English-only i18next instance here instead of importing the app's `./i18n`
// module. That module eager-globs all 7 languages × 7 namespaces (49 catalogs)
// and wires up a `preferencesStore` subscription — none of which a test needs,
// and all of which was paid per test file under Vitest's worker isolation.
//
// No source module imports the i18n singleton directly; every consumer uses
// `useTranslation()`, which resolves against the global instance registered by
// `initReactI18next` below. So this fully serves component tests (getByText)
// without re-triggering the full bootstrap. Keep the namespace list in sync
// with `NAMESPACES` in `src/i18n/index.ts`.
void i18n.use(initReactI18next).init({
  lng: "en",
  fallbackLng: "en",
  ns: ["common", "menu", "game", "deck-builder", "draft", "settings", "multiplayer"],
  defaultNS: "common",
  resources: {
    en: {
      common,
      menu,
      game,
      "deck-builder": deckBuilder,
      draft,
      settings,
      multiplayer,
    },
  },
  interpolation: { escapeValue: false }, // React already escapes
  returnNull: false,
  react: { useSuspense: false },
});
