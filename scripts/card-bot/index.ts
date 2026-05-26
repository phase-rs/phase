// Discord HTTP-interactions server for the /card parse-breakdown bot.
//
// Flow: Discord POSTs each interaction here (behind nginx on 127.0.0.1). We
// verify the Ed25519 signature, then:
//   • PING            → PONG
//   • /card           → defer, then follow up with the parse embed
//   • autocomplete    → name suggestions from the (warm) default build
//
// Deferring the command guarantees we never hit Discord's 3s response window,
// even on a cold preview load or a slow Scryfall call.

import {
  DEFAULT_BUILD,
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
  type Interaction,
  type InteractionOption,
  InteractionType,
  ResponseType,
  editOriginalResponse,
  verifyRequest,
} from "./discord";
import { renderCardEmbed, renderNotFound } from "./render";
import { lookupScryfall, warmScryfall } from "./scryfall";

const PUBLIC_KEY = discord.publicKey();

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function optionValue(options: InteractionOption[] | undefined, name: string): string | undefined {
  const opt = options?.find((o) => o.name === name);
  return typeof opt?.value === "string" ? opt.value : undefined;
}

function resolveBuild(raw: string | undefined): Build {
  return raw && isBuild(raw) ? raw : DEFAULT_BUILD;
}

/** Builds and delivers the parse embed for a deferred /card interaction. */
async function deliverCard(interaction: Interaction): Promise<void> {
  const options = interaction.data?.options;
  const name = optionValue(options, "name")?.trim() ?? "";
  const build = resolveBuild(optionValue(options, "build"));

  const t0 = performance.now();
  try {
    const entry = await lookupCard(build, name);
    const t1 = performance.now();
    if (!entry) {
      await editOriginalResponse(interaction.application_id, interaction.token, {
        embeds: [renderNotFound(name, build)],
      });
      return;
    }

    const [scry, meta] = await Promise.all([
      lookupScryfall(entry.card_name),
      getMeta(build),
    ]);
    const tScry = performance.now();
    await editOriginalResponse(interaction.application_id, interaction.token, {
      embeds: [renderCardEmbed(entry, scry, build, meta)],
    });
    const t2 = performance.now();
    console.log(
      `[card] ${name} (${build}): lookup=${Math.round(t1 - t0)}ms scry+meta=${Math.round(tScry - t1)}ms send=${Math.round(t2 - tScry)}ms total=${Math.round(t2 - t0)}ms`,
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

/** Synchronous autocomplete: suggest names from the warm default build. */
async function autocomplete(interaction: Interaction): Promise<Response> {
  const focused = interaction.data?.options?.find((o) => o.focused);
  const query = typeof focused?.value === "string" ? focused.value : "";
  // Hold off until a couple of characters are typed — the lookup is in-memory
  // and cheap, but 0–1 chars just returns arbitrary names, not useful matches.
  const choices =
    query.trim().length < MIN_AUTOCOMPLETE_CHARS
      ? []
      : (await suggestNames(DEFAULT_BUILD, query)).map((n) => ({ name: n, value: n }));
  return json({
    type: ResponseType.APPLICATION_COMMAND_AUTOCOMPLETE_RESULT,
    data: { choices },
  });
}

async function handleInteraction(req: Request): Promise<Response> {
  const signature = req.headers.get("X-Signature-Ed25519");
  const timestamp = req.headers.get("X-Signature-Timestamp");
  const rawBody = await req.text();

  if (!(await verifyRequest(PUBLIC_KEY, signature, timestamp, rawBody))) {
    return new Response("invalid request signature", { status: 401 });
  }

  const interaction = JSON.parse(rawBody) as Interaction;

  if (interaction.type === InteractionType.PING) {
    return json({ type: ResponseType.PONG });
  }

  if (interaction.type === InteractionType.APPLICATION_COMMAND_AUTOCOMPLETE) {
    return autocomplete(interaction);
  }

  if (interaction.type === InteractionType.APPLICATION_COMMAND) {
    // Defer immediately; the follow-up edit carries the embed.
    void deliverCard(interaction);
    return json({ type: ResponseType.DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE });
  }

  return json({ error: "unsupported interaction type" }, 400);
}

// Warm the default build in the BACKGROUND, and start serving immediately so a
// restart has no closed-port window. A query that lands mid-warm dedupes onto
// the in-flight load (the deferred response covers the wait) instead of failing.
void warmDefaultBuild().catch((err) => console.error("warm-up failed:", err));
void warmScryfall().catch((err) => console.error("scryfall warm-up failed:", err));
startEvictionLoop();

Bun.serve({
  port: PORT,
  async fetch(req) {
    const { pathname } = new URL(req.url);
    if (req.method === "GET" && pathname === "/health") {
      return new Response("ok");
    }
    if (req.method === "POST") {
      return handleInteraction(req);
    }
    return new Response("not found", { status: 404 });
  },
});

console.log(`card-bot listening on :${PORT} (default build: ${DEFAULT_BUILD})`);
