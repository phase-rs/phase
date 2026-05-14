import { existsSync, readFileSync } from "node:fs";

interface LlmTriageItem {
  thread_id: string;
  thread_name: string;
  priority: string;
  category: string;
  cards: string[];
  summary: string;
  source_url: string;
  [key: string]: unknown;
}

interface CardStatus {
  [cardName: string]: "fully_parsed" | "has_gaps" | "unknown_card";
}

export interface CrossrefItem {
  report_id: string;
  thread_name: string;
  priority: string;
  category: string;
  cards: string[];
  card_statuses: CardStatus;
  overall_status: "needs_semantic_verify" | "still_broken" | "unknown_card" | "no_card";
  summary: string;
  source_url: string;
}

interface CardData {
  abilities?: Array<{ effect?: { type?: string } }>;
  triggers?: Array<{ mode?: string }>;
}

function checkCard(
  name: string,
  cardData: Record<string, unknown>,
): "fully_parsed" | "has_gaps" | "unknown_card" {
  const card = cardData[name.toLowerCase()] as CardData | undefined;
  if (!card) return "unknown_card";

  const hasUnimpl = (card.abilities ?? []).some((a) => a.effect?.type === "Unimplemented");
  const hasUnknown = (card.triggers ?? []).some((t) => t.mode === "Unknown");

  return hasUnimpl || hasUnknown ? "has_gaps" : "fully_parsed";
}

export async function crossReference(
  llmTriagePath: string,
  cardDataPath: string,
): Promise<CrossrefItem[]> {
  const cardData = (await Bun.file(cardDataPath).json()) as Record<string, unknown>;

  const mappingPath = "triage/unknown-card-mapping.json";
  let nameMapping: Record<string, { correct_name: string | null }> = {};
  if (existsSync(mappingPath)) {
    nameMapping = (await Bun.file(mappingPath).json()) as typeof nameMapping;
  }

  const lines = readFileSync(llmTriagePath, "utf8")
    .split("\n")
    .filter((l) => l.trim() !== "");

  const results: CrossrefItem[] = [];

  for (const [i, line] of lines.entries()) {
    const item = JSON.parse(line) as LlmTriageItem;
    const resolvedCards = item.cards.map((c) => {
      const mapped = nameMapping[c]?.correct_name ?? nameMapping[c.toLowerCase()]?.correct_name;
      return mapped ?? c;
    });

    const cardStatuses: CardStatus = {};
    for (const card of resolvedCards) {
      cardStatuses[card] = checkCard(card, cardData);
    }

    let overall: CrossrefItem["overall_status"];
    if (resolvedCards.length === 0) {
      overall = "no_card";
    } else {
      const statuses = Object.values(cardStatuses);
      const anyGaps = statuses.includes("has_gaps");
      const anyKnown = statuses.some((s) => s !== "unknown_card");
      const allUnknown = statuses.every((s) => s === "unknown_card");

      if (allUnknown) overall = "unknown_card";
      else if (anyGaps) overall = "still_broken";
      else overall = "needs_semantic_verify";
    }

    results.push({
      report_id: `${item.thread_id}_${i}`,
      thread_name: item.thread_name,
      priority: item.priority,
      category: item.category,
      cards: resolvedCards,
      card_statuses: cardStatuses,
      overall_status: overall,
      summary: item.summary,
      source_url: item.source_url,
    });
  }

  return results;
}
