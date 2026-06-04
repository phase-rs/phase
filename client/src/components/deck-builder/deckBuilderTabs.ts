export type DeckBuilderSurface = "deck" | "info" | "playtest";

// Shared id scheme so the tabs (DeckBuilderTabBar) and the panels (DeckBuilder)
// can cross-reference via aria-controls / aria-labelledby. Kept in a non-
// component module so DeckBuilderTabBar can stay Fast-Refresh-clean (a file
// that exports a component must export only components).
export const tabId = (surface: DeckBuilderSurface) => `deckbuilder-tab-${surface}`;
export const panelId = (surface: DeckBuilderSurface) => `deckbuilder-panel-${surface}`;
