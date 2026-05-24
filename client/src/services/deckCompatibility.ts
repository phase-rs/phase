import type { GameFormat, MatchType } from "../adapter/types";
import { EngineWorkerClient } from "../adapter/engine-worker-client";
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
  summary_only?: boolean;
}

interface EvaluateOptions {
  selectedFormat?: GameFormat | null;
  selectedMatchType?: MatchType | null;
  summaryOnly?: boolean;
  onResult?: (name: string, result: DeckCompatibilityResult) => void;
  onStatus?: (status: "starting-worker" | "loading-card-database" | "checking-deck", name?: string) => void;
}

let compatibilityWorkerPromise: Promise<EngineWorkerClient> | null = null;
const fullCompatibilityCache = new Map<string, DeckCompatibilityResult>();
const summaryCompatibilityCache = new Map<string, DeckCompatibilityResult>();
const fullCompatibilityInflight = new Map<string, Promise<DeckCompatibilityResult>>();
const summaryCompatibilityInflight = new Map<string, Promise<DeckCompatibilityResult>>();

async function getCompatibilityWorker(onStatus?: EvaluateOptions["onStatus"]): Promise<EngineWorkerClient> {
  if (!compatibilityWorkerPromise) {
    compatibilityWorkerPromise = (async () => {
      onStatus?.("starting-worker");
      const worker = new EngineWorkerClient();
      await worker.initialize();
      onStatus?.("loading-card-database");
      await worker.loadCardDbFromUrl();
      return worker;
    })();
  }
  return compatibilityWorkerPromise;
}

/**
 * Tear down the singleton compatibility worker, releasing its full
 * card-database WASM instance. Called when leaving setup to start a match so
 * its ~full-DB linear memory is not resident concurrently with game init
 * (which allocates its own engine instance + per-game card faces) — the
 * combined peak is what traps as "memory access out of bounds" on
 * low-memory devices. The worker is lazily recreated by
 * `getCompatibilityWorker` on the next compatibility check. Result caches are
 * intentionally retained (small JSON, cheap, and useful on return to setup);
 * only the in-flight maps are dropped so a dispose mid-request can't leave a
 * rejected promise cached under its key.
 */
export function disposeCompatibilityWorker(): void {
  const pending = compatibilityWorkerPromise;
  compatibilityWorkerPromise = null;
  fullCompatibilityInflight.clear();
  summaryCompatibilityInflight.clear();
  if (pending) {
    void pending.then((worker) => worker.dispose()).catch(() => {});
  }
}

function buildRequest(deck: ParsedDeck, options: EvaluateOptions): DeckCompatibilityRequest {
  return {
    ...expandParsedDeck(deck),
    selected_format: options.selectedFormat ?? null,
    selected_match_type: options.selectedMatchType ?? null,
    summary_only: options.summaryOnly ?? false,
  };
}

function compatibilityCacheKey(request: DeckCompatibilityRequest): string {
  return JSON.stringify({
    main_deck: request.main_deck,
    sideboard: request.sideboard,
    commander: request.commander,
    selected_format: request.selected_format ?? null,
    selected_match_type: request.selected_match_type ?? null,
  });
}

export async function evaluateDeckCompatibility(
  deck: ParsedDeck,
  options: EvaluateOptions = {},
): Promise<DeckCompatibilityResult> {
  const request = buildRequest(deck, options);
  const cacheKey = compatibilityCacheKey(request);
  if (request.summary_only) {
    const cached = fullCompatibilityCache.get(cacheKey) ?? summaryCompatibilityCache.get(cacheKey);
    if (cached) {
      return cached;
    }
  } else {
    const cached = fullCompatibilityCache.get(cacheKey);
    if (cached) {
      return cached;
    }
  }

  const inflightMap = request.summary_only ? summaryCompatibilityInflight : fullCompatibilityInflight;
  const existingInflight = request.summary_only
    ? (fullCompatibilityInflight.get(cacheKey) ?? summaryCompatibilityInflight.get(cacheKey))
    : fullCompatibilityInflight.get(cacheKey);
  if (existingInflight) return existingInflight;

  const promise = evaluateDeckCompatibilityUncached(request, options).then((result) => {
    if (request.summary_only) {
      summaryCompatibilityCache.set(cacheKey, result);
    } else {
      fullCompatibilityCache.set(cacheKey, result);
      summaryCompatibilityCache.set(cacheKey, result);
    }
    return result;
  }).finally(() => {
    inflightMap.delete(cacheKey);
  });
  inflightMap.set(cacheKey, promise);
  return promise;
}

async function evaluateDeckCompatibilityUncached(
  request: DeckCompatibilityRequest,
  options: EvaluateOptions,
): Promise<DeckCompatibilityResult> {
  const worker = await getCompatibilityWorker(options.onStatus);
  options.onStatus?.("checking-deck");
  return await worker.evaluateDeckCompatibility(request) as DeckCompatibilityResult;
}

export async function evaluateDeckCompatibilityBatch(
  decks: Array<{ name: string; deck: ParsedDeck }>,
  options: EvaluateOptions = {},
): Promise<Record<string, DeckCompatibilityResult>> {
  const results: Record<string, DeckCompatibilityResult> = {};
  for (const { name, deck } of decks) {
    const result = await evaluateDeckCompatibility(deck, {
      ...options,
      onStatus: (status) => options.onStatus?.(status, name),
    });
    results[name] = result;
    options.onResult?.(name, result);
  }

  return results;
}
