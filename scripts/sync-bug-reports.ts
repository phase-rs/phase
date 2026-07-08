#!/usr/bin/env bun

import { existsSync, readFileSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import type {
  PublishedThread,
  RawDiscordMessage,
  ReportItem,
  SyncState,
  TriageItem,
} from "./lib/types.ts";
import {
  addReaction,
  createMessage,
  discordGet,
  fetchActiveThreads,
  fetchArchivedThreads,
  fetchMessages,
  type DiscordThread,
  type DiscordMessage,
} from "./lib/discord.ts";
import { extractReports } from "./lib/extract.ts";
import { normalizeCardName } from "./lib/cardNames.ts";
import { triageReports } from "./lib/triage.ts";
import { renderDashboard, renderTriageDashboard } from "./lib/render.ts";
import { crossReference, type CrossrefItem } from "./lib/crossref.ts";

// ---------------------------------------------------------------------------
// JSONL helpers
// ---------------------------------------------------------------------------

function readJsonl<T>(path: string): T[] {
  if (!existsSync(path)) return [];
  const text = readFileSync(path, "utf8");
  return text
    .split("\n")
    .filter((l) => l.trim() !== "")
    .map((l) => JSON.parse(l) as T);
}

async function appendJsonl<T>(path: string, items: T[]): Promise<void> {
  if (items.length === 0) return;
  const lines = items.map((item) => JSON.stringify(item)).join("\n") + "\n";
  const existing = existsSync(path) ? readFileSync(path, "utf8") : "";
  await Bun.write(path, existing + lines);
}

async function writeJsonl<T>(path: string, items: T[]): Promise<void> {
  const lines = items.map((item) => JSON.stringify(item)).join("\n") + "\n";
  await Bun.write(path, lines);
}

// ---------------------------------------------------------------------------
// Sync state helpers
// ---------------------------------------------------------------------------

const SYNC_STATE_PATH = "triage/sync-state.json";
const MESSAGES_PATH = "triage/raw/discord-messages.jsonl";
const REPORT_ITEMS_PATH = "triage/report-items.jsonl";
const TRIAGE_ITEMS_PATH = "triage/triage-items.jsonl";
const TRIAGE_DELTA_PATH = "triage/triage-delta.jsonl";
const DASHBOARD_PATH = "triage/dashboard.md";
const LEGACY_EXPORT_PATH = "tmp/discord-thread-messages.json";
const CARD_DATA_PATH = "client/public/card-data.json";
const DISCORD_EPOCH_MS = 1420070400000n;

function defaultSyncState(): SyncState {
  return {
    last_fetch_at: new Date(0).toISOString(),
    prev_fetch_at: new Date(0).toISOString(),
    last_thread_cursors: {},
    imported_from_legacy: false,
    published_threads: {},
  };
}

async function loadSyncState(): Promise<SyncState> {
  if (!existsSync(SYNC_STATE_PATH)) return defaultSyncState();
  return (await Bun.file(SYNC_STATE_PATH).json()) as SyncState;
}

async function saveSyncState(state: SyncState): Promise<void> {
  await Bun.write(SYNC_STATE_PATH, JSON.stringify(state, null, 2) + "\n");
}

// ---------------------------------------------------------------------------
// Content hashing
// ---------------------------------------------------------------------------

function hashContent(content: string): string {
  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update(content);
  return hasher.digest("hex");
}

function snowflakeTimestampIso(id: string | null | undefined): string | null {
  if (id === null || id === undefined) return null;
  try {
    const timestampMs = (BigInt(id) >> 22n) + DISCORD_EPOCH_MS;
    return new Date(Number(timestampMs)).toISOString();
  } catch {
    return null;
  }
}

function parseFetchSinceArg(state: SyncState): string | undefined {
  if (process.argv.includes("--full")) return undefined;

  const sinceArg = process.argv
    .slice(3)
    .find((arg) => arg.startsWith("--since="));
  const since = sinceArg?.slice("--since=".length) ?? state.last_fetch_at;

  if (Number.isNaN(Date.parse(since))) {
    console.error(`Invalid --since timestamp: ${since}`);
    process.exit(1);
  }

  return since;
}

// ---------------------------------------------------------------------------
// Legacy import
// ---------------------------------------------------------------------------

interface LegacyThreadData {
  thread: {
    id: string;
    name: string;
    parent_id: string;
  };
  messages: Array<{
    id: string;
    timestamp: string;
    edited_timestamp: string | null;
    author: {
      id: string;
      username: string;
      global_name: string | null;
      bot?: boolean;
    };
    content: string;
    attachments?: Array<{
      id: string;
      filename: string;
      url: string;
      content_type?: string;
      size: number;
    }>;
    embeds?: Array<{
      title?: string;
      description?: string;
      url?: string;
      type?: string;
    }>;
    referenced_message_id?: string | null;
  }>;
}

async function importLegacy(
  guildId: string,
  channelId: string,
): Promise<{ imported: number; cursors: Record<string, string> }> {
  const raw = await Bun.file(LEGACY_EXPORT_PATH).json() as LegacyThreadData[];
  const fetched_at = new Date().toISOString();
  const messages: RawDiscordMessage[] = [];
  const cursors: Record<string, string> = {};

  for (const threadData of raw) {
    const thread = threadData.thread;
    let lastId: string | undefined;

    for (const msg of threadData.messages) {
      const content = msg.content ?? "";
      messages.push({
        source: "discord",
        guild_id: guildId,
        channel_id: channelId,
        thread_id: thread.id,
        thread_name: thread.name,
        message_id: msg.id,
        timestamp: msg.timestamp,
        edited_timestamp: msg.edited_timestamp ?? null,
        author_id: msg.author?.id ?? "",
        author_name: msg.author?.global_name ?? msg.author?.username ?? "",
        author_is_bot: msg.author?.bot ?? false,
        content,
        attachments: (msg.attachments ?? []).map((a) => ({
          id: a.id,
          filename: a.filename,
          url: a.url,
          content_type: a.content_type ?? null,
          size: a.size,
        })),
        embeds: (msg.embeds ?? []).map((e) => ({
          title: e.title ?? null,
          description: e.description ?? null,
          url: e.url ?? null,
          type: e.type ?? null,
        })),
        referenced_message_id: msg.referenced_message_id ?? null,
        fetched_at,
        content_hash: hashContent(content),
      });
      lastId = msg.id;
    }

    if (lastId !== undefined) {
      cursors[thread.id] = lastId;
    }
  }

  await writeJsonl(MESSAGES_PATH, messages);
  return { imported: messages.length, cursors };
}

// ---------------------------------------------------------------------------
// Incremental fetch
// ---------------------------------------------------------------------------

async function fetchIncremental(
  guildId: string,
  channelId: string,
  cursors: Record<string, string>,
  since?: string,
): Promise<{ newMessages: number; cursors: Record<string, string> }> {
  let active: DiscordThread[] = [];
  try {
    active = await fetchActiveThreads(guildId, channelId);
  } catch (err) {
    console.error(`Warning: could not fetch active threads: ${(err as Error).message}`);
  }

  let publicArchived: DiscordThread[] = [];
  try {
    publicArchived = await fetchArchivedThreads(channelId, "public", since);
  } catch (err) {
    console.error(`Warning: could not fetch public archived threads: ${(err as Error).message}`);
  }

  let privateArchived: DiscordThread[] = [];
  try {
    privateArchived = await fetchArchivedThreads(channelId, "private", since);
  } catch (err) {
    console.error(`Skipping private archived threads: ${(err as Error).message}`);
  }

  const threadsById = new Map<string, DiscordThread>();
  for (const t of [...active, ...publicArchived, ...privateArchived]) {
    threadsById.set(t.id, t);
  }

  const fetched_at = new Date().toISOString();
  const newMessages: RawDiscordMessage[] = [];
  const updatedCursors = { ...cursors };
  const candidateThreads = [...threadsById.values()].filter((thread) => {
    if (since === undefined) return true;
    if (cursors[thread.id] === undefined) return true;
    const lastMessageAt = snowflakeTimestampIso(thread.last_message_id);
    return lastMessageAt === null || lastMessageAt > since;
  });

  for (const [i, thread] of candidateThreads.entries()) {
    const after = cursors[thread.id];
    let msgs: DiscordMessage[] = [];
    try {
      msgs = await fetchMessages(thread.id, after);
    } catch (err) {
      console.error(
        `Warning: could not fetch messages for thread ${thread.name}: ${(err as Error).message}`,
      );
      continue;
    }

    if (msgs.length > 0) {
      const threadName =
        (threadsById.get(thread.id)?.name) ?? thread.id;

      for (const msg of msgs) {
        const content = msg.content ?? "";
        newMessages.push({
          source: "discord",
          guild_id: guildId,
          channel_id: channelId,
          thread_id: thread.id,
          thread_name: threadName,
          message_id: msg.id,
          timestamp: msg.timestamp,
          edited_timestamp: msg.edited_timestamp,
          author_id: msg.author.id,
          author_name: msg.author.global_name ?? msg.author.username,
          author_is_bot: msg.author.bot ?? false,
          content,
          attachments: msg.attachments,
          embeds: msg.embeds,
          referenced_message_id: msg.referenced_message_id,
          fetched_at,
          content_hash: hashContent(content),
        });
      }

      updatedCursors[thread.id] = msgs.at(-1)!.id;
    }

    process.stdout.write(
      `\rFetching thread ${i + 1}/${candidateThreads.length}: ${thread.name.slice(0, 40).padEnd(40)}`,
    );
  }

  if (candidateThreads.length > 0) process.stdout.write("\n");
  if (since !== undefined) {
    console.log(
      `  Skipped ${threadsById.size - candidateThreads.length} threads with no activity after ${since}.`,
    );
  }

  if (newMessages.length > 0) {
    await appendJsonl(MESSAGES_PATH, newMessages);
  }

  return { newMessages: newMessages.length, cursors: updatedCursors };
}

// ---------------------------------------------------------------------------
// Fetch-window delta
// ---------------------------------------------------------------------------

const EPOCH = new Date(0).toISOString();

/** Message ids belonging to the most recent fetch window.
 *
 *  Steady state: every message with `fetched_at > prev_fetch_at`. Each `fetch`
 *  run stamps all of its new messages with one `fetched_at` and rolls
 *  `prev_fetch_at` forward, so this is exactly the last run's batch.
 *
 *  Migration fallback (no `prev_fetch_at`, or epoch): the single most recent
 *  `fetched_at` value identifies the last batch on its own. */
function latestFetchMessageIds(
  messages: RawDiscordMessage[],
  prevFetchAt: string | undefined,
): Set<string> {
  if (messages.length === 0) return new Set();
  if (prevFetchAt !== undefined && prevFetchAt !== EPOCH) {
    return new Set(
      messages.filter((m) => m.fetched_at > prevFetchAt).map((m) => m.message_id),
    );
  }
  const maxFetchedAt = messages.reduce(
    (max, m) => (m.fetched_at > max ? m.fetched_at : max),
    "",
  );
  return new Set(
    messages.filter((m) => m.fetched_at === maxFetchedAt).map((m) => m.message_id),
  );
}

/** Emit triage/triage-delta.jsonl — the triage items from the latest fetch
 *  window only. This is the slice a reviewer reads each cycle; it never grows
 *  with the archive. Every non-`skip` delta item is one the LLM operator must
 *  resolve this cycle — file a GH issue, link/dup it to an existing one, or
 *  mark it handled. The script does NOT pre-judge duplication; the operator is
 *  the sole arbiter of checking a report against existing issues. */
async function writeTriageDelta(items: TriageItem[]): Promise<void> {
  const messages = readJsonl<RawDiscordMessage>(MESSAGES_PATH);
  const state = await loadSyncState();
  const deltaIds = latestFetchMessageIds(messages, state.prev_fetch_at);
  const delta = items.filter((it) => deltaIds.has(it.message_id));
  await writeJsonl(TRIAGE_DELTA_PATH, delta);

  const toResolve = delta.filter((it) => it.proposed_action !== "skip");
  const byAction = new Map<string, number>();
  for (const it of delta) {
    byAction.set(it.proposed_action, (byAction.get(it.proposed_action) ?? 0) + 1);
  }

  console.log(`  ---`);
  console.log(`  Delta (new since last fetch): ${delta.length} items → ${TRIAGE_DELTA_PATH}`);
  for (const action of [...byAction.keys()].sort()) {
    console.log(`    ${action}: ${byAction.get(action)}`);
  }
  console.log(`  Reports to resolve this cycle (REVIEW THESE): ${toResolve.length}`);
  for (const o of toResolve) {
    console.log(`    - ${o.thread_name}  [${o.classification} / ${o.proposed_action}]`);
  }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

async function cmdFetch(): Promise<void> {
  await mkdir("triage/raw", { recursive: true });

  const guildId = Bun.env.DISCORD_GUILD_ID;
  const channelId = Bun.env.DISCORD_CHANNEL_ID;
  const token = Bun.env.DISCORD_BOT_TOKEN;

  if (!token || !guildId || !channelId) {
    console.error(
      "Error: DISCORD_BOT_TOKEN, DISCORD_GUILD_ID, and DISCORD_CHANNEL_ID must be set in .env",
    );
    process.exit(1);
  }

  let state = await loadSyncState();

  // Import legacy export if not yet done
  if (!state.imported_from_legacy && existsSync(LEGACY_EXPORT_PATH)) {
    console.log(`Importing legacy export from ${LEGACY_EXPORT_PATH}...`);
    const { imported, cursors } = await importLegacy(guildId, channelId);
    state.imported_from_legacy = true;
    state.last_thread_cursors = cursors;
    console.log(`  Imported ${imported} messages from legacy export.`);
  }

  // Incremental fetch from Discord API
  const since = parseFetchSinceArg(state);
  const mode = since === undefined ? "full scan" : `activity after ${since}`;
  console.log(`Fetching new messages from Discord API (${mode})...`);
  const { newMessages, cursors } = await fetchIncremental(
    guildId,
    channelId,
    state.last_thread_cursors,
    since,
  );

  state.last_thread_cursors = cursors;
  // Roll the delta watermark forward: the run that just completed becomes the
  // "previous" boundary, so the NEXT triage emits exactly this run's messages.
  state.prev_fetch_at = state.last_fetch_at;
  state.last_fetch_at = new Date().toISOString();
  await saveSyncState(state);

  const total = readJsonl<RawDiscordMessage>(MESSAGES_PATH).length;
  console.log(`Fetch complete.`);
  console.log(`  New messages fetched: ${newMessages}`);
  console.log(`  Total messages in store: ${total}`);
  console.log(`  Sync state saved to ${SYNC_STATE_PATH}`);
}

async function cmdExtract(): Promise<void> {
  const messages = readJsonl<RawDiscordMessage>(MESSAGES_PATH);
  if (messages.length === 0) {
    console.error(`No messages found at ${MESSAGES_PATH}. Run 'fetch' first.`);
    process.exit(1);
  }

  console.log(`Extracting reports from ${messages.length} messages...`);
  const reports = await extractReports(messages);

  await mkdir("triage", { recursive: true });
  await writeJsonl(REPORT_ITEMS_PATH, reports);

  // Stats
  const bands = new Map<string, number>();
  let cardsDetected = 0;
  for (const r of reports) {
    const band =
      r.extraction_confidence >= 0.9
        ? "high"
        : r.extraction_confidence >= 0.7
          ? "medium"
          : r.extraction_confidence >= 0.5
            ? "low-medium"
            : r.extraction_confidence >= 0.3
              ? "low"
              : "very low";
    bands.set(band, (bands.get(band) ?? 0) + 1);
    if (r.cards.length > 0) cardsDetected++;
  }

  const botCount = messages.filter((m) => m.author_is_bot).length;

  console.log(`Extraction complete.`);
  console.log(`  Total report items: ${reports.length}`);
  console.log(`  Bot messages excluded: ${botCount}`);
  console.log(`  Items with card names: ${cardsDetected}`);
  for (const [band, count] of [...bands.entries()]) {
    console.log(`  Confidence ${band}: ${count}`);
  }
  console.log(`  Written to ${REPORT_ITEMS_PATH}`);
}

async function cmdTriage(): Promise<void> {
  const reports = readJsonl<ReportItem>(REPORT_ITEMS_PATH);
  if (reports.length === 0) {
    console.error(`No report items found at ${REPORT_ITEMS_PATH}. Run 'extract' first.`);
    process.exit(1);
  }

  console.log(`Triaging ${reports.length} report items...`);
  const items = await triageReports(reports);

  await mkdir("triage", { recursive: true });
  await writeJsonl(TRIAGE_ITEMS_PATH, items);

  // Build stats
  const byClass = new Map<string, number>();
  const byAction = new Map<string, number>();
  const byParserStatus = new Map<string, number>();

  for (const item of items) {
    byClass.set(item.classification, (byClass.get(item.classification) ?? 0) + 1);
    byAction.set(item.proposed_action, (byAction.get(item.proposed_action) ?? 0) + 1);
    byParserStatus.set(item.parser_status, (byParserStatus.get(item.parser_status) ?? 0) + 1);
  }

  console.log(`Triage complete.`);
  console.log(`  Total items: ${items.length}`);
  console.log(`  Primary reports: ${byClass.get("primary_report") ?? 0}`);
  console.log(`  Additional reports: ${byClass.get("additional_report") ?? 0}`);
  console.log(`  Follow-ups: ${byClass.get("follow_up") ?? 0}`);
  console.log(`  Developer replies: ${byClass.get("developer_reply") ?? 0}`);
  console.log(`  Corrections: ${byClass.get("correction") ?? 0}`);
  console.log(`  Chatter: ${byClass.get("chatter") ?? 0}`);
  console.log(`  Evidence-only: ${byClass.get("evidence_only") ?? 0}`);
  console.log(`  ---`);
  for (const action of [...byAction.keys()].sort()) {
    console.log(`  Proposed: ${action}: ${byAction.get(action) ?? 0}`);
  }
  console.log(`  ---`);
  console.log(
    `  Parser status: fully_parsed: ${byParserStatus.get("fully_parsed") ?? 0}, ` +
    `has_gaps: ${byParserStatus.get("has_gaps") ?? 0}, ` +
    `unknown_card: ${byParserStatus.get("unknown_card") ?? 0}, ` +
    `no_card: ${byParserStatus.get("no_card") ?? 0}`,
  );
  console.log(`  Written to ${TRIAGE_ITEMS_PATH}`);

  await writeTriageDelta(items);
}

async function cmdDelta(): Promise<void> {
  // Re-emit triage/triage-delta.jsonl from the existing triage-items.jsonl
  // without re-running classification. Useful to inspect the latest fetch
  // window's new reports on demand.
  const items = readJsonl<TriageItem>(TRIAGE_ITEMS_PATH);
  if (items.length === 0) {
    console.error(`No triage items at ${TRIAGE_ITEMS_PATH}. Run 'triage' first.`);
    process.exit(1);
  }
  await mkdir("triage", { recursive: true });
  console.log(`Computing fetch-window delta from ${items.length} triage items...`);
  await writeTriageDelta(items);
}

// ---------------------------------------------------------------------------
// Publish: create GitHub issues + write back to Discord (👀 + link reply)
// ---------------------------------------------------------------------------

const TRACKED_REPLY_PREFIX = "🔗 Tracked in";
const REACTION_EMOJI = "👀";
const ISSUE_REPO = "phase-rs/phase";

interface CardDataEntry {
  oracle_text?: string | null;
}

let cardDataCache: Record<string, CardDataEntry> | null = null;
let rawMessagesCache: RawDiscordMessage[] | null = null;

function loadCardData(): Record<string, CardDataEntry> {
  if (cardDataCache === null) {
    cardDataCache = existsSync(CARD_DATA_PATH)
      ? (JSON.parse(readFileSync(CARD_DATA_PATH, "utf8")) as Record<string, CardDataEntry>)
      : {};
  }
  return cardDataCache;
}

function loadRawMessages(): RawDiscordMessage[] {
  if (rawMessagesCache === null) {
    rawMessagesCache = readJsonl<RawDiscordMessage>(MESSAGES_PATH);
  }
  return rawMessagesCache;
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function containsCardReference(text: string, card: string, allowSingleWord: boolean): boolean {
  const bracketed = new RegExp(`\\[\\[\\s*${escapeRegex(card)}\\s*\\]\\]`, "i");
  if (bracketed.test(text)) return true;

  // Punctuation-tolerant bracket match: the card-data key can spell punctuation
  // differently from what the user typed (key "welcome to . . ." vs the message
  // "[[welcome to...]]"). Compare normalized forms of every [[...]] reference.
  const normalizedCard = normalizeCardName(card);
  if (normalizedCard !== "") {
    for (const match of text.matchAll(/\[\[(.+?)\]\]/g)) {
      if (normalizeCardName(match[1]) === normalizedCard) return true;
    }
  }

  const cardWords = card.trim().split(/\s+/);
  if (!allowSingleWord && cardWords.length === 1) return false;

  const phrase = new RegExp(`(^|[^\\p{L}\\p{N}])${escapeRegex(card)}([^\\p{L}\\p{N}]|$)`, "iu");
  return phrase.test(text);
}

function selectRelevantOracleCards(item: TriageItem): string[] {
  const rawText = loadRawMessages()
    .filter((message) => message.thread_id === item.thread_id)
    .map((message) => message.content)
    .join("\n");
  const searchableText = `${item.thread_name}\n${item.summary}\n${rawText}`;
  const threadTitle = item.thread_name.trim().toLowerCase();
  // Cards named explicitly via [[...]] / Scryfall links are trusted — they were
  // resolved against card-data at extraction time, so they bypass the text
  // re-scan (which can't re-match a truncated/punctuation-variant reference).
  const explicit = new Set((item.explicitCards ?? []).map((card) => card.trim().toLowerCase()));

  return [...new Set(item.cards.map((card) => card.trim()).filter(Boolean))].filter((card) => {
    const normalized = card.toLowerCase();
    if (explicit.has(normalized)) return true;
    const exactThreadTitle = normalized === threadTitle;
    return exactThreadTitle || containsCardReference(searchableText, card, false);
  });
}

function buildVerifiedOracleTextSection(item: TriageItem): string[] {
  const cards = selectRelevantOracleCards(item);
  const uniqueCards = [...new Set(cards.map((card) => card.trim()).filter(Boolean))];
  if (uniqueCards.length === 0) {
    return [
      `## Oracle text (verified from \`${CARD_DATA_PATH}\`)`,
      `_No card names were detected for this report._`,
    ];
  }

  const cardData = loadCardData();
  const lines = [`## Oracle text (verified from \`${CARD_DATA_PATH}\`)`];
  for (const card of uniqueCards) {
    const entry = cardData[card.toLowerCase()];
    lines.push(`### ${card}`);
    if (entry?.oracle_text === undefined || entry.oracle_text === null || entry.oracle_text === "") {
      lines.push(`_No exact card-data entry with Oracle text was found for \`${card}\`._`);
      continue;
    }
    lines.push(entry.oracle_text);
  }
  return lines;
}

// NOTE: this module deliberately does NOT pre-judge threads. The orchestrator
// (an LLM in chat) reads candidate threads, cross-references existing GH issues,
// and decides per thread whether to file, dedupe, skip-resolved, etc. This
// script's job is mechanics: list candidates, expose raw messages, create
// issues on instruction, react/reply on instruction, persist state.


// Mechanics-only: pick the primary_report for a thread, or fall back to the
// first item. The orchestrator (LLM) has already decided to publish this
// thread by listing it in --thread; this function only chooses WHICH item's
// summary/cards to use for the issue body. It does not gate.
function pickPublishItem(items: TriageItem[]): TriageItem | null {
  if (items.length === 0) return null;
  return items.find((it) => it.classification === "primary_report") ?? items[0];
}

function buildIssueTitle(item: TriageItem): string {
  // Thread title is consistently more readable than the cards array, which
  // routinely contains false-positive single-word matches ("life", "x", "give")
  // alongside real card names. Use the thread title as the canonical prefix;
  // the cards array stays in the body for search/grep visibility.
  const prefix = item.thread_name.trim() || "Bug report";
  const summary = item.summary.replace(/\s+/g, " ").trim();
  const max = 120;
  const raw = summary === "" ? prefix : `${prefix} — ${summary}`;
  return raw.length <= max ? raw : `${raw.slice(0, max - 1).trimEnd()}…`;
}

function buildIssueBody(item: TriageItem): string {
  const relevantCards = selectRelevantOracleCards(item);
  // Keep the Discord source URL anchor in the body — it is the stable handle the
  // LLM operator greps for when checking whether a report was already filed.
  const lines = [
    `<!-- phase-discord-thread-id: ${item.thread_id} -->`,
    `<!-- phase-discord-message-id: ${item.message_id} -->`,
    ``,
    `Reported in Discord: ${item.source_url}`,
    ``,
    `**Thread:** ${item.thread_name}`,
    `**Discord thread id:** \`${item.thread_id}\``,
    `**Discord message id:** \`${item.message_id}\``,
    `**Cards:** ${relevantCards.length > 0 ? relevantCards.join(", ") : "_none detected_"}`,
    `**Parser status:** ${item.parser_status}`,
    `**Extraction confidence:** ${item.extraction_confidence.toFixed(2)}`,
    ``,
    `## Summary`,
    item.summary || "_(no summary extracted — see Discord thread)_",
    ``,
    ...buildVerifiedOracleTextSection(item),
    ``,
    `---`,
    `<sub>report_id: \`${item.report_id}\` · discord: \`${item.thread_id}/${item.message_id}\`</sub>`,
  ];
  return lines.join("\n");
}

interface CreatedIssue {
  number: number;
  url: string;
}

function createGithubIssue(item: TriageItem): CreatedIssue {
  const title = buildIssueTitle(item);
  const body = buildIssueBody(item);
  // `classifier:pending` marks the issue as awaiting LLM analysis (AST-vs-Oracle
  // faithfulness check per .claude/skills/bug-coverage-classifier). The classifier
  // pass replaces `pending` with one of the four verdict labels and posts the
  // reasoning comment. Greppable backlog: `gh issue list -l classifier:pending`.
  const labels = ["source:discord", "status:needs-triage", "classifier:pending"];

  const result = Bun.spawnSync([
    "gh",
    "issue",
    "create",
    "--repo",
    ISSUE_REPO,
    "--title",
    title,
    "--body",
    body,
    "--label",
    labels.join(","),
  ]);

  if (result.exitCode !== 0) {
    throw new Error(
      `gh issue create failed: ${result.stderr.toString() || result.stdout.toString()}`,
    );
  }

  const url = result.stdout.toString().trim().split("\n").at(-1)?.trim() ?? "";
  const match = url.match(/\/issues\/(\d+)$/);
  if (match === null) {
    throw new Error(`Could not parse issue number from gh output: ${url}`);
  }
  return { number: Number(match[1]), url };
}

// Phase 1 of publish: create the GitHub issue. Always succeeds-or-throws
// *before* any Discord side-effects, so the caller can persist the GH record
// before risking the Discord call. The operator has already decided this thread
// has no existing issue (the script does not dedup against GitHub).
function resolveIssue(
  item: TriageItem,
  dryRun: boolean,
): { number: number; url: string } {
  if (dryRun) {
    console.log(`    [dry-run] would: gh issue create --title "${buildIssueTitle(item)}"`);
    return { number: 0, url: "https://github.com/phase-rs/phase/issues/DRY" };
  }
  const issue = createGithubIssue(item);
  console.log(`    created issue #${issue.number}: ${issue.url}`);
  return issue;
}

// Phase 2 of publish: react 👀 on the thread starter, post the tracking link.
// Does NOT auto-unarchive: at PUBLISH time a live thread is expected, so an
// already-archived thread means the operator archived it between the LLM's
// judgment and the publish call — that's a strong "skip, leave it alone"
// signal, and the caller logs the Discord error 50083 without touching the
// archive state.
//
// NOTE: archiving means "resolved" here, but the two ends of the pipeline reach
// it differently. At publish (issue OPEN) the thread should stay live, so we
// never archive and treat an archive as a manual skip. At issue CLOSE the loop
// is done, so `scripts/notify-discord-issue-closed.ts` DOES archive the thread
// (unarchiving first if needed) to mark the report resolved. Same end-state,
// opposite trigger — don't "fix" this asymmetry into a contradiction.
async function writeDiscordTracking(
  threadId: string,
  issueUrl: string,
  dryRun: boolean,
): Promise<string> {
  const replyContent = `${TRACKED_REPLY_PREFIX} ${issueUrl}`;
  if (dryRun) {
    console.log(`    [dry-run] would: react ${REACTION_EMOJI} on ${threadId}`);
    console.log(`    [dry-run] would: post "${replyContent}" in ${threadId}`);
    return "DRY";
  }
  await addReaction(threadId, threadId, REACTION_EMOJI);
  const posted = await createMessage(threadId, replyContent);
  return posted.id;
}

function hasDiscordWriteBack(record: PublishedThread): boolean {
  return (
    record.issue_number > 0 &&
    record.issue_url !== "" &&
    record.reacted_message_id !== "" &&
    record.reply_message_id !== ""
  );
}

async function cmdMarkHandled(): Promise<void> {
  //  --thread=<id>[,<id>...]      tag specific threads (e.g. "dup of #406")
  // Stores a sentinel record so `pending` never resurfaces the thread.
  // NOTE: the bulk `--until-thread=<watermark>` form was REMOVED. It once
  // swept ~368 threads into sentinels in a single run, silently suppressing
  // unresolved bugs along with resolved ones. Mark threads one at a time,
  // intentionally — never in bulk by timestamp.
  const argv = process.argv.slice(3);
  const threadArgs = argv
    .filter((a: string) => a.startsWith("--thread="))
    .flatMap((a: string) => a.slice("--thread=".length).split(",").map((s: string) => s.trim()))
    .filter((s: string) => s !== "");
  const notesArg = argv.find((a: string) => a.startsWith("--notes="));
  const notes = notesArg !== undefined ? notesArg.slice("--notes=".length) : undefined;
  const dryRun = argv.includes("--dry-run");

  if (threadArgs.length === 0) {
    console.error(
      "Usage:\n" +
        "  mark-handled --thread=<id>[,<id>...] [--notes='dup of #406'] [--dry-run]",
    );
    process.exit(1);
  }

  const rawMessages = readJsonl<RawDiscordMessage>(MESSAGES_PATH);
  const lastByThread = new Map<string, string>();
  for (const m of rawMessages) {
    const prev = lastByThread.get(m.thread_id);
    if (prev === undefined || m.timestamp > prev) lastByThread.set(m.thread_id, m.timestamp);
  }

  // Resolve the set of thread ids to mark.
  const targets = new Set<string>();
  for (const tid of threadArgs) {
    if (!lastByThread.has(tid)) {
      console.error(`Warning: thread ${tid} not in local messages — recording anyway.`);
    }
    targets.add(tid);
  }

  const state = await loadSyncState();
  const published = { ...(state.published_threads ?? {}) };
  const stamp = new Date().toISOString();

  let added = 0;
  let alreadyTracked = 0;
  for (const threadId of targets) {
    if (published[threadId] !== undefined) {
      alreadyTracked++;
      continue;
    }
    published[threadId] = {
      issue_number: 0,
      issue_url: "",
      reacted_message_id: "",
      reply_message_id: "",
      published_at: stamp,
      mode: "reconciled",
      ...(notes !== undefined ? { notes } : {}),
    };
    added++;
  }

  console.log(`  targets:           ${targets.size}`);
  console.log(`  newly recorded:    ${added}`);
  console.log(`  already tracked:   ${alreadyTracked}`);

  if (dryRun) {
    console.log("  (dry-run — sync-state.json not modified)");
    return;
  }

  state.published_threads = published;
  await saveSyncState(state);
  console.log(`  sync state:        ${SYNC_STATE_PATH}`);
}

async function cmdPending(): Promise<void> {
  // Mechanics only: enumerate unpublished threads with optional --since
  // watermark. NO verdict/staleness/dev-reply scanning — those judgments
  // belong to the LLM driving this script.
  const argv = process.argv.slice(3);
  const limitArg = argv.find((a: string) => a.startsWith("--limit="));
  const limit = limitArg !== undefined ? Number(limitArg.slice("--limit=".length)) : 50;
  const sinceArg = argv.find((a: string) => a.startsWith("--since="));
  const sinceThreadId = sinceArg !== undefined ? sinceArg.slice("--since=".length) : null;

  const state = await loadSyncState();
  const published = new Set(Object.keys(state.published_threads ?? {}));
  const rawMessages = readJsonl<RawDiscordMessage>(MESSAGES_PATH);

  let sinceTs: string | null = null;
  if (sinceThreadId !== null) {
    const watermark = rawMessages
      .filter((m) => m.thread_id === sinceThreadId)
      .map((m) => m.timestamp)
      .sort()
      .at(-1);
    if (watermark === undefined) {
      console.error(`Error: --since=${sinceThreadId} not found in local messages.`);
      process.exit(1);
    }
    sinceTs = watermark;
    console.log(`Watermark thread ${sinceThreadId} last activity: ${sinceTs}`);
  }

  interface Row {
    thread_id: string;
    thread_name: string;
    last_ts: string;
    message_count: number;
  }
  const byThread = new Map<string, Row>();
  for (const m of rawMessages) {
    const existing = byThread.get(m.thread_id);
    if (existing === undefined) {
      byThread.set(m.thread_id, {
        thread_id: m.thread_id, thread_name: m.thread_name,
        last_ts: m.timestamp, message_count: 1,
      });
    } else {
      existing.message_count++;
      if (m.timestamp > existing.last_ts) existing.last_ts = m.timestamp;
    }
  }

  const rows = [...byThread.values()]
    .filter((r) => !published.has(r.thread_id))
    .filter((r) => sinceTs === null || r.last_ts > sinceTs)
    .sort((a, b) => b.last_ts.localeCompare(a.last_ts));

  console.log(`Pending threads (local-state diff vs published_threads)`);
  console.log(`  total cursors:    ${Object.keys(state.last_thread_cursors ?? {}).length}`);
  console.log(`  published:        ${published.size}`);
  console.log(`  pending (shown):  ${Math.min(rows.length, limit)} of ${rows.length}`);
  console.log("");

  for (const r of rows.slice(0, limit)) {
    console.log(
      `  ${r.thread_id}  ${r.last_ts.slice(0, 10)}  msgs=${String(r.message_count).padStart(3)}  ${r.thread_name}`,
    );
  }

  if (rows.length > 0) {
    console.log("");
    console.log("Read a thread inline:");
    console.log(`  bun scripts/sync-bug-reports.ts read --thread=${rows[0].thread_id}`);
  }
}

async function cmdReadThread(): Promise<void> {
  // Dump the full message stream of a thread so the LLM can read and judge it.
  // Output is plain text, oldest-first.
  const argv = process.argv.slice(3);
  const threadArg = argv.find((a: string) => a.startsWith("--thread="));
  if (threadArg === undefined) {
    console.error("Usage: read --thread=<thread_id>");
    process.exit(1);
  }
  const threadId = threadArg.slice("--thread=".length);

  const rawMessages = readJsonl<RawDiscordMessage>(MESSAGES_PATH);
  const msgs = rawMessages
    .filter((m) => m.thread_id === threadId)
    .sort((a, b) => a.timestamp.localeCompare(b.timestamp));

  if (msgs.length === 0) {
    console.error(`No messages found for thread ${threadId}.`);
    process.exit(1);
  }

  console.log(`Thread: ${msgs[0].thread_name}  (${msgs.length} messages)`);
  console.log(`Id:     ${threadId}`);
  console.log("");
  for (const m of msgs) {
    const tag = m.author_is_bot ? "[bot] " : "";
    console.log(`── ${m.timestamp.slice(0, 19)}  ${tag}${m.author_name}`);
    if (m.content !== "") console.log(m.content);
    if (m.attachments.length > 0) {
      console.log(`   attachments: ${m.attachments.map((a) => a.filename).join(", ")}`);
    }
    console.log("");
  }
}

async function cmdPublish(): Promise<void> {
  // Mechanics only: the LLM driving this script has decided which threads to
  // publish and lists them via --thread. No verdicts, no sweeping, no
  // candidate scoring. The script's job: create the GH issue and write back to
  // Discord. The only local-state gate is published_threads (so retries on
  // partial failures don't double-file).
  const argv = process.argv.slice(3);
  const dryRun = argv.includes("--dry-run");

  const targets = new Set<string>(
    argv
      .filter((a: string) => a.startsWith("--thread="))
      .flatMap((a: string) => a.slice("--thread=".length).split(",").map((s: string) => s.trim()))
      .filter((s: string) => s !== ""),
  );

  if (targets.size === 0) {
    console.error(
      "Usage: publish --thread=<id>[,<id>...] [--dry-run]\n" +
        "publish takes an explicit list of thread ids decided by the operator.",
    );
    process.exit(1);
  }

  const items = readJsonl<TriageItem>(TRIAGE_ITEMS_PATH);
  if (items.length === 0) {
    console.error(`No triage items at ${TRIAGE_ITEMS_PATH}. Run 'triage' first.`);
    process.exit(1);
  }
  if (Bun.env.DISCORD_BOT_TOKEN === undefined) {
    console.error("Error: DISCORD_BOT_TOKEN must be set in .env");
    process.exit(1);
  }

  const state = await loadSyncState();
  const published = { ...(state.published_threads ?? {}) };

  // Group triage items by thread so we can pick a representative for the issue body.
  const itemsByThread = new Map<string, TriageItem[]>();
  for (const item of items) {
    if (!itemsByThread.has(item.thread_id)) itemsByThread.set(item.thread_id, []);
    itemsByThread.get(item.thread_id)!.push(item);
  }

  let created = 0;
  let repairedDiscordWriteBacks = 0;
  let skippedAlreadyPublished = 0;
  let skippedNoItems = 0;
  let failed = 0;

  for (const threadId of targets) {
    const existing = published[threadId];
    if (existing !== undefined) {
      if (existing.issue_number > 0 && existing.issue_url !== "" && !hasDiscordWriteBack(existing)) {
        console.log(`  [repair] ${threadId} — issue #${existing.issue_number} exists, Discord write-back is incomplete`);
        try {
          const replyId = await writeDiscordTracking(threadId, existing.issue_url, dryRun);
          if (!dryRun) {
            published[threadId] = {
              ...existing,
              reacted_message_id: threadId,
              reply_message_id: replyId,
            };
            state.published_threads = published;
            await saveSyncState(state);
          }
          repairedDiscordWriteBacks++;
        } catch (err) {
          console.error(`    failed (discord repair): ${(err as Error).message}`);
          failed++;
        }
        continue;
      }

      console.log(`  [skip] ${threadId} — already in published_threads (#${existing.issue_number})`);
      skippedAlreadyPublished++;
      continue;
    }

    const threadItems = itemsByThread.get(threadId) ?? [];
    const item = pickPublishItem(threadItems);
    if (item === null) {
      console.error(`  [skip] ${threadId} — no triage items for this thread`);
      skippedNoItems++;
      continue;
    }

    console.log(`  [publish] ${item.thread_name}`);

    // Phase 1: GH issue first. If this throws we recorded no state — safe to retry.
    let issue: { number: number; url: string };
    try {
      issue = resolveIssue(item, dryRun);
    } catch (err) {
      console.error(`    failed (issue resolve): ${(err as Error).message}`);
      failed++;
      continue;
    }

    // Persist the GH side BEFORE attempting Discord — a Discord failure must
    // never cause a duplicate GH issue on the next run. Discord success
    // rewrites the record below with the actual reply message id.
    if (!dryRun) {
      published[threadId] = {
        issue_number: issue.number,
        issue_url: issue.url,
        reacted_message_id: "",
        reply_message_id: "",
        published_at: new Date().toISOString(),
        mode: "created",
      };
      state.published_threads = published;
      await saveSyncState(state);
    }

    // Phase 2: Discord write-back. Failures (including 50083 archived-thread)
    // leave the GH record intact and surface the error to the operator.
    try {
      const replyId = await writeDiscordTracking(item.thread_id, issue.url, dryRun);
      if (!dryRun) {
        published[threadId] = {
          ...published[threadId],
          reacted_message_id: item.thread_id,
          reply_message_id: replyId,
        };
        state.published_threads = published;
        await saveSyncState(state);
      }
      created++;
    } catch (err) {
      console.error(`    failed (discord): ${(err as Error).message}`);
      console.error(`    (GH issue #${issue.number} retained; write-back missed — operator must follow up)`);
      failed++;
    }
  }

  console.log(`Publish complete${dryRun ? " (dry-run)" : ""}.`);
  console.log(`  Targets:                    ${targets.size}`);
  console.log(`  Created:                    ${created}`);
  console.log(`  Repaired Discord write-back: ${repairedDiscordWriteBacks}`);
  console.log(`  Skipped (already published): ${skippedAlreadyPublished}`);
  console.log(`  Skipped (no triage items):   ${skippedNoItems}`);
  console.log(`  Failed:                     ${failed}`);
  if (!dryRun) {
    console.log(`  Sync state:                 ${SYNC_STATE_PATH}`);
  }
}

async function cmdRender(): Promise<void> {
  const reports = readJsonl<ReportItem>(REPORT_ITEMS_PATH);
  if (reports.length === 0) {
    console.error(`No report items found at ${REPORT_ITEMS_PATH}. Run 'extract' first.`);
    process.exit(1);
  }

  await mkdir("triage", { recursive: true });

  const triageItems = readJsonl<TriageItem>(TRIAGE_ITEMS_PATH);
  if (triageItems.length > 0) {
    const triageMarkdown = renderTriageDashboard(triageItems);
    const triageDashboardPath = "triage/triage-dashboard.md";
    await Bun.write(triageDashboardPath, triageMarkdown);
    console.log(`Triage dashboard written to ${triageDashboardPath}`);
  }

  const markdown = renderDashboard(reports);
  await Bun.write(DASHBOARD_PATH, markdown);
  console.log(`Dashboard written to ${DASHBOARD_PATH}`);
}

const LLM_TRIAGE_PATH = "triage/llm-triage-items.jsonl";
const CROSSREF_PATH = "triage/coverage-crossref.jsonl";
const CROSSREF_SUMMARY_PATH = "triage/coverage-crossref-summary.md";

async function cmdCrossref(): Promise<void> {
  if (!existsSync(LLM_TRIAGE_PATH)) {
    console.error(`No LLM triage items at ${LLM_TRIAGE_PATH}. Run LLM triage first.`);
    process.exit(1);
  }

  console.log("Cross-referencing LLM triage against card-data.json parser coverage...");
  const items = await crossReference(LLM_TRIAGE_PATH, "client/public/card-data.json");

  await writeJsonl(CROSSREF_PATH, items);

  const counts = { needs_semantic_verify: 0, still_broken: 0, unknown_card: 0, no_card: 0 };
  for (const item of items) counts[item.overall_status]++;

  console.log(`Cross-reference complete.`);
  console.log(`  Total items: ${items.length}`);
  console.log(`  needs_semantic_verify: ${counts.needs_semantic_verify}`);
  console.log(`  still_broken: ${counts.still_broken}`);
  console.log(`  unknown_card: ${counts.unknown_card}`);
  console.log(`  no_card: ${counts.no_card}`);
  console.log(`  Written to ${CROSSREF_PATH}`);

  const summaryLines: string[] = [];
  summaryLines.push(`# Coverage Cross-Reference Summary`);
  summaryLines.push(`_Generated: ${new Date().toISOString()}_\n`);
  summaryLines.push(`| Status | Count |`);
  summaryLines.push(`|--------|-------|`);
  for (const [status, count] of Object.entries(counts)) {
    summaryLines.push(`| ${status} | ${count} |`);
  }
  summaryLines.push(``);

  const broken = items.filter((i) => i.overall_status === "still_broken");
  summaryLines.push(`## Still Broken (${broken.length} items)\n`);
  if (broken.length > 0) {
    const byPriority = new Map<string, CrossrefItem[]>();
    for (const item of broken) {
      if (!byPriority.has(item.priority)) byPriority.set(item.priority, []);
      byPriority.get(item.priority)!.push(item);
    }
    for (const [prio, pItems] of [...byPriority.entries()].sort()) {
      summaryLines.push(`### ${(prio ?? "unknown").toUpperCase()}\n`);
      summaryLines.push(`| Thread | Cards | Summary |`);
      summaryLines.push(`|--------|-------|---------|`);
      for (const item of pItems) {
        summaryLines.push(`| ${item.thread_name} | ${item.cards.join(", ")} | ${item.summary.slice(0, 80)} |`);
      }
      summaryLines.push(``);
    }
  }

  const candidates = items.filter((i) => i.overall_status === "needs_semantic_verify");
  summaryLines.push(`## Needs Semantic Verification (${candidates.length} items)\n`);
  const candidatesByPrio = new Map<string, number>();
  for (const item of candidates) {
    candidatesByPrio.set(item.priority, (candidatesByPrio.get(item.priority) ?? 0) + 1);
  }
  for (const [prio, count] of [...candidatesByPrio.entries()].sort()) {
    summaryLines.push(`- **${prio}**: ${count}`);
  }
  summaryLines.push(``);

  await Bun.write(CROSSREF_SUMMARY_PATH, summaryLines.join("\n"));
  console.log(`  Summary written to ${CROSSREF_SUMMARY_PATH}`);
}

async function cmdVerify(): Promise<void> {
  const crossref = readJsonl<CrossrefItem>(CROSSREF_PATH);
  if (crossref.length === 0) {
    console.error(`No crossref data at ${CROSSREF_PATH}. Run 'crossref' first.`);
    process.exit(1);
  }

  console.log("Checking open GitHub issues against current coverage...");

  const ghResult = Bun.spawnSync(
    ["gh", "issue", "list", "--repo", "phase-rs/phase", "--state", "open", "--limit", "200", "--json", "number,title,labels"],
  );
  if (ghResult.exitCode !== 0) {
    console.error("Failed to fetch GitHub issues. Is `gh` authenticated?");
    process.exit(1);
  }

  const ghIssues = JSON.parse(ghResult.stdout.toString()) as Array<{
    number: number;
    title: string;
    labels: Array<{ name: string }>;
  }>;

  const parserIssues = ghIssues.filter((i) =>
    i.labels.some((l) => l.name === "area:parser") &&
    i.labels.some((l) => l.name === "status:confirmed"),
  );

  const cardData = (await Bun.file("client/public/card-data.json").json()) as Record<string, unknown>;

  console.log(`\n  Open parser issues: ${parserIssues.length}`);
  console.log(`  Checking each against current card-data.json...\n`);

  let fixedCount = 0;
  for (const issue of parserIssues) {
    const matchingCrossref = crossref.filter((c) =>
      c.summary.length > 10 &&
      issue.title.toLowerCase().includes(c.cards[0]?.toLowerCase() ?? "___nomatch___"),
    );

    if (
      matchingCrossref.length > 0 &&
      matchingCrossref.every((c) => c.overall_status === "needs_semantic_verify")
    ) {
      console.log(`  ? #${issue.number} ${issue.title.slice(0, 60)} → needs semantic verification`);
      fixedCount++;
    }
  }

  if (fixedCount === 0) {
    console.log("  No parser issues became semantic-verification candidates.");
  } else {
    console.log(`\n  ${fixedCount} issue(s) have fully-parsed cards. Inspect semantics before any status change:`);
    console.log(`    gh issue edit <N> --repo phase-rs/phase --remove-label "status:confirmed" --add-label "status:needs-runtime-verify"`);
    console.log(`  After runtime verification, close with:`);
    console.log(`    gh issue close <N> --repo phase-rs/phase --comment "Verified fixed in gameplay."`);
  }
}

function printHelp(): void {
  console.log(`Usage: bun scripts/sync-bug-reports.ts <command>

Commands:
  fetch     Fetch Discord messages → triage/raw/discord-messages.jsonl
            Defaults to threads with activity after the previous successful
            fetch. Flags: --since=<ISO timestamp>, --full
  extract   Extract report items from messages → triage/report-items.jsonl
  triage    Classify report items → triage/triage-items.jsonl
            Also emits triage/triage-delta.jsonl: ONLY the reports from the
            latest fetch window (messages with fetched_at > prev_fetch_at).
            Review the delta — never the full archive — each cycle.
  delta     Re-emit triage/triage-delta.jsonl from existing triage-items.jsonl
            without re-classifying. Lists every non-skip item the operator must
            resolve this cycle (file / dup-link / mark-handled).
  pending   List unpublished threads (local-state diff of cursors vs
            published_threads), newest-activity-first. NO judgment applied —
            the operator (an LLM in chat) reads candidates and decides.
            Flags: --limit=N (default 50), --since=<thread_id>
  read      Dump the full message stream of a thread so the operator can read
            and judge it inline.
            Flags: --thread=<thread_id>
  mark-handled  Mark specific threads as already-handled without filing a GH
            issue (e.g. "this is a dup of #N", "already resolved"). One thread
            at a time — the bulk --until-thread watermark form was removed
            because it once mass-suppressed unresolved bugs.
            Flags: --thread=<id>[,<id>], --notes='...', --dry-run
  publish   Create a GH issue for each --thread=<id> the operator listed, then
            include Discord ids in the issue body, react 👀 + post tracking
            link in the Discord thread. If a previous run created the issue but
            missed Discord write-back, rerunning repairs the missing reply.
            Mechanics only — the operator has already decided these threads are
            worth filing.
            Flags:
              --dry-run                 preview without side effects
              --thread=<id>[,<id>...]   thread ids to publish (required)
  crossref  Cross-reference LLM triage against parser coverage → triage/coverage-crossref.jsonl
  verify    Check open GitHub issues against current coverage for newly-fixed bugs
  render    Generate dashboard markdown → triage/dashboard.md (+ triage-dashboard.md if triaged)
  --help    Show this help message
`);
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

const [, , command] = process.argv;

switch (command) {
  case "fetch":
    await cmdFetch();
    break;
  case "extract":
    await cmdExtract();
    break;
  case "triage":
    await cmdTriage();
    break;
  case "delta":
    await cmdDelta();
    break;
  case "publish":
    await cmdPublish();
    break;
  case "pending":
    await cmdPending();
    break;
  case "mark-handled":
    await cmdMarkHandled();
    break;
  case "read":
    await cmdReadThread();
    break;
  case "crossref":
    await cmdCrossref();
    break;
  case "verify":
    await cmdVerify();
    break;
  case "render":
    await cmdRender();
    break;
  case "--help":
  case "-h":
    printHelp();
    break;
  default:
    if (command) {
      console.error(`Unknown command: ${command}`);
    }
    printHelp();
    process.exit(command ? 1 : 0);
}
