// /lfg rendering: the public post, the ephemeral Get-my-link reply, the ready
// ping, and the button custom_ids. Pure functions over `Lfg`.
//
// Link grammar is Phase B's (client multiplayer page):
//   host:  <site>/multiplayer?code=CODE&format=<GameFormat>&players=N&room=<name>[&server=<ws url>]
//   guest: <site>/multiplayer?join=CODE@<ws url>
// Both are built with URLSearchParams in B's field order; never `view=`.
//
// Desktop grammar is Phase E's (client /open-desktop trampoline → desktop shell):
//   desktop: <site>/open-desktop?to=<phase://open?site=<build>&path=</multiplayer?… of the web link>>

import { BUILD_ENDPOINTS } from "./config";
import { ButtonStyle, ComponentType, MessageFlags, ResponseType } from "./discord";
import type { LfgFormat } from "./formats";
import { LFG_IDLE_MS, type Lfg, type Refusal } from "./lfg";
import type { Embed } from "./render";

export type LfgAction = "join" | "leave" | "start" | "link";

const LFG_ACTIONS: readonly LfgAction[] = ["join", "leave", "start", "link"];

interface ActionButton {
  type: typeof ComponentType.BUTTON;
  style: typeof ButtonStyle.PRIMARY | typeof ButtonStyle.SECONDARY | typeof ButtonStyle.SUCCESS;
  label: string;
  custom_id: string;
}

export interface LinkButton {
  type: typeof ComponentType.BUTTON;
  style: typeof ButtonStyle.LINK;
  label: string;
  url: string;
}

export interface ActionRow {
  type: typeof ComponentType.ACTION_ROW;
  components: (ActionButton | LinkButton)[];
}

/** `lfg:<action>:<id>` — at most 46 chars with a UUID id (Discord allows 100). */
export function customId(action: LfgAction, id: string): string {
  return `lfg:${action}:${id}`;
}

export function parseCustomId(value: string): { action: LfgAction; id: string } | null {
  const parts = value.split(":");
  if (parts.length !== 3) return null;
  const [prefix, action, id] = parts;
  const known = LFG_ACTIONS.find((a) => a === action);
  if (prefix !== "lfg" || known === undefined || id === "") return null;
  return { action: known, id };
}

/** The lobby room name. No user text reaches it, so the lobby's name filter can
 *  never reject a bot game on a nickname. */
export function roomName(format: LfgFormat): string {
  return `Discord ${format.label}`;
}

function readyCode(lfg: Lfg): string {
  if (lfg.code === null) throw new Error(`lfg ${lfg.id} has no code (not ready)`);
  return lfg.code;
}

/** The creator's link: HostSetup seeded with the code, format, seat count and room. */
export function hostLink(lfg: Lfg): string {
  const params = new URLSearchParams({
    code: readyCode(lfg),
    format: lfg.format.format,
    players: String(lfg.seated.length),
    room: roomName(lfg.format),
  });
  if (lfg.server) params.append("server", lfg.server.url);
  return `${BUILD_ENDPOINTS[lfg.build].site}/multiplayer?${params}`;
}

/** A seated guest's link: the code at the game's server (the dedicated server,
 *  or the build's official lobby for P2P). */
export function guestLink(lfg: Lfg): string {
  const serverUrl = lfg.server?.url ?? BUILD_ENDPOINTS[lfg.build].lobbyWs;
  const params = new URLSearchParams({ join: `${readyCode(lfg)}@${serverUrl}` });
  return `${BUILD_ENDPOINTS[lfg.build].site}/multiplayer?${params}`;
}

function actionButton(
  style: ActionButton["style"],
  label: string,
  action: LfgAction,
  id: string,
): ActionButton {
  return { type: ComponentType.BUTTON, style, label, custom_id: customId(action, id) };
}

function descriptionLines(lfg: Lfg): string[] {
  const modeLine =
    lfg.server === null
      ? `Peer-to-peer — hosted in <@${lfg.creatorId}>'s browser`
      : `Dedicated server: ${lfg.server.name}`;
  const siteLine = `Site: ${lfg.build} (${new URL(BUILD_ENDPOINTS[lfg.build].site).host})`;
  const seatLines = lfg.seated.map((id) => (id === lfg.creatorId ? `<@${id}> (host)` : `<@${id}>`));
  return [modeLine, siteLine, "", `**Players ${lfg.seated.length}/${lfg.seats}**`, ...seatLines];
}

/** The public post. The room code never appears in it. */
export function renderLfg(lfg: Lfg): {
  embeds: [Embed];
  components: ActionRow[];
  allowed_mentions: { parse: [] };
} {
  const embed: Embed = { title: `LFG · ${lfg.format.label}` };
  const lines = descriptionLines(lfg);
  let components: ActionRow[] = [];
  switch (lfg.state) {
    case "open":
      embed.footer = { text: `Expires after ${LFG_IDLE_MS / 60_000} min idle` };
      components = [
        {
          type: ComponentType.ACTION_ROW,
          components: [
            actionButton(ButtonStyle.PRIMARY, "Join", "join", lfg.id),
            actionButton(ButtonStyle.SECONDARY, "Leave", "leave", lfg.id),
            actionButton(ButtonStyle.SUCCESS, "Start", "start", lfg.id),
          ],
        },
      ];
      break;
    case "ready":
      lines.push(
        "",
        "Ready! Press **Get my link**. Host: open your link first. Links stay available for 24 hours.",
      );
      components = [
        {
          type: ComponentType.ACTION_ROW,
          components: [actionButton(ButtonStyle.PRIMARY, "Get my link", "link", lfg.id)],
        },
      ];
      break;
    case "cancelled":
      lines.push("", "Cancelled by the host");
      break;
    case "expired":
      lines.push("", "Expired");
      break;
    default: {
      const unreachable: never = lfg.state;
      throw new Error(`unknown lfg state ${unreachable}`);
    }
  }
  embed.description = lines.join("\n");
  return { embeds: [embed], components, allowed_mentions: { parse: [] } };
}

/** The post after its row is gone (swept, or from another guild). */
export function renderEnded(): { content: string; embeds: []; components: [] } {
  return { content: "This LFG has ended.", embeds: [], components: [] };
}

/** Discord's link-button URL limit. One over-limit component rejects the whole
 *  message, so a button that might exceed it must be left out, not sent. */
export const LINK_BUTTON_URL_MAX = 512;

/** The desktop-app link for a web link: the site's /open-desktop trampoline,
 *  because Discord link buttons must be http(s). The web link's path is encoded
 *  inside `phase://`, which is encoded again inside `to=`, so every encoded byte
 *  compounds; callers check the result against `LINK_BUTTON_URL_MAX`. */
export function desktopLink(lfg: Lfg, webLink: string): string {
  const url = new URL(webLink);
  const to = `phase://open?${new URLSearchParams({ site: lfg.build, path: url.pathname + url.search })}`;
  return `${BUILD_ENDPOINTS[lfg.build].site}/open-desktop?${new URLSearchParams({ to })}`;
}

function linkButton(label: string, url: string): LinkButton {
  return { type: ComponentType.BUTTON, style: ButtonStyle.LINK, label, url };
}

/** The Get-my-link buttons for a seated user of a ready LFG: the web link
 *  first, always, then the desktop link when it fits Discord's URL limit. */
export function linkButtons(lfg: Lfg, userId: string): LinkButton[] {
  const [label, url] =
    userId === lfg.creatorId ? ["Open as host", hostLink(lfg)] : ["Join game", guestLink(lfg)];
  const desktop = desktopLink(lfg, url);
  return desktop.length <= LINK_BUTTON_URL_MAX
    ? [linkButton(label, url), linkButton("Open in desktop app", desktop)]
    : [linkButton(label, url)];
}

export interface LinkReply {
  type: typeof ResponseType.CHANNEL_MESSAGE_WITH_SOURCE;
  data: {
    flags: typeof MessageFlags.EPHEMERAL;
    content: string;
    components: ActionRow[];
    allowed_mentions: { parse: [] };
  };
}

/** The ephemeral Get-my-link reply for a seated user of a ready LFG. */
export function linkReply(lfg: Lfg, userId: string): LinkReply {
  const role =
    userId === lfg.creatorId
      ? "You're hosting — open this first, then pick a deck and press Host."
      : `You're joining <@${lfg.creatorId}>'s game — open this once the host has opened theirs.`;
  return {
    type: ResponseType.CHANNEL_MESSAGE_WITH_SOURCE,
    data: {
      flags: MessageFlags.EPHEMERAL,
      content: `${role}\nGame code: \`${readyCode(lfg)}\``,
      components: [{ type: ComponentType.ACTION_ROW, components: linkButtons(lfg, userId) }],
      allowed_mentions: { parse: [] },
    },
  };
}

/** The follow-up that pings every seated user once the game is ready. Mentions
 *  only those users (`users` without `parse`; ≤ 8 seats, under Discord's 100). */
export function readyPing(lfg: Lfg): { content: string; allowed_mentions: { users: string[] } } {
  const mentions = lfg.seated.map((id) => `<@${id}>`).join(" ");
  return {
    content: `Game ready: ${mentions} — press **Get my link** on the post above.`,
    allowed_mentions: { users: [...lfg.seated] },
  };
}

/** One sentence per refusal, for an ephemeral reply. */
export function refusalText(reason: Refusal, format: LfgFormat): string {
  switch (reason) {
    case "has_open":
      return "You already have an open LFG here — cancel it (Leave) or wait for it to expire first.";
    case "already_seated":
      return "You're already in this game.";
    case "not_seated":
      return "You're not seated in this game.";
    case "not_creator":
      return "Only the host can start the game.";
    case "too_few":
      return `${format.label} needs at least ${format.min_players} players to start.`;
    case "not_open":
      return "This game isn't taking seat changes anymore.";
    case "not_ready":
      return "Links appear once the game is ready.";
    default: {
      const unreachable: never = reason;
      throw new Error(`unknown refusal ${unreachable}`);
    }
  }
}
