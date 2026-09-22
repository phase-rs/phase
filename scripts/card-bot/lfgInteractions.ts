// /lfg interaction handlers: the slash command, its `server` autocomplete, and the
// post's buttons. Every input is in memory (the ServerCache snapshot) or
// synchronous (bun:sqlite), so each handler answers within Discord's 3 s window
// without deferring — which lets refusals be ephemeral and the post public.

import { isBuild, LFG_DEFAULT_BUILD, type Build } from "./config";
import {
  type CommandInteraction,
  type ComponentInteraction,
  integerOption,
  invokerId,
  jsonResponse,
  MessageFlags,
  ResponseType,
  stringOption,
} from "./discord";
import { findFormat, type LfgMode, seatCap } from "./formats";
import type { LfgStore, Outcome } from "./lfg";
import {
  type LfgAction,
  linkReply,
  readyPing,
  refusalText,
  renderEnded,
  renderLfg,
} from "./lfgView";
import { brokerSupportsBotGames, eligibleServers, type ServerCache } from "./servers";

export interface LfgDeps {
  store: LfgStore;
  servers: ServerCache;
  now: () => number;
  /** Posts a follow-up message on an interaction (createFollowupMessage in production). */
  followup: (appId: string, token: string, body: unknown) => Promise<void>;
}

/** Discord caps autocomplete at 25 choices, each name at 100 chars. */
const MAX_AUTOCOMPLETE_CHOICES = 25;
const MAX_CHOICE_NAME_LENGTH = 100;

/** A reply only the invoker sees. */
function ephemeral(content: string): Response {
  return jsonResponse({
    type: ResponseType.CHANNEL_MESSAGE_WITH_SOURCE,
    data: { flags: MessageFlags.EPHEMERAL, content, allowed_mentions: { parse: [] } },
  });
}

function lfgBuild(raw: string | undefined): Build {
  return raw !== undefined && isBuild(raw) ? raw : LFG_DEFAULT_BUILD;
}

/** `mode` if given; otherwise a picked `server` implies a dedicated-server game. */
function resolveMode(modeOption: string | undefined, serverOption: string | undefined): LfgMode | null {
  switch (modeOption) {
    case undefined:
      return serverOption === undefined ? "p2p" : "server";
    case "p2p":
    case "server":
      return modeOption;
    default:
      return null;
  }
}

/** `/lfg`: validates the request and posts the public LFG, or refuses ephemerally. */
export function lfgCommand(i: CommandInteraction, deps: LfgDeps): Response {
  const guildId = i.guild_id;
  const userId = invokerId(i);
  if (guildId === undefined || userId === undefined) {
    return ephemeral("Use /lfg in a server channel.");
  }
  const options = i.data.options;

  const serverOption = stringOption(options, "server");
  const modeOption = stringOption(options, "mode");
  const mode = resolveMode(modeOption, serverOption);
  if (mode === null) return ephemeral("Unknown mode.");
  if (mode === "p2p" && serverOption !== undefined) {
    return ephemeral("`server` only applies to dedicated-server games.");
  }

  const format = findFormat(stringOption(options, "format") ?? "");
  if (format === undefined) return ephemeral("Unknown format.");

  const cap = seatCap(format, mode);
  const seats = integerOption(options, "seats") ?? cap;
  if (seats < format.min_players || seats > cap) {
    const range = format.min_players === cap ? `${cap}` : `${format.min_players}–${cap}`;
    return ephemeral(`${format.label} ${mode === "p2p" ? "peer-to-peer " : ""}games take ${range} players.`);
  }

  const build = lfgBuild(stringOption(options, "build"));
  const availability = deps.servers.get(build);
  if (availability === null) {
    return ephemeral("Checking lobby status — try again in a few seconds.");
  }
  // The build's broker is the bot's only signal for its site's client version: a
  // pre-10 build's site ignores bot links, whichever mode would host the game.
  if (!brokerSupportsBotGames(availability.broker)) {
    return ephemeral(`The ${build} lobby doesn't support Discord games yet.`);
  }

  let server: { url: string; name: string } | null = null;
  if (mode === "server") {
    const eligible = eligibleServers(availability.broker, availability.servers);
    if (eligible.length === 0) return ephemeral("No dedicated server is currently available.");
    // The option is user-typeable, so only a URL from the verified directory is used.
    const row = serverOption === undefined ? eligible[0] : eligible.find((r) => r.url === serverOption);
    if (row === undefined) return ephemeral("That server isn't available for Discord games.");
    server = { url: row.url, name: row.name };
  }

  const result = deps.store.create(
    { guildId, creatorId: userId, format, seats, mode, build, server },
    deps.now(),
  );
  if (result.kind === "refused") return ephemeral(refusalText(result.reason, format));
  console.log(`[lfg] create id=${result.lfg.id} format=${format.format} mode=${mode} build=${build}`);
  return jsonResponse({
    type: ResponseType.CHANNEL_MESSAGE_WITH_SOURCE,
    data: renderLfg(result.lfg),
  });
}

function dispatch(
  store: LfgStore,
  action: LfgAction,
  id: string,
  guildId: string,
  userId: string,
  now: number,
): Outcome {
  switch (action) {
    case "join":
      return store.join(id, guildId, userId, now);
    case "leave":
      return store.leave(id, guildId, userId, now);
    case "start":
      return store.start(id, guildId, userId, now);
    case "link":
      return store.linkFor(id, guildId, userId, now);
    default: {
      const unreachable: never = action;
      throw new Error(`unknown lfg action ${unreachable}`);
    }
  }
}

/** A button on an LFG post. */
export function lfgComponent(
  i: ComponentInteraction,
  parsed: { action: LfgAction; id: string },
  deps: LfgDeps,
): Response {
  const guildId = i.guild_id;
  const userId = invokerId(i);
  if (guildId === undefined || userId === undefined) {
    return ephemeral("This button only works in a server channel.");
  }
  const outcome = dispatch(deps.store, parsed.action, parsed.id, guildId, userId, deps.now());
  console.log(`[lfg] ${parsed.action} id=${parsed.id} state=${outcome.lfg?.state ?? "gone"}`);

  switch (outcome.kind) {
    case "changed": {
      const response = jsonResponse({ type: ResponseType.UPDATE_MESSAGE, data: renderLfg(outcome.lfg) });
      if (outcome.becameReady) {
        void deps
          .followup(i.application_id, i.token, readyPing(outcome.lfg))
          .catch((err) => console.error(`[lfg] ready ping for ${parsed.id} failed:`, err));
      }
      return response;
    }
    case "ended":
      return jsonResponse({
        type: ResponseType.UPDATE_MESSAGE,
        data: outcome.lfg === null ? renderEnded() : renderLfg(outcome.lfg),
      });
    case "refused":
      return ephemeral(refusalText(outcome.reason, outcome.lfg.format));
    case "ready":
      return jsonResponse(linkReply(outcome.lfg, userId));
    default: {
      const unreachable: never = outcome;
      throw new Error(`unknown outcome ${JSON.stringify(unreachable)}`);
    }
  }
}

/** `/lfg server:` suggestions: the build's eligible servers, from the cache only. */
export function lfgAutocomplete(i: CommandInteraction, deps: LfgDeps): Response {
  const options = i.data.options;
  const focused = options?.find((o) => o.focused);
  let choices: Array<{ name: string; value: string }> = [];
  if (focused?.name === "server") {
    const availability = deps.servers.get(lfgBuild(stringOption(options, "build")));
    // A pre-10 build refuses every /lfg, so it offers no servers either.
    if (availability !== null && brokerSupportsBotGames(availability.broker)) {
      const q = (typeof focused.value === "string" ? focused.value : "").trim().toLowerCase();
      choices = eligibleServers(availability.broker, availability.servers)
        .filter((r) => r.name.toLowerCase().includes(q) || r.url.toLowerCase().includes(q))
        .slice(0, MAX_AUTOCOMPLETE_CHOICES)
        .map((r) => ({
          name: `${r.name} (${r.current_players} online)`.slice(0, MAX_CHOICE_NAME_LENGTH),
          value: r.url,
        }));
    }
  }
  return jsonResponse({
    type: ResponseType.APPLICATION_COMMAND_AUTOCOMPLETE_RESULT,
    data: { choices },
  });
}
