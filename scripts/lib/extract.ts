import type { RawDiscordMessage, ReportItem } from "./types.ts";
import { extractCardReferences, loadCardNameIndex } from "./cardNames.ts";
import type { CardNameIndex } from "./cardNames.ts";

const MECHANIC_KEYWORDS = [
  "mana",
  "trigger",
  "triggered",
  "token",
  "tokens",
  "counter",
  "counters",
  "stack",
  "combat",
  "attack",
  "block",
  "draw",
  "discard",
  "sacrifice",
  "destroy",
  "exile",
  "graveyard",
  "library",
  "hand",
  "life",
  "damage",
  "tap",
  "untap",
  "flash",
  "flying",
  "trample",
  "lifelink",
  "deathtouch",
  "haste",
  "vigilance",
  "first strike",
  "double strike",
  "hexproof",
  "indestructible",
  "protection",
  "reach",
  "regenerate",
  "shroud",
  "equip",
  "equipment",
  "enchant",
  "aura",
  "planeswalker",
  "loyalty",
  "phase",
  "priority",
  "targeting",
  "copy",
  "respond",
  "activate",
  "ability",
  "instant",
  "sorcery",
  "cast",
  "spell",
  "land",
  "creature",
  "artifact",
  "enchantment",
  "saga",
  "suspend",
  "escape",
  "flashback",
  "delve",
  "convoke",
  "kicker",
  "morph",
  "transform",
  "modal",
  "choose",
  "scry",
  "surveil",
  "proliferate",
];

function detectMechanics(text: string): string[] {
  const lower = text.toLowerCase();
  const found = new Set<string>();
  for (const kw of MECHANIC_KEYWORDS) {
    if (lower.includes(kw)) found.add(kw);
  }
  return [...found];
}

interface DetectedCards {
  /** All detected card keys: the substring scan plus explicit references. */
  cards: string[];
  /** Keys from `[[...]]` / Scryfall URLs — trusted, no single-word false positives. */
  explicitCards: string[];
}

function detectCards(text: string, index: CardNameIndex): DetectedCards {
  const lower = text.toLowerCase();
  const found = new Set<string>();
  for (const name of index.rawKeys) {
    if (lower.includes(name)) found.add(name);
  }
  // Explicit references ([[Card]], Scryfall links) resolve names the substring
  // scan can't — truncated / punctuation-heavy / brand-new — and are trusted at
  // publish time. They always join `cards` so parser-status and dedup see them.
  const explicitCards = extractCardReferences(text, index);
  for (const key of explicitCards) found.add(key);
  return { cards: [...found], explicitCards };
}

function isClarificationOrFollowUp(content: string): boolean {
  const lower = content.toLowerCase().trim();
  return (
    lower.startsWith("scratch that") ||
    lower.startsWith("never mind") ||
    lower.startsWith("nevermind") ||
    lower.startsWith("actually") ||
    lower.startsWith("correction:") ||
    lower.startsWith("update:") ||
    lower.length < 20
  );
}

function scoreConfidence(
  content: string,
  cards: string[],
  hasBugDescription: boolean,
  isEvidenceOnly: boolean,
): number {
  if (isEvidenceOnly) return 0.2;
  if (isClarificationOrFollowUp(content)) return 0.4;

  const lower = content.toLowerCase();
  const hasBugKeyword =
    lower.includes("bug") ||
    lower.includes("not working") ||
    lower.includes("broken") ||
    lower.includes("wrong") ||
    lower.includes("incorrect") ||
    lower.includes("should") ||
    lower.includes("expected") ||
    lower.includes("instead") ||
    lower.includes("crash") ||
    lower.includes("infinite") ||
    lower.includes("free") ||
    lower.includes("can't") ||
    lower.includes("cannot") ||
    lower.includes("doesn't") ||
    lower.includes("does not") ||
    lower.includes("unable");

  if (cards.length > 0 && (hasBugKeyword || hasBugDescription)) return 0.9;
  if (cards.length > 0) return 0.75;
  if (hasBugKeyword || hasBugDescription) return 0.6;
  return 0.4;
}

export function extractSummary(content: string): string {
  // Find the first REAL sentence end, ignoring `.` that is not a boundary:
  // inside a URL, inside a `[[card]]` reference, or part of an ellipsis. Without
  // this, "[[welcome to...]]" truncates to the garbage summary "[[welcome to.".
  const masks: Array<[number, number]> = [];
  const addMasks = (re: RegExp): void => {
    for (const m of content.matchAll(re)) {
      if (m.index !== undefined) masks.push([m.index, m.index + m[0].length]);
    }
  };
  addMasks(/https?:\/\/\S+/g);
  addMasks(/\[\[.+?\]\]/g);
  addMasks(/\.\s*\.\s*\.|…/g);
  const inMask = (i: number): boolean => masks.some(([s, e]) => i >= s && i < e);

  for (let i = 0; i < content.length && i < 200; i++) {
    const ch = content[i];
    if ((ch === "." || ch === "!" || ch === "?") && !inMask(i)) {
      return content.slice(0, i + 1).trim();
    }
  }
  return content.slice(0, 150).trim();
}

function splitIntoItems(content: string): string[] {
  const numberedLines = content.split("\n").filter((l) => /^\d+[.)]\s/.test(l));
  if (numberedLines.length >= 2) return numberedLines;

  const bulletLines = content.split("\n").filter((l) => /^[-*•]\s/.test(l));
  if (bulletLines.length >= 2) return bulletLines;

  return [content];
}

function contentHash(content: string): string {
  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update(content);
  return hasher.digest("hex");
}

export async function extractReports(
  messages: RawDiscordMessage[],
): Promise<ReportItem[]> {
  const cardIndex = await loadCardNameIndex();
  const reports: ReportItem[] = [];

  for (const msg of messages) {
    if (msg.author_is_bot) continue;

    const guildId = msg.guild_id;
    const content = msg.content.trim();
    const isEvidenceOnly = content === "" && msg.attachments.length > 0;

    if (isEvidenceOnly) {
      const { cards, explicitCards } = detectCards("", cardIndex);
      reports.push({
        report_id: `discord:${msg.thread_id}:${msg.message_id}:0`,
        source: "discord",
        thread_id: msg.thread_id,
        thread_name: msg.thread_name,
        message_id: msg.message_id,
        item_index: 0,
        reported_at: msg.timestamp,
        author_name: msg.author_name,
        cards,
        explicitCards,
        mechanics: [],
        summary: "[evidence only — no text content]",
        actual: "",
        expected: "",
        evidence: {
          source_url: `https://discord.com/channels/${guildId}/${msg.thread_id}/${msg.message_id}`,
          attachments: msg.attachments,
          raw_content_hash: msg.content_hash,
        },
        extraction_confidence: 0.2,
        status: "unlinked",
      });
      continue;
    }

    if (content === "") continue;

    const items = splitIntoItems(content);

    items.forEach((item, itemIndex) => {
      const text = item.replace(/^\d+[.)]\s/, "").replace(/^[-*•]\s/, "").trim();
      if (text === "") return;

      const { cards, explicitCards } = detectCards(text, cardIndex);
      const mechanics = detectMechanics(text);
      const hasBugDescription = text.length > 30;
      const confidence = scoreConfidence(text, cards, hasBugDescription, false);
      const summary = extractSummary(text);

      reports.push({
        report_id: `discord:${msg.thread_id}:${msg.message_id}:${itemIndex}`,
        source: "discord",
        thread_id: msg.thread_id,
        thread_name: msg.thread_name,
        message_id: msg.message_id,
        item_index: itemIndex,
        reported_at: msg.timestamp,
        author_name: msg.author_name,
        cards,
        explicitCards,
        mechanics,
        summary,
        actual: text,
        expected: "",
        evidence: {
          source_url: `https://discord.com/channels/${guildId}/${msg.thread_id}/${msg.message_id}`,
          attachments: msg.attachments,
          raw_content_hash: msg.content_hash,
        },
        extraction_confidence: confidence,
        status: "unlinked",
      });
    });
  }

  return reports;
}
