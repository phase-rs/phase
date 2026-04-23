import type { GameFormat, MatchType } from "../adapter/types";
import { evaluateDeckCompatibilityJs } from "./engineRuntime";
import { expandParsedDeck, type ParsedDeck } from "./deckParser";

export interface CompatibilityCheck {
  compatible: boolean;
  reasons: string[];
}

export type ParseCategory = "keyword" | "ability" | "trigger" | "static" | "replacement" | "cost";

export interface ParsedItem {
  category: ParseCategory;
  label: string;
  source_text?: string;
  supported: boolean;
  details?: [string, string][];
  children?: ParsedItem[];
}

export interface UnsupportedCard {
  name: string;
  gaps: string[];
  copies?: number;
  oracle_text?: string;
  parse_details?: ParsedItem[];
}

export interface DeckCoverage {
  total_unique: number;
  supported_unique: number;
  unsupported_cards: UnsupportedCard[];
}

export interface DeckCompatibilityResult {
  standard: CompatibilityCheck;
  commander: CompatibilityCheck;
  bo3_ready: boolean;
  unknown_cards: string[];
  selected_format_compatible?: boolean | null;
  selected_format_reasons: string[];
  /** Combined color identity of all cards in the deck, in WUBRG order (e.g. ["W", "U", "R"]). */
  color_identity: string[];
  /** Engine coverage summary — how many unique cards are fully supported. */
  coverage?: DeckCoverage | null;
  /** Per-format legality: maps format key (e.g. "standard", "modern") to status ("legal", "not_legal", "banned"). */
  format_legality?: Record<string, string>;
}

interface DeckCompatibilityRequest {
  main_deck: string[];
  sideboard: string[];
  commander: string[];
  selected_format?: GameFormat | null;
  selected_match_type?: MatchType | null;
}

interface EvaluateOptions {
  selectedFormat?: GameFormat | null;
  selectedMatchType?: MatchType | null;
}

function buildRequest(deck: ParsedDeck, options: EvaluateOptions): DeckCompatibilityRequest {
  return {
    ...expandParsedDeck(deck),
    selected_format: options.selectedFormat ?? null,
    selected_match_type: options.selectedMatchType ?? null,
  };
}

export async function evaluateDeckCompatibility(
  deck: ParsedDeck,
  options: EvaluateOptions = {},
): Promise<DeckCompatibilityResult> {
  const request = buildRequest(deck, options);
  return await evaluateDeckCompatibilityJs(request) as DeckCompatibilityResult;
}

export async function evaluateDeckCompatibilityBatch(
  decks: Array<{ name: string; deck: ParsedDeck }>,
  options: EvaluateOptions = {},
): Promise<Record<string, DeckCompatibilityResult>> {
  const results = await Promise.all(
    decks.map(async ({ name, deck }) => ({ name, result: await evaluateDeckCompatibility(deck, options) })),
  );

  return results.reduce<Record<string, DeckCompatibilityResult>>((acc, entry) => {
    acc[entry.name] = entry.result;
    return acc;
  }, {});
}
