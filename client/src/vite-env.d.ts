/// <reference types="vite/client" />
/// <reference types="vite-plugin-pwa/client" />

declare const __APP_VERSION__: string;
declare const __BUILD_HASH__: string;
declare const __DEFAULT_MULTIPLAYER_SERVER_URL__: string;
declare const __CARD_DATA_URL__: string;
declare const __CARD_DATA_LOCALE_URL_TEMPLATE__: string;
declare const __CARD_NAMES_URL__: string;
declare const __CHANGELOG_URL__: string;
declare const __CHANGELOG_META_URL__: string;
declare const __COVERAGE_DATA_URL__: string;
declare const __COVERAGE_SUMMARY_URL__: string;
declare const __CARD_DATA_META_URL__: string;
declare const __SET_LIST_URL__: string;
declare const __DECKS_URL__: string;
declare const __DRAFT_POOLS_URL__: string;
declare const __SCRYFALL_DATA_URL__: string;
declare const __SCRYFALL_TOKEN_IMAGES_URL__: string;
declare const __SCRYFALL_PRINTINGS_URL__: string;
declare const __SCRYFALL_SETS_URL__: string;
declare const __GIT_REPO_URL__: string;
declare const __PREVIEW_SITE_URL__: string;
declare const __IS_RELEASE_BUILD__: boolean;
declare const __SUPABASE_URL__: string;
declare const __SUPABASE_ANON_KEY__: string;
declare const __TELEMETRY_URL__: string;

// Fontsource packages ship only side-effect CSS (no type declarations); Vite
// resolves the import at build time, but tsc needs an ambient module.
declare module "@fontsource-variable/*";
