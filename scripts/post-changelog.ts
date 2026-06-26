#!/usr/bin/env bun
/**
 * Posts the newest changelog entry to the Discord #announcements channel.
 *
 * This is the publish counterpart to fetch-changelog.ts (which reads
 * #announcements into draft entries). It is the LAST step of the `changelog`
 * skill: after an entry is prepended to client/public/changelog.json and the
 * meta is regenerated, this mirrors that same entry out to Discord so the
 * #announcements post and the in-app "What's New" modal stay one source of
 * truth — the post is reconstructed from changelog.json, never re-authored, so
 * the two can't drift.
 *
 * Safe to run unconditionally: if DISCORD_BOT_TOKEN (or the channel id) is
 * absent it no-ops with exit 0, so wiring it into the skill flow never breaks a
 * token-less environment. Idempotent: it records the posted id in
 * scripts/changelog/state.json (`lastPostedId`) and skips if the newest entry
 * has already been posted, so re-running the skill can't double-post.
 *
 * Config (no hardcoded secrets or ids — same contract as fetch-changelog.ts):
 *   DISCORD_BOT_TOKEN        — bot token (read by scripts/lib/discord.ts); gate
 *   ANNOUNCEMENTS_CHANNEL_ID — channel to post to (or pass as the first CLI arg)
 *   DISCORD_GUILD_ID         — optional; when set, the posted entry gets a
 *                              discordUrl written back into changelog.json so
 *                              the in-app modal links to the announcement
 *
 * Usage:
 *   ANNOUNCEMENTS_CHANNEL_ID=... bun scripts/post-changelog.ts [channelId]
 *   bun scripts/post-changelog.ts --dry-run    # print what would be posted
 */
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { createMessage } from "./lib/discord.ts";

interface ChangelogEntry {
  id: number;
  date: string;
  title: string;
  tags: string[];
  body: string;
  discordUrl?: string;
}
interface Changelog {
  entries: ChangelogEntry[];
}
interface ChangelogState {
  lastTip?: string;
  lastPostedId?: number;
}

const ROOT = path.resolve(import.meta.dir, "..");
const CHANGELOG_PATH = path.join(ROOT, "client/public/changelog.json");
const STATE_PATH = path.join(ROOT, "scripts/changelog/state.json");

// The header line the in-app body omits but the Discord post leads with — the
// inverse of cleanBody()'s HEADER_RE filter in fetch-changelog.ts. The leading
// emoji is env-configurable because a custom Discord emoji is a guild-scoped id
// (`<:phase:1234…>`) — same no-hardcoded-ids rule as DISCORD_GUILD_ID — and
// falls back to a plain emoji so a token-less or other-guild run still reads
// sensibly. Bots must send custom emoji as `<:name:id>`; a bare `:name:`
// shortcode posts as literal text.
const HEADER_EMOJI = Bun.env.CHANGELOG_HEADER_EMOJI ?? "🎴";
const HEADER = `${HEADER_EMOJI} What's New in phase.rs`;
// Discord rejects messages longer than 2000 characters.
const DISCORD_MAX = 2000;

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const channelId =
  args.find((a) => !a.startsWith("-")) ?? Bun.env.ANNOUNCEMENTS_CHANNEL_ID;

/**
 * Pack lines into ≤limit-char chunks without ever splitting a line, so a bullet
 * or section header is never torn across two Discord messages. Changelog lines
 * are short (bullets, section headers), so per-line < limit always holds.
 */
function chunk(content: string, limit = DISCORD_MAX): string[] {
  const chunks: string[] = [];
  let current = "";
  for (const line of content.split("\n")) {
    if (current && current.length + 1 + line.length > limit) {
      chunks.push(current);
      current = line;
    } else {
      current = current ? `${current}\n${line}` : line;
    }
  }
  if (current) chunks.push(current);
  return chunks;
}

const { entries } = JSON.parse(readFileSync(CHANGELOG_PATH, "utf-8")) as Changelog;
if (entries.length === 0) {
  console.error("changelog.json has no entries — nothing to post.");
  process.exit(1);
}
const entry = entries[0]; // newest-first invariant (asserted by gen-changelog-meta.ts)

const state = JSON.parse(readFileSync(STATE_PATH, "utf-8")) as ChangelogState;
if ((state.lastPostedId ?? 0) >= entry.id) {
  console.log(`Entry #${entry.id} already posted (lastPostedId=${state.lastPostedId}). Nothing to do.`);
  process.exit(0);
}

const post = `${HEADER}\n\n${entry.body}`;
const messages = chunk(post);

if (dryRun) {
  console.log(`[dry-run] would post entry #${entry.id} "${entry.title}" as ${messages.length} message(s):\n`);
  messages.forEach((m, i) => console.log(`--- message ${i + 1}/${messages.length} (${m.length} chars) ---\n${m}\n`));
  process.exit(0);
}

// Token-gated: a token-less environment is a clean no-op, so the skill can call
// this unconditionally without failing when DISCORD_BOT_TOKEN isn't set.
if (!Bun.env.DISCORD_BOT_TOKEN || !channelId) {
  console.log(
    "Skipping Discord post: " +
      `${!Bun.env.DISCORD_BOT_TOKEN ? "DISCORD_BOT_TOKEN" : "ANNOUNCEMENTS_CHANNEL_ID"} not set.`,
  );
  process.exit(0);
}

let firstMessageId: string | undefined;
for (const content of messages) {
  const posted = await createMessage(channelId, content);
  firstMessageId ??= posted.id;
}

// Record the watermark so a re-run is a no-op.
state.lastPostedId = entry.id;
writeFileSync(STATE_PATH, `${JSON.stringify(state, null, 2)}\n`);

// Link the in-app entry back to the announcement (only possible with a guild id).
if (Bun.env.DISCORD_GUILD_ID && firstMessageId && !entry.discordUrl) {
  entry.discordUrl = `https://discord.com/channels/${Bun.env.DISCORD_GUILD_ID}/${channelId}/${firstMessageId}`;
  writeFileSync(CHANGELOG_PATH, `${JSON.stringify({ entries }, null, 2)}\n`);
}

console.log(
  `Posted entry #${entry.id} "${entry.title}" to #announcements as ${messages.length} message(s)` +
    `${entry.discordUrl ? ` (linked ${entry.discordUrl})` : ""}.`,
);
