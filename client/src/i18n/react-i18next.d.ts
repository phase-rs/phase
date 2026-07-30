// Typed translation keys: augment react-i18next with the English catalogs as the
// type oracle so `t("bad.key")` is a compile error. Only English is imported for
// types — other locales are runtime-only and may lag without breaking the build.
// `import type` (not plain import) is required under `verbatimModuleSyntax` so the
// JSON never becomes a runtime import in this declaration file.
import type common from "./locales/en/common.json";
import type deckBuilder from "./locales/en/deck-builder.json";
import type draft from "./locales/en/draft.json";
import type game from "./locales/en/game.json";
import type menu from "./locales/en/menu.json";
import type multiplayer from "./locales/en/multiplayer.json";
import type replay from "./locales/en/replay.json";
import type settings from "./locales/en/settings.json";

declare module "react-i18next" {
  interface CustomTypeOptions {
    defaultNS: "common";
    resources: {
      common: typeof common;
      menu: typeof menu;
      game: typeof game;
      "deck-builder": typeof deckBuilder;
      draft: typeof draft;
      settings: typeof settings;
      multiplayer: typeof multiplayer;
      replay: typeof replay;
    };
  }
}
