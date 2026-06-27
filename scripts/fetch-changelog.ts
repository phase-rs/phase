#!/usr/bin/env bun
/**
 * One-time backfill: turn the existing #announcements posts into draft changelog
 * entries for client/public/changelog.json.
 *
 * This is a STAGING tool, not a publish tool. It writes drafts to a file you
 * review and hand-merge — it never touches client/public/ directly, so an
 * unreviewed (or mis-parsed) draft can never ship. After reviewing/editing the
 * drafts, paste them into client/public/changelog.json (newest-first, ids
 * append-only) and run `bun scripts/gen-changelog-meta.ts`.
 *
 * Config (no hardcoded secrets or ids):
 *   DISCORD_BOT_TOKEN        — bot token (read by scripts/lib/discord.ts)
 *   ANNOUNCEMENTS_CHANNEL_ID — channel to read (or pass as the first CLI arg)
 *   DISCORD_GUILD_ID         — optional; when set, each draft gets a discordUrl
 *
 * Usage:
 *   ANNOUNCEMENTS_CHANNEL_ID=... bun scripts/fetch-changelog.ts [channelId] [outPath]
 */
import { writeFileSync } from "node:fs";
import path from "node:path";
import { fetchMessages, type DiscordMessage } from "./lib/discord.ts";

type ChangelogTag =
  | "new-cards"
  | "card-fixes"
  | "gameplay"
  | "interface"
  | "localization"
  | "ai"
  | "multiplayer";

interface ChangelogEntry {
  id: number;
  date: string;
  title: string;
  tags: ChangelogTag[];
  body: string;
  discordUrl?: string;
}

// Emoji section → tag. Matched on the base codepoint so an optional variation
// selector (U+FE0F) in the post doesn't defeat the lookup. Mirrors the section
// set the `changelog` skill emits.
const SECTION_TAGS: Array<[string, ChangelogTag]> = [
  ["✨", "new-cards"],
  ["🛠", "card-fixes"],
  ["⚔", "gameplay"],
  ["🖥", "interface"],
  ["🌍", "localization"],
  ["🤖", "ai"],
  ["🌐", "multiplayer"],
];

// Keyword fallback for historical posts that used sections outside the canonical
// set (🛡️ Stability, ⚡ Performance, daily "- " dumps, etc.). Best-effort — an
// entry that matches nothing simply gets no chips.
const KEYWORD_TAGS: Array<[RegExp, ChangelogTag]> = [
  [/\b(new (set|card)|sets? (are |is )|import pass|now playable|are here)\b/i, "new-cards"],
  [/\b(card|parse|parser|oracle|wording|now work|work right|fixed?)\b/i, "card-fixes"],
  [/\b(combat|attack|block|stack|priority|mana|turn|softlock|panic|crash|freeze|stability)\b/i, "gameplay"],
  [/\b(ui|interface|display|dialog|banner|render|layout|performance|speedup|faster|animation)\b/i, "interface"],
  [/\b(multiplayer|online|host|lobby|room|spectat|sandbox)\b/i, "multiplayer"],
  [/\b(localization|locale|language|translat)\b/i, "localization"],
  [/\b(\bAI\b|opponent|policy)\b/i, "ai"],
];

const TAG_LABEL: Record<ChangelogTag, string> = {
  "new-cards": "New cards",
  "card-fixes": "Card fixes",
  gameplay: "Gameplay",
  interface: "Interface",
  localization: "Localization",
  ai: "AI",
  multiplayer: "Multiplayer",
};

const channelId = process.argv[2] ?? Bun.env.ANNOUNCEMENTS_CHANNEL_ID;
const outPath =
  process.argv[3] ??
  path.resolve(import.meta.dir, "changelog/backfill-drafts.json");

if (!channelId) {
  console.error(
    "Missing channel id. Set ANNOUNCEMENTS_CHANNEL_ID or pass it as the first arg.",
  );
  process.exit(1);
}

/** Drop Discord custom emoji (`<:name:id>` / `<a:name:id>`). */
function stripCustomEmoji(s: string): string {
  return s.replace(/<a?:\w+:\d+>/g, "");
}

const HEADER_RE = /what'?s new in phase\.rs/i;
const CONTINUED_RE = /^\s*continued\b/i;

/** Normalize a post body to the plain-text the in-app modal renders. */
function cleanBody(raw: string): string {
  return stripCustomEmoji(raw)
    .split("\n")
    .filter((line) => !HEADER_RE.test(line)) // drop the "What's New…" header line
    .map((line) =>
      line
        .replace(/\*\*(.+?)\*\*/g, "$1") // **bold** → text
        .replace(/__(.+?)__/g, "$1") // __bold__ → text
        .replace(/^(\s*)[-*]\s+/, "$1• "), // - / * bullets → •
    )
    .join("\n")
    .replace(/\n{3,}/g, "\n\n") // collapse runs of blank lines
    .trim();
}

function inferTags(body: string): ChangelogTag[] {
  const bySection = SECTION_TAGS.filter(([emoji]) => body.includes(emoji)).map(
    ([, tag]) => tag,
  );
  if (bySection.length > 0) return bySection;
  // Keyword fallback (deduped, order-stable).
  const seen = new Set<ChangelogTag>();
  for (const [re, tag] of KEYWORD_TAGS) {
    if (!seen.has(tag) && re.test(body)) seen.add(tag);
  }
  return [...seen];
}

/** Title from the section themes — consistent and readable next to the date. */
function deriveTitle(tags: ChangelogTag[], body: string): string {
  if (tags.length > 0) {
    const labels = tags.map((t) => TAG_LABEL[t]);
    const joined =
      labels.length === 1
        ? labels[0]
        : `${labels.slice(0, -1).join(", ")} & ${labels.at(-1)}`;
    return joined;
  }
  // No tags: first meaningful line, stripped of leading emoji/punctuation.
  for (const line of body.split("\n")) {
    const t = line.replace(/^[\s\p{Emoji_Presentation}\p{Extended_Pictographic}•\-*]+/u, "").trim();
    if (t) return t.length > 70 ? `${t.slice(0, 67)}…` : t;
  }
  return "Updates";
}

const messages = await fetchMessages(channelId);

const drafts: ChangelogEntry[] = [];
let id = 0;
for (const m of messages as DiscordMessage[]) {
  const rawSource = m.content?.trim() || m.embeds?.[0]?.description?.trim() || "";
  if (!rawSource) continue;

  const body = cleanBody(rawSource);
  if (!body) continue;

  // A "continued…" message is the tail of the previous post (Discord's 2000-char
  // limit splits long changelogs). Fold it into the prior entry instead of
  // creating a standalone one.
  if (CONTINUED_RE.test(stripCustomEmoji(rawSource).trimStart()) && drafts.length > 0) {
    const prev = drafts[drafts.length - 1];
    const tail = body.replace(CONTINUED_RE, "").replace(/^[\s.…]+/, "").trim();
    prev.body = `${prev.body}\n\n${tail}`.trim();
    prev.tags = [...new Set([...prev.tags, ...inferTags(prev.body)])];
    prev.title = deriveTitle(prev.tags, prev.body);
    continue;
  }

  const tags = inferTags(body);
  id += 1;
  const entry: ChangelogEntry = {
    id,
    date: m.timestamp.slice(0, 10),
    title: deriveTitle(tags, body),
    tags,
    body,
  };
  if (Bun.env.DISCORD_GUILD_ID) {
    entry.discordUrl = `https://discord.com/channels/${Bun.env.DISCORD_GUILD_ID}/${channelId}/${m.id}`;
  }
  drafts.push(entry);
}

// Newest-first, matching changelog.json's canonical order.
drafts.reverse();

writeFileSync(outPath, `${JSON.stringify({ entries: drafts }, null, 2)}\n`);
console.log(
  `Wrote ${drafts.length} draft entries to ${outPath}.\n` +
    `Review/edit, then merge into client/public/changelog.json and run ` +
    `\`bun scripts/gen-changelog-meta.ts\`.`,
);
