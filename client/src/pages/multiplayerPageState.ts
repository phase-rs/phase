import type { GameFormat } from "../adapter/types";
import { parseWebSocketUrl } from "../config/multiplayerServer";
import type { DeckCompatibilityResult } from "../services/deckCompatibility";

/** Views available on the multiplayer page. "draft-lobby" shows the
 *  multiplayer draft pod lobby after hosting or joining a draft. */
export type MultiplayerView = "lobby" | "host-setup" | "deck-select" | "draft-lobby";

export type LiveCheck =
  | { status: "idle" }
  | { status: "checking"; format: GameFormat }
  | { status: "legal"; format: GameFormat }
  | { status: "illegal"; format: GameFormat; reasons: string[] };

/**
 * Classify an engine compatibility result into a `LiveCheck` display state.
 *
 * `selected_format_compatible` is a three-state:
 *   false → illegal (show the engine-provided reasons)
 *   true  → legal
 *   null / undefined → indeterminate; the chip must be suppressed (idle)
 *     rather than claiming "legal". Treating indeterminate as affirmative
 *     would mislead the user whenever the engine's format registry can't
 *     make a decision.
 */
export function classifyCompatResult(
  format: GameFormat,
  result: DeckCompatibilityResult,
): LiveCheck {
  if (result.selected_format_compatible === false) {
    return {
      status: "illegal",
      format,
      reasons: result.selected_format_reasons,
    };
  }
  if (result.selected_format_compatible === true) {
    return { status: "legal", format };
  }
  return { status: "idle" };
}

/** Shape of a Discord-link game code (the broker's requested-code guard). */
const BOT_GAME_CODE = /^[A-Z0-9]{6}$/;

/**
 * Host settings carried by a Discord host link. Syntax-checked only: whether
 * the format is known and the count fits it is decided by `HostSetup`, which
 * owns the registry and connection-mode context.
 */
export interface HostSeed {
  code: string;
  format: string | null;
  playerCount: number | null;
  roomName: string | null;
  /** Dedicated server the game lives on; `null` → P2P over the build's
   *  official broker. */
  serverUrl: string | null;
}

export type BotLink =
  | { kind: "host"; seed: HostSeed }
  | { kind: "join"; code: string; serverUrl: string }
  | { kind: "invalid" };

/**
 * Read a Discord bot link from the multiplayer page's search string.
 *
 * Grammar:
 * - host: `?code=AB12CD&format=<GameFormat>&players=N&room=<name>[&server=<ws(s) url>]`
 * - guest: `?join=AB12CD@<ws(s) url>`
 *
 * `null` when the search carries neither `code` nor `join` (not a bot link);
 * `invalid` when it does but any part is malformed. The single reader of
 * these params; `hostLinkSearch` is its inverse for the host branch.
 */
export function parseBotLink(search: string): BotLink | null {
  const params = new URLSearchParams(search);
  const code = params.get("code");
  const join = params.get("join");
  if (code !== null && join !== null) return { kind: "invalid" };

  if (code !== null) {
    if (!BOT_GAME_CODE.test(code)) return { kind: "invalid" };
    const players = params.get("players");
    if (players !== null && !/^\d+$/.test(players)) return { kind: "invalid" };
    const server = params.get("server");
    const serverUrl = server === null ? null : (parseWebSocketUrl(server)?.href ?? null);
    if (server !== null && serverUrl === null) return { kind: "invalid" };
    const room = params.get("room")?.trim();
    return {
      kind: "host",
      seed: {
        code,
        format: params.get("format"),
        playerCount: players === null ? null : Number(players),
        roomName: room ? room : null,
        serverUrl,
      },
    };
  }

  if (join === null) return null;
  // `parseJoinCode` is not used: it cannot keep a URL path (`…/ws` → `…/ws/ws`).
  const at = join.indexOf("@");
  if (at < 0) return { kind: "invalid" };
  const joinCode = join.slice(0, at);
  const url = parseWebSocketUrl(join.slice(at + 1));
  if (!BOT_GAME_CODE.test(joinCode) || url === null) return { kind: "invalid" };
  return { kind: "join", code: joinCode, serverUrl: url.href };
}

/**
 * The host-link search (no leading `?`) that `parseBotLink` reads back as
 * `seed`. Used to carry a Discord host flow across the deck-builder round trip,
 * whose return re-enters the page through the bot-link arrival.
 */
export function hostLinkSearch(seed: HostSeed): string {
  const params = new URLSearchParams({ code: seed.code });
  if (seed.format !== null) params.set("format", seed.format);
  if (seed.playerCount !== null) params.set("players", String(seed.playerCount));
  if (seed.roomName !== null) params.set("room", seed.roomName);
  if (seed.serverUrl !== null) params.set("server", seed.serverUrl);
  return params.toString();
}
