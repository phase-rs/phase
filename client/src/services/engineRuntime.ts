import type {
  DeckCopyLimit,
  FormatConfig,
  GameFormat,
  SideboardPolicy,
  TokenCharacteristics,
  TokenImageRef,
  TokenPtProvenance,
} from "../adapter/types";
import {
  buildLocalSearchCard,
  loadScryfallData,
  type ScryfallCard,
} from "./scryfall";

type EngineModule = typeof import("@wasm/engine");

let engineModulePromise: Promise<EngineModule> | null = null;
let wasmInitPromise: Promise<void> | null = null;
let cardDbPromise: Promise<number> | null = null;

/**
 * A browser's module map retains failed dynamic imports for the document
 * lifetime. Retrying this requires a page reload, unlike WASM initialization
 * and card-data loading failures.
 */
export class EngineModuleReloadRequiredError extends Error {
  readonly cause: unknown;

  constructor(cause: unknown) {
    super("The engine module failed to load; reload the page to retry.");
    this.name = "EngineModuleReloadRequiredError";
    this.cause = cause;
  }
}

async function loadEngineModule(): Promise<EngineModule> {
  if (!engineModulePromise) {
    engineModulePromise = import("@wasm/engine").catch((cause: unknown) => {
      throw new EngineModuleReloadRequiredError(cause);
    });
  }
  return engineModulePromise;
}

export async function ensureWasmInit(): Promise<void> {
  if (!wasmInitPromise) {
    const pending = (async () => {
      const engine = await loadEngineModule();
      if (__ENGINE_WASM_URL__) {
        await engine.default({ module_or_path: __ENGINE_WASM_URL__ });
      } else {
        await engine.default();
      }
    })();
    wasmInitPromise = pending;
    void pending.catch((error: unknown) => {
      if (!(error instanceof EngineModuleReloadRequiredError) && wasmInitPromise === pending) {
        wasmInitPromise = null;
      }
    });
  }
  return wasmInitPromise;
}

export async function ensureCardDatabase(): Promise<number> {
  if (!cardDbPromise) {
    const pending = (async () => {
      await ensureWasmInit();
      const engine = await loadEngineModule();
      const resp = await fetch(__CARD_DATA_URL__);
      if (!resp.ok) {
        throw new Error(`Failed to load card-data.json (${resp.status})`);
      }
      const text = await resp.text();
      const loaded = await engine.load_card_database(text);
      if (loaded <= 0) {
        throw new Error("Failed to load card-data.json (no cards loaded)");
      }
      return loaded;
    })();
    cardDbPromise = pending;
    void pending.catch((error: unknown) => {
      if (!(error instanceof EngineModuleReloadRequiredError) && cardDbPromise === pending) {
        cardDbPromise = null;
      }
    });
  }
  return cardDbPromise;
}

export async function getCardFaceData(cardName: string) {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.get_card_face_data(cardName);
}

/** A localized card face from a per-language content-i18n sidecar. Fields are
 *  optional — absent fields fall back to the engine's English text. Mirrors the
 *  `LocalizedFace` struct emitted by `oracle-gen --sidecar-dir`. */
export interface LocalizedFace {
  name?: string;
  oracle_text?: string;
  type_line?: string;
}

const cardLocalePromises = new Map<string, Promise<Map<string, LocalizedFace>>>();

/**
 * Lazily fetch the per-locale card-content sidecar (`card-data.<lng>.json`) once,
 * into a Map keyed by lowercased canonical card name (the same key the engine's
 * `face_index` uses). English needs no sidecar. A missing sidecar (e.g. 404 for a
 * locale not yet published) resolves to an empty map so callers fall back to
 * English per-field — content localization is best-effort display data, never a
 * hard dependency.
 */
export async function ensureCardLocale(lang: string): Promise<Map<string, LocalizedFace>> {
  if (lang === "en") return new Map();
  let promise = cardLocalePromises.get(lang);
  if (!promise) {
    promise = (async () => {
      const url = __CARD_DATA_LOCALE_URL_TEMPLATE__.replace("{lng}", lang);
      const resp = await fetch(url);
      if (!resp.ok) return new Map<string, LocalizedFace>();
      const obj = (await resp.json()) as Record<string, LocalizedFace>;
      return new Map(Object.entries(obj));
    })().catch(() => new Map<string, LocalizedFace>());
    cardLocalePromises.set(lang, promise);
  }
  return promise;
}

export async function getCardParseDetails(cardName: string) {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.get_card_parse_details(cardName);
}

/**
 * A deck-builder card search. Mirrors the engine's `CardSearchQuery`
 * (`crates/engine/src/database/search.rs`). All fields optional; an empty query
 * matches nothing — callers gate on "has criteria" first.
 */
export interface CardSearchQuery {
  text?: string;
  /** WUBRG color letters the card's colors must include (superset match). */
  colors?: string[];
  /** A type word (core type, supertype, or subtype). */
  type?: string;
  cmcMax?: number;
  /** Set codes; card must have a printing in at least one. */
  sets?: string[];
  /** A legality-format key (e.g. `"modern"`); card must be legal in it. */
  legalFormat?: string;
  limit?: number;
}

/** Engine result shape — rules data only (see `CardSearchResult` in the engine). */
interface EngineCardSearchResult {
  name: string;
  oracle_id: string | null;
  mana_value: number;
  color_identity: string[];
  legalities: Record<string, string>;
}

interface EngineCardSearchResults {
  results: EngineCardSearchResult[];
  total: number;
}

/**
 * Search the local card database through the engine. The engine is the single
 * authority for the rules data search filters on (legality, sets, types, mana
 * value, colors); the frontend hydrates artwork and type lines from the local
 * Scryfall image map. No network search ever leaves the device.
 */
export async function searchCards(
  query: CardSearchQuery,
): Promise<{ cards: ScryfallCard[]; total: number }> {
  await ensureCardDatabase();
  // Hydration of artwork/type line needs the image map resolved.
  await loadScryfallData();
  const engine = await loadEngineModule();
  const { results, total } = engine.search_cards_js({
    text: query.text ?? "",
    colors: query.colors ?? [],
    type_line: query.type ?? "",
    cmc_max: query.cmcMax ?? null,
    sets: query.sets ?? [],
    legal_format: query.legalFormat ?? null,
    limit: query.limit ?? null,
  }) as EngineCardSearchResults;

  const cards = results.map((result) =>
    buildLocalSearchCard({
      oracleId: result.oracle_id ?? undefined,
      name: result.name,
      cmc: result.mana_value,
      colorIdentity: result.color_identity,
      legalities: result.legalities,
    }),
  );
  return { cards, total };
}

export async function getCardRulings(cardName: string): Promise<CardRuling[]> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return (engine.get_card_rulings(cardName) as CardRuling[]) ?? [];
}

/** An official WotC ruling: date + body text. Mirrors the Rust `Ruling` struct. */
export interface CardRuling {
  date: string;
  text: string;
}

export async function evaluateDeckCompatibilityJs(request: unknown) {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.evaluate_deck_compatibility_js(request);
}

/** Archetype classification from phase-ai. The engine is the single authority —
 *  never compute archetype client-side. */
export type DeckArchetype = "Aggro" | "Midrange" | "Control" | "Combo" | "Ramp";

export interface DeckProfileResult {
  archetype: DeckArchetype;
  confidence: "Pure" | "Hybrid";
  /** Present only when `confidence === "Hybrid"`. */
  secondary?: DeckArchetype;
}

/** Classify a deck's archetype from a flat list of card names. */
export async function classifyDeck(cardNames: string[]): Promise<DeckProfileResult> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.classify_deck_js(cardNames) as DeckProfileResult;
}

/// CR 903.3: Whether the named card can be a commander
/// (legendary creature, legendary background, or "can be your commander").
export async function isCardCommanderEligible(name: string): Promise<boolean> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.is_card_commander_eligible(name);
}

export async function isCardCommanderEligibleForFormat(
  name: string,
  format: GameFormat,
): Promise<boolean> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.isCardCommanderEligibleForFormat(name, format);
}

/**
 * CR 702.124: Of `candidates`, which can legally pair with `firstCommander` as a
 * co-commander? The engine is the single authority for the partner family
 * (Partner, Partner with [Name], Friends Forever, Character Select, Doctor's
 * Companion, Choose a Background) — the frontend never re-derives these rules.
 *
 * `draftSetCodes` is every set whose draft boosters this deck's draft CONTAINED,
 * or an empty array for constructed play. CR 903.13f(3) extends the partner
 * ability at deckbuilding "if the draft contained draft boosters from Commander
 * Masters" — a property of the DRAFT, not of a card. A LIST because that rule
 * asks about containment: a mixed draft that opened Commander Masters boosters
 * among others contained them, and the grant is in force. The client passes the
 * set codes through and the engine decides what they grant; which sets grant
 * what is engine knowledge and must never be mirrored here.
 */
export async function commanderPartnerCandidates(
  firstCommander: string,
  candidates: string[],
  draftSetCodes: readonly string[],
): Promise<string[]> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.commanderPartnerCandidates(
    firstCommander,
    candidates,
    [...draftSetCodes],
  ) as string[];
}

export type SignatureSpellSelectionPolicy =
  | { type: "None" }
  | { type: "Required"; data: { candidates: string[] } };

/** Returns the engine-authored Oathbreaker signature-spell selection policy. */
export async function signatureSpellSelectionPolicy(
  request: unknown,
): Promise<SignatureSpellSelectionPolicy> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.signatureSpellSelectionPolicy(request) as SignatureSpellSelectionPolicy;
}

/** Returns the engine-approved Commander-family companion candidates. */
export async function companionCandidates(request: unknown): Promise<string[]> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.companionCandidates(request) as string[];
}

/**
 * Engine-owned deck-construction unions, re-exported so deck-builder callers
 * keep a single import site. `DeckCopyLimit` is a card's / format's copy
 * ceiling (CR 100.2a / CR 903.5b) and `SideboardPolicy` a format's sideboard
 * rule (CR 100.4a). Both are discriminated unions whose unit variants carry no
 * `data` field — always switch on `type`, never destructure `data`
 * unconditionally. The engine is the single authority; the frontend never
 * re-parses Oracle text or hardcodes a cap.
 */
export type { DeckCopyLimit, SideboardPolicy } from "../adapter/types";

/**
 * Query the engine for a card's deck-construction copy-limit override. Returns
 * `null` when the format-default limit applies. The frontend must not infer
 * this from Oracle text — the engine owns the rule.
 */
export async function deckCopyLimit(name: string): Promise<DeckCopyLimit | null> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.deckCopyLimit(name) as DeckCopyLimit | null;
}

/**
 * CR 100.2a / CR 903.5b: How many copies of a card a deck built under
 * `formatConfig` may hold across main deck, sideboard, and command zone
 * combined (CR 100.4a).
 *
 * Unlike `deckCopyLimit` (which reports only a card's printed override), this
 * is the resolved ceiling — the engine has already applied the basic-land
 * exemption, the printed override, and the format default. Compare a combined
 * count against it directly; never re-derive four-of / singleton client-side.
 *
 * Pass the registry's `default_config`, not a bare format string: only the
 * config carries a custom format's declared copy limit.
 */
export async function maxDeckCopies(
  name: string,
  formatConfig: FormatConfig,
): Promise<DeckCopyLimit> {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.maxDeckCopies(name, formatConfig) as DeckCopyLimit;
}

/**
 * Query the engine for the sideboard policy of a resolved `FormatConfig`. The
 * engine is the single authority for these rules — the frontend never hardcodes
 * 15 or any other cap. Pass the registry's `default_config`, not a bare format
 * string: only the config carries a custom format's declared policy.
 */
export async function sideboardPolicyForFormat(
  formatConfig: FormatConfig,
): Promise<SideboardPolicy> {
  await ensureWasmInit();
  const engine = await loadEngineModule();
  return engine.sideboardPolicyForFormat(formatConfig) as SideboardPolicy;
}

/**
 * Engine-typed catalog of debug-spawnable token presets. Loaded once on
 * first access; the result is cached for the session because the catalog is
 * static engine data (compiled into the WASM binary via `include_str!`).
 */
export type PredefinedTokenKind =
  | "Treasure"
  | "Food"
  | "Gold"
  | "Clue"
  | "Blood"
  | "Powerstone"
  | "Map"
  | "Lander";

export type TokenCategory =
  | { PredefinedArtifact: { kind: PredefinedTokenKind } }
  | "Creature"
  | "Aura"
  | "Equipment"
  | "Vehicle"
  | "Enchantment"
  | "Land"
  | "Artifact";

export type PresetFidelity = "Full" | "PartialMissingAbilities";

export interface TokenPreset {
  id: string;
  category: TokenCategory;
  fidelity: PresetFidelity;
  pt_provenance?: TokenPtProvenance;
  body: TokenCharacteristics;
  source_card_names?: string[];
  source_card_refs?: Array<{
    card_name: string;
    face_name?: string | null;
    scryfall_oracle_id?: string | null;
    scryfall_id?: string | null;
  }>;
  token_image_ref?: TokenImageRef | null;
  set_code?: string;
  set_name?: string;
  collector_number?: string | null;
  released_at?: string | null;
  type_line?: string;
  rules_text?: string | null;
}

let tokenPresetsCache: TokenPreset[] | null = null;

export async function listTokenPresets(): Promise<TokenPreset[]> {
  if (tokenPresetsCache !== null) return tokenPresetsCache;
  await ensureWasmInit();
  const engine = await loadEngineModule();
  tokenPresetsCache = engine.list_token_presets_js() as TokenPreset[];
  return tokenPresetsCache;
}
