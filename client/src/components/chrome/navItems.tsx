import type { ComponentType } from "react";

import {
  DecksNavIcon,
  DraftNavIcon,
  HomeIcon,
  OnlineNavIcon,
  PlayNavIcon,
  TournamentNavIcon,
} from "./navIcons";

export interface NavItem {
  key: string;
  /** Destination route. */
  path: string;
  /** i18n key under the `menu` namespace, e.g. `nav.home`. */
  labelKey: string;
  Icon: ComponentType<{ className?: string }>;
  /** Route prefixes that should light this item up (besides `path`). */
  match: (pathname: string) => boolean;
}

const PRIMARY_NAV_ITEMS: NavItem[] = [
  { key: "home", path: "/", labelKey: "nav.home", Icon: HomeIcon, match: (p) => p === "/" },
  { key: "play", path: "/setup", labelKey: "nav.play", Icon: PlayNavIcon, match: (p) => p.startsWith("/setup") },
  { key: "online", path: "/multiplayer", labelKey: "nav.online", Icon: OnlineNavIcon, match: (p) => p.startsWith("/multiplayer") },
  { key: "draft", path: "/draft", labelKey: "nav.draft", Icon: DraftNavIcon, match: (p) => p.startsWith("/draft") },
  { key: "decks", path: "/my-decks", labelKey: "nav.decks", Icon: DecksNavIcon, match: (p) => p.startsWith("/my-decks") || p.startsWith("/deck-builder") },
];

const TOURNAMENT_NAV_ITEM: NavItem = {
  key: "tournament",
  path: "/tournament",
  labelKey: "nav.tournament",
  Icon: TournamentNavIcon,
  match: (p) => p.startsWith("/tournament"),
};

/** Non-experimental primary navigation destinations. */
export const NAV_ITEMS: NavItem[] = PRIMARY_NAV_ITEMS;

/**
 * Tournaments are still in development, so their navigation entry is opt-in.
 * The routes remain available for testers using a direct tournament link.
 */
export function navItemsFor(experimentalTournamentsEnabled: boolean): NavItem[] {
  return experimentalTournamentsEnabled
    ? [...PRIMARY_NAV_ITEMS, TOURNAMENT_NAV_ITEM]
    : PRIMARY_NAV_ITEMS;
}

/** The key of the nav item that should appear active for a given pathname. */
export function activeNavKey(pathname: string, navItems = NAV_ITEMS): string | null {
  return navItems.find((item) => item.match(pathname))?.key ?? null;
}
