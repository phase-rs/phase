#!/usr/bin/env bun

import { existsSync, readFileSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import type { RawDiscordMessage, ReportItem, SyncState, TriageItem } from "./lib/types.ts";
import {
  discordGet,
  fetchActiveThreads,
  fetchArchivedThreads,
  fetchMessages,
  type DiscordThread,
  type DiscordMessage,
} from "./lib/discord.ts";
import { extractReports } from "./lib/extract.ts";
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
const DASHBOARD_PATH = "triage/dashboard.md";
const LEGACY_EXPORT_PATH = "tmp/discord-thread-messages.json";

function defaultSyncState(): SyncState {
  return {
    last_fetch_at: new Date(0).toISOString(),
    last_thread_cursors: {},
    imported_from_legacy: false,
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
): Promise<{ newMessages: number; cursors: Record<string, string> }> {
  let active: DiscordThread[] = [];
  try {
    active = await fetchActiveThreads(guildId, channelId);
  } catch (err) {
    console.error(`Warning: could not fetch active threads: ${(err as Error).message}`);
  }

  let publicArchived: DiscordThread[] = [];
  try {
    publicArchived = await fetchArchivedThreads(channelId, "public");
  } catch (err) {
    console.error(`Warning: could not fetch public archived threads: ${(err as Error).message}`);
  }

  let privateArchived: DiscordThread[] = [];
  try {
    privateArchived = await fetchArchivedThreads(channelId, "private");
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

  for (const [i, thread] of [...threadsById.values()].entries()) {
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
      `\rFetching thread ${i + 1}/${threadsById.size}: ${thread.name.slice(0, 40).padEnd(40)}`,
    );
  }

  if (threadsById.size > 0) process.stdout.write("\n");

  if (newMessages.length > 0) {
    await appendJsonl(MESSAGES_PATH, newMessages);
  }

  return { newMessages: newMessages.length, cursors: updatedCursors };
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
  console.log("Fetching new messages from Discord API...");
  const { newMessages, cursors } = await fetchIncremental(
    guildId,
    channelId,
    state.last_thread_cursors,
  );

  state.last_thread_cursors = cursors;
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
  let items = await triageReports(reports);
  items = applyGithubDedupe(items, await loadGithubIssueIndex());

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
}

interface GithubIssueIndexItem {
  number: number;
  title: string;
  state: "OPEN" | "CLOSED";
  body: string;
  url: string;
  closedAt: string | null;
}

async function loadGithubIssueIndex(): Promise<GithubIssueIndexItem[]> {
  const ghResult = Bun.spawnSync([
    "gh",
    "issue",
    "list",
    "--repo",
    "phase-rs/phase",
    "--state",
    "all",
    "--limit",
    "1000",
    "--json",
    "number,title,state,body,url,closedAt",
  ]);

  if (ghResult.exitCode !== 0) {
    console.error("Warning: failed to fetch GitHub issue index for dedupe.");
    return [];
  }

  const issues = JSON.parse(ghResult.stdout.toString()) as GithubIssueIndexItem[];
  if (issues.length === 1000) {
    console.error("Warning: GitHub issue dedupe index hit the 1000 issue limit.");
  }
  return issues;
}

function applyGithubDedupe(
  items: TriageItem[],
  issues: GithubIssueIndexItem[],
): TriageItem[] {
  if (issues.length === 0) return items;

  const searchableIssues = issues.map((issue) => ({
    issue,
    text: `${issue.title}\n${issue.body}\n${issue.url}`,
  }));

  let matchedOpen = 0;
  let matchedClosed = 0;

  const deduped = items.map((item) => {
    if (item.proposed_action !== "create_issue" && item.proposed_action !== "needs_human_review") {
      return item;
    }

    const markers: Array<{
      kind: NonNullable<TriageItem["github_issue"]>["match_kind"];
      value: string;
    }> = [
      { kind: "report_id", value: item.report_id },
      { kind: "source_url", value: item.source_url },
      { kind: "discord_message", value: `${item.thread_id}/${item.message_id}` },
    ];

    const matches = searchableIssues
      .flatMap(({ issue, text }) => {
        const marker = markers.find(
          (candidate) => candidate.value !== "" && text.includes(candidate.value),
        );
        return marker === undefined ? [] : [{ issue, marker }];
      })
      .sort((a, b) => {
        if (a.issue.state !== b.issue.state) return a.issue.state === "OPEN" ? -1 : 1;
        return a.issue.number - b.issue.number;
      });
    const match = matches[0];

    if (match === undefined) return item;

    const github_issue: NonNullable<TriageItem["github_issue"]> = {
      number: match.issue.number,
      title: match.issue.title,
      state: match.issue.state,
      url: match.issue.url,
      closed_at: match.issue.closedAt,
      match_kind: match.marker.kind,
    };

    if (match.issue.state === "OPEN") {
      matchedOpen++;
      return {
        ...item,
        proposed_action: "append_to_existing" as const,
        github_issue,
      };
    }

    matchedClosed++;
    return {
      ...item,
      proposed_action: "skip_existing_closed" as const,
      github_issue,
    };
  });

  console.log(`  GitHub dedupe: matched open issues: ${matchedOpen}`);
  console.log(`  GitHub dedupe: matched closed issues: ${matchedClosed}`);

  return deduped;
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
  extract   Extract report items from messages → triage/report-items.jsonl
  triage    Classify report items → triage/triage-items.jsonl
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
