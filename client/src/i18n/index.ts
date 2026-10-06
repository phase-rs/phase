import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import { usePreferencesStore } from "../stores/preferencesStore";
import { resources, SUPPORTED_LNGS, type SupportedLng } from "./resources";

/** All translation namespaces, one per UI domain. `common` is the default ns and
 *  is loaded implicitly by every `useTranslation()`; others are opted into via
 *  `useTranslation("game")`. Keep in sync with the catalog files under locales/. */
export const NAMESPACES = [
  "common",
  "menu",
  "game",
  "deck-builder",
  "draft",
  "settings",
  "multiplayer",
  "replay",
  "tournament",
] as const;

// Resources are eager-bundled and synchronous (see resources.ts), so init runs
// to completion before any component renders — no Suspense, no async load. The
// initial language is seeded from the preferences store, which has already
// hydrated synchronously from localStorage by the time this module evaluates
// (zustand `persist` over a sync storage hydrates during `create()`). The store
// is the single source of truth; i18next and the document language are derived
// mirrors.
function syncDocumentLanguage(language: SupportedLng): void {
  if (typeof document !== "undefined") {
    document.documentElement.lang = language;
  }
}

const initialLanguage = usePreferencesStore.getState().language;
syncDocumentLanguage(initialLanguage);

void i18n.use(initReactI18next).init({
  resources,
  lng: initialLanguage,
  fallbackLng: "en",
  supportedLngs: SUPPORTED_LNGS,
  ns: NAMESPACES,
  defaultNS: "common",
  nonExplicitSupportedLngs: true, // "es-MX" → "es"
  interpolation: { escapeValue: false }, // React already escapes
  returnNull: false,
  react: { useSuspense: false },
});

// Mirror store → i18next. Only the store writes the language; this keeps i18next's
// active language in lockstep. Basic subscribe (no subscribeWithSelector needed).
const unsubscribePreferences = usePreferencesStore.subscribe((state, prev) => {
  if (state.language !== prev.language) {
    syncDocumentLanguage(state.language);
    if (i18n.language !== state.language) {
      void i18n.changeLanguage(state.language);
    }
  }
});

if (import.meta.hot) {
  import.meta.hot.dispose(unsubscribePreferences);
}

export default i18n;
