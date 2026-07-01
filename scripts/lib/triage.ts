import type { ReportItem } from "./types.ts";

export type TriageClassification =
  | "primary_report"
  | "additional_report"
  | "follow_up"
  | "developer_reply"
  | "correction"
  | "chatter"
  | "evidence_only";

export interface TriageItem {
  report_id: string;
  classification: TriageClassification;
  reason: string;
  thread_id: string;
  thread_name: string;
  message_id: string;
  cards: string[];
  /** Trusted `[[Card]]` / Scryfall-link subset of `cards` (see ./types.ts). */
  explicitCards?: string[];
  summary: string;
  extraction_confidence: number;
  source_url: string;
  parser_status: "fully_parsed" | "has_gaps" | "unknown_card" | "no_card";
  // Kept in lockstep with the canonical definition in ./types.ts. The script
  // never pre-judges duplication; an LLM operator is the sole arbiter of whether
  // a report duplicates an existing GH issue.
  proposed_action: "create_issue" | "skip" | "needs_human_review";
  dedup_group: string | null;
}

// ---------------------------------------------------------------------------
// Card data cache
// ---------------------------------------------------------------------------

let cardDataCache: Record<string, unknown> | null = null;

async function loadCardData(): Promise<Record<string, unknown>> {
  if (cardDataCache !== null) return cardDataCache;
  const file = Bun.file("client/public/card-data.json");
  cardDataCache = (await file.json()) as Record<string, unknown>;
  return cardDataCache;
}

// ---------------------------------------------------------------------------
// Parser status check
// ---------------------------------------------------------------------------

function checkCardParserStatus(
  cardName: string,
  cardData: Record<string, unknown>,
): "fully_parsed" | "has_gaps" | "unknown_card" {
  const card = cardData[cardName] as
    | { abilities?: Array<{ effect?: { type?: string } }>; triggers?: Array<{ mode?: string }> }
    | undefined;
  if (!card) return "unknown_card";

  const hasUnimpl = (card.abilities ?? []).some((a) => a.effect?.type === "Unimplemented");
  const hasUnknown = (card.triggers ?? []).some((t) => t.mode === "Unknown");

  if (!hasUnimpl && !hasUnknown) return "fully_parsed";
  return "has_gaps";
}

function getParserStatus(
  cards: string[],
  cardData: Record<string, unknown>,
): TriageItem["parser_status"] {
  if (cards.length === 0) return "no_card";

  let allFullyParsed = true;
  let anyKnown = false;

  for (const card of cards) {
    const status = checkCardParserStatus(card.toLowerCase(), cardData);
    if (status === "unknown_card") continue;
    anyKnown = true;
    if (status === "has_gaps") allFullyParsed = false;
  }

  if (!anyKnown) return "unknown_card";
  if (allFullyParsed) return "fully_parsed";
  return "has_gaps";
}

// ---------------------------------------------------------------------------
// Classification heuristics
// ---------------------------------------------------------------------------

const DEVELOPER_PHRASES = [
  "looking into it",
  "fixed in",
  "this should be",
  "will fix",
  "good catch",
  "can you try",
  "just pushed",
  "latest build",
  "current data",
  "confirmed",
  "investigating",
  "i'll take a look",
  "take a look",
  "i'm unable to reproduce",
  "unable to reproduce",
  "i've been working on",
  "working on it",
  "appreciate your",
  "thank you for the report",
  "thanks for the report",
  "let me check",
];

const CORRECTION_PHRASES = [
  "scratch that",
  "never mind",
  "nevermind",
  "nvm",
  "ignore that",
  "false alarm",
  "my bad",
  "correction:",
  "update:",
  "i was wrong",
  "actually,",
  "actually it",
  "actually that",
];

function isDeveloperReply(summary: string, actual: string): boolean {
  const lower = (summary + " " + actual).toLowerCase();
  return DEVELOPER_PHRASES.some((phrase) => lower.includes(phrase));
}

function isCorrection(summary: string, actual: string): boolean {
  const lower = (summary + " " + actual).toLowerCase().trim();
  return CORRECTION_PHRASES.some((phrase) => lower.startsWith(phrase) || lower.includes(phrase));
}

function isChatter(summary: string, actual: string): boolean {
  const combined = (summary + " " + actual).trim();
  if (combined.length < 20) return true;
  // Pure emoji / reaction messages
  if (/^[\p{Emoji}\s]+$/u.test(combined)) return true;
  return false;
}

// ---------------------------------------------------------------------------
// Main triage function
// ---------------------------------------------------------------------------

export async function triageReports(reports: ReportItem[]): Promise<TriageItem[]> {
  const cardData = await loadCardData();

  // Group by thread_id, sort each group by reported_at ascending
  const byThread = new Map<string, ReportItem[]>();
  for (const r of reports) {
    if (!byThread.has(r.thread_id)) byThread.set(r.thread_id, []);
    byThread.get(r.thread_id)!.push(r);
  }
  for (const items of byThread.values()) {
    items.sort((a, b) => a.reported_at.localeCompare(b.reported_at));
  }

  const result: TriageItem[] = [];

  for (const [, threadItems] of byThread) {
    const firstReportedAt = threadItems[0].reported_at;
    const threadStarterAuthor = threadItems[0].author_name;

    // Track whether we've emitted a primary for this thread
    let threadHasPrimary = false;

    for (const r of threadItems) {
      const parserStatus = getParserStatus(r.cards, cardData);
      const sourceUrl = r.evidence.source_url;
      const dedup_group = r.cards.length > 0 ? r.cards[0].toLowerCase() : null;
      const isFirstInThread = r.reported_at === firstReportedAt;
      const summary = r.summary;
      const actual = r.actual;

      // --- Evidence-only ---
      if (summary.startsWith("[evidence only")) {
        result.push({
          report_id: r.report_id,
          classification: "evidence_only",
          reason: "Attachment-only message with no text content",
          thread_id: r.thread_id,
          thread_name: r.thread_name,
          message_id: r.message_id,
          cards: r.cards,
          explicitCards: r.explicitCards,
          summary,
          extraction_confidence: r.extraction_confidence,
          source_url: sourceUrl,
          parser_status: parserStatus,
          proposed_action: "skip",
          dedup_group,
        });
        continue;
      }

      // --- Correction ---
      if (isCorrection(summary, actual)) {
        result.push({
          report_id: r.report_id,
          classification: "correction",
          reason: "Message contains correction/retraction phrase",
          thread_id: r.thread_id,
          thread_name: r.thread_name,
          message_id: r.message_id,
          cards: r.cards,
          explicitCards: r.explicitCards,
          summary,
          extraction_confidence: r.extraction_confidence,
          source_url: sourceUrl,
          parser_status: parserStatus,
          proposed_action: "skip",
          dedup_group,
        });
        continue;
      }

      // --- Developer reply ---
      if (isDeveloperReply(summary, actual)) {
        result.push({
          report_id: r.report_id,
          classification: "developer_reply",
          reason: "Message contains developer/maintainer response phrases",
          thread_id: r.thread_id,
          thread_name: r.thread_name,
          message_id: r.message_id,
          cards: r.cards,
          explicitCards: r.explicitCards,
          summary,
          extraction_confidence: r.extraction_confidence,
          source_url: sourceUrl,
          parser_status: parserStatus,
          proposed_action: "skip",
          dedup_group,
        });
        continue;
      }

      // --- Chatter ---
      if (isChatter(summary, actual)) {
        result.push({
          report_id: r.report_id,
          classification: "chatter",
          reason: "Very short or emoji-only message without bug context",
          thread_id: r.thread_id,
          thread_name: r.thread_name,
          message_id: r.message_id,
          cards: r.cards,
          explicitCards: r.explicitCards,
          summary,
          extraction_confidence: r.extraction_confidence,
          source_url: sourceUrl,
          parser_status: parserStatus,
          proposed_action: "skip",
          dedup_group,
        });
        continue;
      }

      // --- Non-first-in-thread, non-starter: follow_up ---
      if (!isFirstInThread && r.author_name !== threadStarterAuthor) {
        // Could still be a distinct additional report if it has cards and reasonable confidence
        const isDistinctReport =
          r.cards.length > 0 &&
          r.extraction_confidence >= 0.7 &&
          r.cards.some((c) => !threadItems[0].cards.includes(c));

        const classification: TriageClassification = isDistinctReport
          ? "additional_report"
          : "follow_up";

        result.push({
          report_id: r.report_id,
          classification,
          reason: isDistinctReport
            ? "Later message from different author with new card names and high confidence"
            : "Later message from a different author in the thread",
          thread_id: r.thread_id,
          thread_name: r.thread_name,
          message_id: r.message_id,
          cards: r.cards,
          explicitCards: r.explicitCards,
          summary,
          extraction_confidence: r.extraction_confidence,
          source_url: sourceUrl,
          parser_status: parserStatus,
          proposed_action: isDistinctReport ? "needs_human_review" : "skip",
          dedup_group,
        });
        continue;
      }

      // --- Non-first-in-thread, same author as thread starter: follow_up ---
      if (!isFirstInThread) {
        result.push({
          report_id: r.report_id,
          classification: "follow_up",
          reason: "Later message from the original thread reporter adding context",
          thread_id: r.thread_id,
          thread_name: r.thread_name,
          message_id: r.message_id,
          cards: r.cards,
          explicitCards: r.explicitCards,
          summary,
          extraction_confidence: r.extraction_confidence,
          source_url: sourceUrl,
          parser_status: parserStatus,
          // Same-thread follow-up from the original reporter — context for the
          // thread's primary report, not its own issue. Publish files one issue
          // per thread from the primary, so this item is skipped.
          proposed_action: "skip",
          dedup_group,
        });
        continue;
      }

      // --- First in thread (primary candidate) ---
      // Low-signal primaries go to the operator; everything else is a candidate
      // issue. The script does NOT guess duplicates — the LLM operator decides
      // whether a candidate duplicates an existing GH issue.
      const proposed_action: TriageItem["proposed_action"] =
        r.cards.length === 0 || r.extraction_confidence < 0.7
          ? "needs_human_review"
          : "create_issue";

      const classification: TriageClassification = threadHasPrimary
        ? "additional_report"
        : "primary_report";

      if (!threadHasPrimary) threadHasPrimary = true;

      result.push({
        report_id: r.report_id,
        classification,
        reason: isFirstInThread
          ? "First message in thread"
          : "Thread starter's additional report item",
        thread_id: r.thread_id,
        thread_name: r.thread_name,
        message_id: r.message_id,
        cards: r.cards,
        explicitCards: r.explicitCards,
        summary,
        extraction_confidence: r.extraction_confidence,
        source_url: sourceUrl,
        parser_status: parserStatus,
        proposed_action,
        dedup_group,
      });
    }
  }

  return result;
}
