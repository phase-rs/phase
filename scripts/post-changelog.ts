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
import { createMessage, crosspostMessage, discordGet } from "./lib/discord.ts";

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
// emoji is env-configurable (same no-hardcoded-ids rule as DISCORD_GUILD_ID),
// falling back to plain `🎴` so a token-less or other-guild run still reads
// sensibly. A custom guild emoji is stored bracket-free as `name:id` — bots
// must send it to the API as `<:name:id>` (a bare `:name:` only resolves in the
// Discord client composer, not over REST), but the `<`/`>` would break
// `set -a; source .env` (shell redirects), so we keep them out of .env and wrap
// here. A literal Unicode emoji (no colon) is used verbatim.
const rawEmoji = Bun.env.CHANGELOG_HEADER_EMOJI?.trim();
const HEADER_EMOJI = rawEmoji ? (rawEmoji.includes(":") ? `<:${rawEmoji}>` : rawEmoji) : "🎴";
const HEADER = `${HEADER_EMOJI} What's New in phase.rs`;
// Discord rejects messages longer than 2000 characters.
const DISCORD_MAX = 2000;

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const channelId =
  args.find((a) => !a.startsWith("-")) ?? Bun.env.ANNOUNCEMENTS_CHANNEL_ID;

/**
 * Pack the post into ≤limit-char chunks, preferring to break at blank-line
 * (section) boundaries so a "✨ Section" header is never stranded at the end of
 * a message away from its bullets. Sections are packed whole; a single section
 * larger than the limit falls back to line-packing (still never tearing a line)
 * so it always fits.
 */
function chunk(content: string, limit = DISCORD_MAX): string[] {
  const chunks: string[] = [];
  let current = "";

  // Line-pack a single over-limit section, continuing from `current`.
  const linePack = (block: string) => {
    for (const line of block.split("\n")) {
      if (current && current.length + 1 + line.length > limit) {
        chunks.push(current);
        current = line;
      } else {
        current = current ? `${current}\n${line}` : line;
      }
    }
  };

  for (const block of content.split("\n\n")) {
    const sep = current ? 2 : 0; // the "\n\n" rejoining this block to current
    if (current && current.length + sep + block.length > limit) {
      chunks.push(current);
      current = "";
    }
    if (block.length > limit) {
      linePack(block);
    } else {
      current = current ? `${current}\n\n${block}` : block;
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

const postedIds: string[] = [];
for (const content of messages) {
  const posted = await createMessage(channelId, content);
  postedIds.push(posted.id);
}

// Record the watermark so a re-run is a no-op.
state.lastPostedId = entry.id;
writeFileSync(STATE_PATH, `${JSON.stringify(state, null, 2)}\n`);

// Link the in-app entry back to the announcement (only possible with a guild id).
if (Bun.env.DISCORD_GUILD_ID && postedIds[0] && !entry.discordUrl) {
  entry.discordUrl = `https://discord.com/channels/${Bun.env.DISCORD_GUILD_ID}/${channelId}/${postedIds[0]}`;
  writeFileSync(CHANGELOG_PATH, `${JSON.stringify({ entries }, null, 2)}\n`);
}

// Auto-publish: an Announcement/News channel (type 5) can crosspost each message
// to follower servers — the "Share with your followers?" the Discord client
// prompts for, done automatically. Other channel types have nothing to publish.
const channel = await discordGet<{ type: number }>(`/channels/${channelId}`);
let published = 0;
if (channel.type === 5) {
  for (const id of postedIds) {
    await crosspostMessage(channelId, id);
    published += 1;
  }
}

console.log(
  `Posted entry #${entry.id} "${entry.title}" to #announcements as ${messages.length} message(s)` +
    `${published ? `, published ${published} to followers` : ""}` +
    `${entry.discordUrl ? ` (linked ${entry.discordUrl})` : ""}.`,
);
