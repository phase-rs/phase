import init, {
  evaluate_deck_compatibility_js,
  load_card_database,
} from "@wasm/engine";
import type { GameFormat, MatchType } from "../adapter/types";
import type { ParsedDeck } from "./deckParser";

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

let wasmInitPromise: Promise<void> | null = null;
let cardDbLoadPromise: Promise<void> | null = null;

function ensureWasmInit(): Promise<void> {
  if (!wasmInitPromise) {
    wasmInitPromise = init().then(() => {});
  }
  return wasmInitPromise;
}

async function ensureCardDatabase(): Promise<void> {
  if (!cardDbLoadPromise) {
    cardDbLoadPromise = (async () => {
      const response = await fetch(__CARD_DATA_URL__);
      if (!response.ok) {
        throw new Error(`Failed to load card-data.json (${response.status})`);
      }
      const data = await response.text();
      load_card_database(data);
    })();
  }
  return cardDbLoadPromise;
}

function expandDeckEntries(entries: ParsedDeck["main"]): string[] {
  const cards: string[] = [];
  for (const entry of entries) {
    for (let i = 0; i < entry.count; i++) {
      cards.push(entry.name);
    }
  }
  return cards;
}

function buildRequest(deck: ParsedDeck, options: EvaluateOptions): DeckCompatibilityRequest {
  return {
    main_deck: expandDeckEntries(deck.main),
    sideboard: expandDeckEntries(deck.sideboard),
    commander: deck.commander ?? [],
    selected_format: options.selectedFormat ?? null,
    selected_match_type: options.selectedMatchType ?? null,
  };
}

export async function evaluateDeckCompatibility(
  deck: ParsedDeck,
  options: EvaluateOptions = {},
): Promise<DeckCompatibilityResult> {
  await ensureWasmInit();
  await ensureCardDatabase();

  const request = buildRequest(deck, options);
  return evaluate_deck_compatibility_js(request) as DeckCompatibilityResult;
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
