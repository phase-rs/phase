import type { RawDiscordMessage, ReportItem } from "./types.ts";

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

let cardNamesCache: Set<string> | null = null;

async function loadCardNames(): Promise<Set<string>> {
  if (cardNamesCache !== null) return cardNamesCache;
  const file = Bun.file("client/public/card-data.json");
  const data = (await file.json()) as Record<string, unknown>;
  cardNamesCache = new Set(Object.keys(data));
  return cardNamesCache;
}

function detectMechanics(text: string): string[] {
  const lower = text.toLowerCase();
  const found = new Set<string>();
  for (const kw of MECHANIC_KEYWORDS) {
    if (lower.includes(kw)) found.add(kw);
  }
  return [...found];
}

function detectCards(text: string, cardNames: Set<string>): string[] {
  const lower = text.toLowerCase();
  const found = new Set<string>();
  for (const name of cardNames) {
    if (lower.includes(name)) found.add(name);
  }
  return [...found];
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

function extractSummary(content: string): string {
  const firstSentenceEnd = content.search(/[.!?]/);
  if (firstSentenceEnd !== -1 && firstSentenceEnd < 200) {
    return content.slice(0, firstSentenceEnd + 1).trim();
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
  const cardNames = await loadCardNames();
  const reports: ReportItem[] = [];

  for (const msg of messages) {
    if (msg.author_is_bot) continue;

    const guildId = msg.guild_id;
    const content = msg.content.trim();
    const isEvidenceOnly = content === "" && msg.attachments.length > 0;

    if (isEvidenceOnly) {
      const cards = detectCards("", cardNames);
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

      const cards = detectCards(text, cardNames);
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
