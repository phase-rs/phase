import type { ComponentType } from "react";

import {
  DecksNavIcon,
  DraftNavIcon,
  HomeIcon,
  OnlineNavIcon,
  PlayNavIcon,
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

export const NAV_ITEMS: NavItem[] = [
  { key: "home", path: "/", labelKey: "nav.home", Icon: HomeIcon, match: (p) => p === "/" },
  { key: "play", path: "/setup", labelKey: "nav.play", Icon: PlayNavIcon, match: (p) => p.startsWith("/setup") },
  { key: "online", path: "/multiplayer", labelKey: "nav.online", Icon: OnlineNavIcon, match: (p) => p.startsWith("/multiplayer") },
  { key: "draft", path: "/draft", labelKey: "nav.draft", Icon: DraftNavIcon, match: (p) => p.startsWith("/draft") },
  { key: "decks", path: "/my-decks", labelKey: "nav.decks", Icon: DecksNavIcon, match: (p) => p.startsWith("/my-decks") || p.startsWith("/deck-builder") },
];

/** The key of the nav item that should appear active for a given pathname. */
export function activeNavKey(pathname: string): string | null {
  return NAV_ITEMS.find((item) => item.match(pathname))?.key ?? null;
}
