// Lobby mirror rendering: the Discord post for a public web-lobby room. Pure
// functions over `LobbyRoom`; the build labelling matches /lfg posts (lfgView.ts).

import { type Build, BUILD_ENDPOINTS } from "./config";
import { ComponentType } from "./discord";
import { findFormat } from "./formats";
import { type ActionRow, BUILD_COLORS, buildTag, joinLink, linkButton, siteLine } from "./lfgView";
import type { Embed } from "./render";

/** What a mirror post shows of a listed room. Stored as the post's `shown` JSON,
 *  so a change to any field is a change to the post. */
export interface LobbyRoom {
  code: string;
  /** The listing's registration time, in seconds (lobby `created_at`). */
  createdAt: number;
  /** `GameFormat` key, or null when the host's client sent none. */
  format: string | null;
  /** The room name, or the host's name when the room has none. */
  name: string;
  /** The host's name when `name` is a room name, else null. */
  host: string | null;
  current: number;
  max: number;
}

/** Backslash-escapes Discord markdown, so a player-chosen name renders as typed. */
export function escapeMarkdown(text: string): string {
  return text.replace(/[\\*_~`|>[\]#\-<]/g, "\\$&");
}

function formatLabel(format: string | null): string {
  return format === null ? "Game" : (findFormat(format)?.label ?? format);
}

/** A mirror post's message body (create or edit). */
type LobbyPost = { embeds: [Embed]; components: ActionRow[]; allowed_mentions: { parse: [] } };

/** The post for a listed room: a Join link while a seat is free, "Full" once none is. */
export function renderLobbyPost(build: Build, room: LobbyRoom): LobbyPost {
  const full = room.current >= room.max;
  const lines = [
    escapeMarkdown(room.name),
    ...(room.host === null ? [] : [`Host: ${escapeMarkdown(room.host)}`]),
    siteLine(build),
    full ? "Full — waiting for the host to start." : "Open in the lobby — press Join to take a seat.",
  ];
  return {
    embeds: [
      {
        title: `${formatLabel(room.format)} · ${room.current}/${room.max} · ${buildTag(build)}`,
        description: lines.join("\n"),
        color: BUILD_COLORS[build],
      },
    ],
    components: full
      ? []
      : [
          {
            type: ComponentType.ACTION_ROW,
            components: [linkButton("Join", joinLink(build, room.code, BUILD_ENDPOINTS[build].lobbyWs))],
          },
        ],
    allowed_mentions: { parse: [] },
  };
}

/** The post once its room has left the lobby. The lobby cannot say whether the
 *  game started or was abandoned, so the post says only that it is no longer open. */
export function renderLobbyPostClosed(build: Build, room: LobbyRoom): LobbyPost {
  return {
    embeds: [
      {
        title: `${formatLabel(room.format)} · ${buildTag(build)}`,
        description: [escapeMarkdown(room.name), siteLine(build), "No longer open."].join("\n"),
        color: BUILD_COLORS[build],
      },
    ],
    components: [],
    allowed_mentions: { parse: [] },
  };
}
