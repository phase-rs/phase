import type { GameFormat } from "../adapter/types";

type EngineModule = typeof import("@wasm/engine");

let engineModulePromise: Promise<EngineModule> | null = null;
let wasmInitPromise: Promise<void> | null = null;
let cardDbPromise: Promise<number> | null = null;

async function loadEngineModule(): Promise<EngineModule> {
  if (!engineModulePromise) {
    engineModulePromise = import("@wasm/engine");
  }
  return engineModulePromise;
}

export async function ensureWasmInit(): Promise<void> {
  if (!wasmInitPromise) {
    wasmInitPromise = (async () => {
      const engine = await loadEngineModule();
      await engine.default();
    })();
  }
  return wasmInitPromise;
}

export async function ensureCardDatabase(): Promise<number> {
  if (!cardDbPromise) {
    cardDbPromise = (async () => {
      await ensureWasmInit();
      const engine = await loadEngineModule();
      const resp = await fetch(__CARD_DATA_URL__);
      if (!resp.ok) {
        throw new Error(`Failed to load card-data.json (${resp.status})`);
      }
      const text = await resp.text();
      return engine.load_card_database(text);
    })();
  }
  return cardDbPromise;
}

export async function getCardFaceData(cardName: string) {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.get_card_face_data(cardName);
}

export async function getCardParseDetails(cardName: string) {
  await ensureCardDatabase();
  const engine = await loadEngineModule();
  return engine.get_card_parse_details(cardName);
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

/**
 * CR 100.4a: Per-format sideboard policy as a discriminated union.
 *
 * `Forbidden` and `Unlimited` are unit variants and do not carry a `data`
 * field — always exhaustive-switch on `type`, never destructure `data`
 * unconditionally.
 */
export type SideboardPolicy =
  | { type: "Forbidden" }
  | { type: "Limited"; data: number }
  | { type: "Unlimited" };

/**
 * Query the engine for the sideboard policy of a given format. The engine is
 * the single authority for these rules — the frontend never hardcodes 15
 * or any other cap.
 */
export async function sideboardPolicyForFormat(
  format: GameFormat,
): Promise<SideboardPolicy> {
  await ensureWasmInit();
  const engine = await loadEngineModule();
  return engine.sideboardPolicyForFormat(format) as SideboardPolicy;
}
