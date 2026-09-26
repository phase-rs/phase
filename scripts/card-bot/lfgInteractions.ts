// /lfg interaction handlers: the slash command, its `server` autocomplete, the
// post's buttons, and the game thread's End game button and closing timer. Every
// input is in memory (the ServerCache snapshot) or synchronous (bun:sqlite), so
// each handler answers within Discord's 3 s window without deferring — which
// lets refusals be ephemeral and the post public.

import { isBuild, LFG_DEFAULT_BUILD, type Build } from "./config";
import {
  type CommandInteraction,
  type ComponentInteraction,
  DiscordHttpError,
  integerOption,
  invokerId,
  jsonResponse,
  MessageFlags,
  ResponseType,
  stringOption,
  type ThreadApi,
} from "./discord";
import { defaultSeats, findFormat, type LfgMode, seatCap } from "./formats";
import type { Lfg, LfgStore, Outcome } from "./lfg";
import {
  type LfgAction,
  linkReply,
  readyPing,
  refusalText,
  renderEnded,
  renderLfg,
  threadClosed,
  threadEnded,
  threadName,
  threadTimedOut,
  threadWelcome,
} from "./lfgView";
import { brokerSupportsBotGames, eligibleServers, type ServerCache } from "./servers";

export interface LfgDeps {
  store: LfgStore;
  servers: ServerCache;
  now: () => number;
  /** Posts a follow-up message on an interaction (createFollowupMessage in production). */
  followup: (appId: string, token: string, body: unknown) => Promise<void>;
  /** Edits the message a button was on (editOriginalResponse in production). */
  editOriginal: (appId: string, token: string, body: unknown) => Promise<void>;
  /** Game threads, or null when the bot runs without its token (no threads). */
  threads: ThreadApi | null;
}

/** Wait before closing a thread whose End game click is being answered, so the
 *  button's message update lands before the thread is archived and locked. */
const THREAD_CLOSE_DELAY_MS = 1000;

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
  const seats = integerOption(options, "seats") ?? defaultSeats(format, mode);
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
  action: Exclude<LfgAction, "end">,
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
  if (parsed.action === "end") return endGame(parsed.id, guildId, userId, deps);
  const outcome = dispatch(deps.store, parsed.action, parsed.id, guildId, userId, deps.now());
  console.log(`[lfg] ${parsed.action} id=${parsed.id} state=${outcome.lfg?.state ?? "gone"}`);

  switch (outcome.kind) {
    case "changed": {
      const response = jsonResponse({ type: ResponseType.UPDATE_MESSAGE, data: renderLfg(outcome.lfg) });
      if (outcome.becameReady) {
        void announceReady(i, outcome.lfg, deps).catch((err) =>
          console.error(`[lfg] ready announcement for ${parsed.id} failed:`, err),
        );
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

/**
 * Tells the players their game is ready: in a new private thread when the bot
 * can open one, otherwise with a ping under the post.
 */
async function announceReady(i: ComponentInteraction, lfg: Lfg, deps: LfgDeps): Promise<void> {
  const threadId = await openGameThread(i.channel_id, lfg, deps).catch((err) => {
    console.error(`[lfg] game thread for ${lfg.id} failed:`, err);
    return null;
  });
  if (threadId === null) {
    await deps.followup(i.application_id, i.token, readyPing(lfg));
    return;
  }
  // This edit follows the thread's own REST calls, so Discord has long since
  // processed the UPDATE_MESSAGE; if it fails, only the post's link is lost.
  await deps.editOriginal(i.application_id, i.token, renderLfg({ ...lfg, thread: { id: threadId, ended: false } }));
}

/** Opens the game's private thread with the seated players in it, or returns
 *  null when threads are off or the interaction has no channel. A thread whose
 *  setup fails after it exists is ended and closed before the error propagates,
 *  so the caller's ping replaces it (and the timer retries a failed close). */
async function openGameThread(channelId: string | undefined, lfg: Lfg, deps: LfgDeps): Promise<string | null> {
  const threads = deps.threads;
  if (threads === null || channelId === undefined) return null;
  const threadId = await threads.create(channelId, threadName(lfg));
  deps.store.attachThread(lfg.id, threadId);
  try {
    for (const userId of lfg.seated) await threads.addMember(threadId, userId);
    await threads.post(threadId, threadWelcome(lfg));
  } catch (err) {
    deps.store.endThread(lfg.id, deps.now());
    await closeThread(deps.store, threads, lfg.id, threadId, deps.now());
    throw err;
  }
  console.log(`[lfg] thread id=${lfg.id} players=${lfg.seated.length}`);
  return threadId;
}

/** Closes an ended game's thread in Discord and records it. A 4xx refusal (e.g.
 *  the bot's role lacks Manage Threads) is logged and recorded as closed, since
 *  retrying cannot fix it; any other failure is left for the timer's next pass. */
async function closeThread(
  store: LfgStore,
  threads: ThreadApi,
  id: string,
  threadId: string,
  now: number,
): Promise<void> {
  try {
    await threads.close(threadId);
  } catch (err) {
    if (!(err instanceof DiscordHttpError && err.status >= 400 && err.status < 500)) {
      console.error(`[lfg] closing thread for ${id} failed; retrying on the next pass:`, err);
      return;
    }
    console.error(`[lfg] closing thread for ${id} was refused; not retrying:`, err);
  }
  store.markThreadClosed(id, now);
}

/** The game thread's End game button: any seated player ends the game. */
function endGame(id: string, guildId: string, userId: string, deps: LfgDeps): Response {
  const result = deps.store.endGame(id, guildId, userId, deps.now());
  console.log(`[lfg] end id=${id} result=${result.kind}`);
  switch (result.kind) {
    case "ending": {
      const threads = deps.threads;
      if (threads !== null) {
        void Bun.sleep(THREAD_CLOSE_DELAY_MS)
          .then(() => closeThread(deps.store, threads, id, result.threadId, deps.now()))
          .catch((err) => console.error(`[lfg] closing thread for ${id} failed:`, err));
      }
      return jsonResponse({ type: ResponseType.UPDATE_MESSAGE, data: threadEnded(userId) });
    }
    case "refused":
      return ephemeral(refusalText(result.reason, result.lfg.format));
    case "ended":
      return jsonResponse({ type: ResponseType.UPDATE_MESSAGE, data: threadClosed() });
    default: {
      const unreachable: never = result;
      throw new Error(`unknown end result ${JSON.stringify(unreachable)}`);
    }
  }
}

/** The timer's pass: ends each thread past GAME_THREAD_MAX_MS, posting its
 *  notice once, then closes every ended thread Discord has not closed yet. */
export async function closeThreads(store: LfgStore, threads: ThreadApi, now: number): Promise<void> {
  for (const { id, threadId, timedOut } of store.threadsToClose(now)) {
    if (timedOut) {
      // Ended before the notice, so a failed notice is never re-posted.
      store.endThread(id, now);
      await threads
        .post(threadId, threadTimedOut())
        .catch((err) => console.error(`[lfg] timeout notice for ${id} failed:`, err));
      console.log(`[lfg] thread timed out id=${id}`);
    }
    await closeThread(store, threads, id, threadId, now);
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
