// Discord HTTP-interactions server for the /card parse-breakdown bot.
//
// Flow: Discord POSTs each interaction here (behind nginx on 127.0.0.1). We
// verify the Ed25519 signature, then:
//   • PING            → PONG
//   • /card           → defer, then follow up with the parse embed
//   • /lfg            → post the LFG publicly (or refuse ephemerally), synchronously
//   • autocomplete    → /lfg: eligible dedicated servers; otherwise card names
//                       from the (warm) default build
//   • button          → /lfg Join / Leave / Start / Get my link, and the game
//                       thread's End game (lfgInteractions.ts)
//
// Deferring /card guarantees we never hit Discord's 3s response window, even on
// a cold preview load or a slow Scryfall call. /lfg needs only in-memory and
// sqlite state, so it answers directly.

import {
  DEFAULT_BUILD,
  LFG_DB_PATH,
  PORT,
  discord,
  isBuild,
  type Build,
} from "./config";
import {
  getMeta,
  lookupCard,
  startEvictionLoop,
  suggestNames,
  warmDefaultBuild,
} from "./coverageData";
import {
  type CommandInteraction,
  type Interaction,
  InteractionType,
  ResponseType,
  botThreadApi,
  createFollowupMessage,
  editOriginalResponse,
  jsonResponse,
  stringOption,
  verifyRequest,
} from "./discord";
import { LfgStore } from "./lfg";
import {
  closeThreads,
  type LfgDeps,
  lfgAutocomplete,
  lfgCommand,
  lfgComponent,
} from "./lfgInteractions";
import { parseCustomId } from "./lfgView";
import type { Embed } from "./render";
import {
  renderCardEmbed,
  renderFaceFallback,
  renderNotFound,
  renderTokenEmbed,
} from "./render";
import {
  type ScryfallCard,
  lookupScryfall,
  lookupToken,
  peekTokenNames,
  warmScryfall,
} from "./scryfall";
import { ServerCache } from "./servers";

function resolveBuild(raw: string | undefined): Build {
  return raw && isBuild(raw) ? raw : DEFAULT_BUILD;
}

/** Max faces rendered for one card. Discord allows 10 embeds; DFCs are 2–3. */
const MAX_FACES = 3;
/** Total text budget across all embeds in one Discord message. */
const TOTAL_EMBED_BUDGET = 5800;
/** Rough per-embed non-description overhead (author + footer + title). */
const EMBED_OVERHEAD = 80;

/**
 * Renders one embed per face of a multi-face card. Each face's parse tree comes
 * from its own coverage entry (DFC faces are separate coverage-data entries);
 * the shared description budget keeps the message under Discord's 6000-char cap.
 */
async function renderFaces(
  build: Build,
  scry: ScryfallCard,
  meta: Awaited<ReturnType<typeof getMeta>>,
): Promise<Embed[]> {
  const faceNames = scry.faceNames.slice(0, MAX_FACES);
  const perFaceBudget = Math.floor(TOTAL_EMBED_BUDGET / faceNames.length) - EMBED_OVERHEAD;

  const embeds: Embed[] = [];
  for (let i = 0; i < faceNames.length; i++) {
    const faceName = faceNames[i];
    const faceImage = scry.faceImages[i] ?? null;
    const faceEntry = await lookupCard(build, faceName);
    embeds.push(
      faceEntry
        ? renderCardEmbed(faceEntry, scry, build, meta, {
            faceImage,
            descriptionBudget: perFaceBudget,
          })
        : renderFaceFallback(faceName, faceImage, build, meta),
    );
  }
  return embeds;
}

/** Builds and delivers the parse embed for a deferred /card interaction. */
async function deliverCard(interaction: CommandInteraction): Promise<void> {
  const options = interaction.data.options;
  const name = stringOption(options, "name")?.trim() ?? "";
  const build = resolveBuild(stringOption(options, "build"));

  const t0 = performance.now();
  try {
    const entry = await lookupCard(build, name);
    const t1 = performance.now();
    if (!entry) {
      // Not a playable card — try tokens (Pilot, Treasure, …) before giving up.
      const [token, meta] = await Promise.all([lookupToken(name), getMeta(build)]);
      await editOriginalResponse(interaction.application_id, interaction.token, {
        embeds: [token ? renderTokenEmbed(token, build, meta) : renderNotFound(name, build)],
      });
      return;
    }

    const [scry, meta] = await Promise.all([
      lookupScryfall(entry.card_name),
      getMeta(build),
    ]);
    const tScry = performance.now();
    // Multi-face cards (transform / modal_dfc / split / adventure) get one embed
    // per face so both sides are visible; single-face cards get one embed.
    const embeds =
      scry && scry.faceNames.length > 1
        ? await renderFaces(build, scry, meta)
        : [renderCardEmbed(entry, scry, build, meta)];
    await editOriginalResponse(interaction.application_id, interaction.token, { embeds });
    const t2 = performance.now();
    console.log(
      `[card] ${name} (${build}): lookup=${Math.round(t1 - t0)}ms scry+meta=${Math.round(tScry - t1)}ms send=${Math.round(t2 - tScry)}ms total=${Math.round(t2 - t0)}ms faces=${embeds.length}`,
    );
  } catch (err) {
    console.error(`deliverCard(${name}, ${build}) failed:`, err);
    await editOriginalResponse(interaction.application_id, interaction.token, {
      content: `Something went wrong looking up **${name}**. Try again in a moment.`,
    }).catch(() => {});
  }
}

/** Suggestions only begin once this many characters are typed. */
const MIN_AUTOCOMPLETE_CHARS = 2;

/** Discord caps autocomplete at 25 choices; reserve a few for token matches. */
const MAX_AUTOCOMPLETE_CHOICES = 25;
const MAX_TOKEN_CHOICES = 5;

/** How often the timer looks for game threads to close (lfg.ts GAME_THREAD_MAX_MS). */
const THREAD_SWEEP_INTERVAL_MS = 10 * 60_000;

/** Synchronous autocomplete: suggest names from the warm default build. */
async function autocomplete(interaction: CommandInteraction): Promise<Response> {
  const focused = interaction.data.options?.find((o) => o.focused);
  const query = typeof focused?.value === "string" ? focused.value : "";
  const q = query.trim().toLowerCase();

  // Hold off until a couple of characters are typed — the lookup is in-memory
  // and cheap, but 0–1 chars just returns arbitrary names, not useful matches.
  let choices: Array<{ name: string; value: string }> = [];
  if (q.length >= MIN_AUTOCOMPLETE_CHARS) {
    // Token suggestions come from a non-blocking peek at the scryfall cache — the
    // 3s autocomplete window can't await the cold R2 load, so tokens simply
    // don't appear until the export has warmed. Value is the bare name so it
    // round-trips through the token fallback in deliverCard.
    const tokenChoices = peekTokenNames()
      .filter((n) => n.toLowerCase().includes(q))
      .slice(0, MAX_TOKEN_CHOICES)
      .map((n) => ({ name: `${n} (token)`, value: n }));
    const cardChoices = (await suggestNames(DEFAULT_BUILD, query))
      .map((n) => ({ name: n, value: n }))
      .slice(0, MAX_AUTOCOMPLETE_CHOICES - tokenChoices.length);
    choices = [...cardChoices, ...tokenChoices];
  }
  return jsonResponse({
    type: ResponseType.APPLICATION_COMMAND_AUTOCOMPLETE_RESULT,
    data: { choices },
  });
}

export interface InteractionDeps {
  publicKey: string;
  lfg: LfgDeps;
}

/** Verifies the signature (every interaction type), then routes by type and, for
 *  commands and autocomplete, by command name (`card` is the fallback). */
export async function handleInteraction(req: Request, deps: InteractionDeps): Promise<Response> {
  const signature = req.headers.get("X-Signature-Ed25519");
  const timestamp = req.headers.get("X-Signature-Timestamp");
  const rawBody = await req.text();

  if (!(await verifyRequest(deps.publicKey, signature, timestamp, rawBody))) {
    return new Response("invalid request signature", { status: 401 });
  }

  const interaction = JSON.parse(rawBody) as Interaction;

  switch (interaction.type) {
    case InteractionType.PING:
      return jsonResponse({ type: ResponseType.PONG });
    case InteractionType.APPLICATION_COMMAND_AUTOCOMPLETE:
      return interaction.data.name === "lfg"
        ? lfgAutocomplete(interaction, deps.lfg)
        : autocomplete(interaction);
    case InteractionType.APPLICATION_COMMAND:
      if (interaction.data.name === "lfg") return lfgCommand(interaction, deps.lfg);
      // Defer immediately; the follow-up edit carries the embed.
      void deliverCard(interaction);
      return jsonResponse({ type: ResponseType.DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE });
    case InteractionType.MESSAGE_COMPONENT: {
      const parsed = parseCustomId(interaction.data.custom_id);
      return parsed
        ? lfgComponent(interaction, parsed, deps.lfg)
        : jsonResponse({ error: "unknown component" }, 400);
    }
  }
  return jsonResponse({ error: "unsupported interaction type" }, 400);
}

if (import.meta.main) {
  // Warm the default build in the BACKGROUND, and start serving immediately so a
  // restart has no closed-port window. A query that lands mid-warm dedupes onto
  // the in-flight load (the deferred response covers the wait) instead of failing.
  void warmDefaultBuild().catch((err) => console.error("warm-up failed:", err));
  void warmScryfall().catch((err) => console.error("scryfall warm-up failed:", err));
  startEvictionLoop();

  const servers = new ServerCache();
  servers.start();
  const botToken = discord.tokenIfSet();
  const threads = botToken === undefined ? null : botThreadApi(botToken);
  const store = new LfgStore(LFG_DB_PATH);
  const deps: InteractionDeps = {
    publicKey: discord.publicKey(),
    lfg: {
      store,
      servers,
      now: Date.now,
      followup: createFollowupMessage,
      editOriginal: editOriginalResponse,
      threads,
    },
  };
  if (threads !== null) {
    setInterval(() => {
      void closeThreads(store, threads, Date.now()).catch((err) =>
        console.error("[lfg] thread sweep failed:", err),
      );
    }, THREAD_SWEEP_INTERVAL_MS);
  }

  Bun.serve({
    port: PORT,
    async fetch(req) {
      const { pathname } = new URL(req.url);
      if (req.method === "GET" && pathname === "/health") {
        return new Response("ok");
      }
      if (req.method === "POST") {
        return handleInteraction(req, deps);
      }
      return new Response("not found", { status: 404 });
    },
  });

  console.log(
    `card-bot listening on :${PORT} (default build: ${DEFAULT_BUILD}, game threads: ${threads === null ? "off, no CARD_BOT_TOKEN" : "on"})`,
  );
}
