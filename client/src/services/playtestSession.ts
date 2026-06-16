/**
 * Thin TypeScript wrappers around the WASM playtest bindings.
 *
 * All functions call `ensureCardDatabase()` before invoking engine code so
 * callers never need to worry about load order. The engine module is loaded
 * lazily via the main-thread WASM (not the worker) because playtest sessions
 * are short-lived solitaire interactions that don't benefit from worker
 * isolation.
 */

import { ensureCardDatabase, ensureWasmInit } from "./engineRuntime";

// Cast to `any` so the service is not coupled to the auto-generated WASM d.ts.
// The WASM is rebuilt by Tilt; the d.ts lags until a new WASM build lands.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type WasmModule = any;

let enginePromise: Promise<WasmModule> | null = null;

async function engine(): Promise<WasmModule> {
  if (!enginePromise) {
    enginePromise = (async () => {
      await ensureWasmInit();
      return import("@wasm/engine");
    })();
  }
  return enginePromise;
}

// ── Types ─────────────────────────────────────────────────────────────────────

export interface PlaytestCardFace {
  name: string;
  oracleText: string;
  manaCost: string | null;
  manaValue: number | null;
  cardType: {
    coreTypes: string[];
    subtypes: string[];
    supertypes: string[];
  };
  power: string | null;
  toughness: string | null;
  loyalty: string | null;
  colors: string[];
  colorIdentity: string[];
}

export interface PlaytestCard {
  id: number;
  face: PlaytestCardFace;
}

export interface PlaytestPermanent {
  card: PlaytestCard;
  tapped: boolean;
  enteredThisTurn: boolean;
}

export interface TurnSnapshot {
  turnNumber: number;
  handSize: number;
  landsInPlay: number;
  manaSourcesInPlay: number;
  availableMana: number;
  cardsDrawn: number;
  landsInHand: number;
  playableCount: number;
}

export interface PlaytestSession {
  library: PlaytestCard[];
  hand: PlaytestCard[];
  battlefield: PlaytestPermanent[];
  graveyard: PlaytestCard[];
  exile: PlaytestCard[];
  turnNumber: number;
  landPlayedThisTurn: boolean;
  drewThisTurn: boolean;
  inMulligan: boolean;
  mulliganCount: number;
  bottomingRequired: number;
  history: TurnSnapshot[];
  goingFirst: boolean;
}

export interface TurnAggregate {
  turnNumber: number;
  avgHandSize: number;
  avgLandsInPlay: number;
  avgManaSources: number;
  avgAvailableMana: number;
  avgPlayableCount: number;
  pctLandInHand: number;
  pctEmptyLibrary: number;
  minAvailableMana: number;
  maxAvailableMana: number;
  stddevAvailableMana: number;
}

export interface OpeningHandStats {
  avgLands: number;
  avgSpells: number;
  avgHandSize: number;
  handSizeDistribution: number[];
  avgMulligans: number;
  pctKeepFirst: number;
}

export interface SimulationConfig {
  numGames: number;
  numTurns: number;
  baseSeed: number;
  goingFirst: boolean;
  autoKeep: boolean;
}

export interface SimulationResult {
  turns: TurnAggregate[];
  openingHand: OpeningHandStats;
  gamesSimulated: number;
  config: SimulationConfig;
}

// ── Session lifecycle ─────────────────────────────────────────────────────────

/**
 * Start a new playtest session from a list of card names.
 * The card database must be loaded (ensured automatically).
 */
export async function startPlaytest(
  cardNames: string[],
  seed?: number,
  goingFirst = true,
): Promise<PlaytestSession> {
  await ensureCardDatabase();
  const eng = await engine();
  const result = eng.playtest_start(cardNames, seed, goingFirst);
  return result as PlaytestSession;
}

/** Get the current playtest session state. */
export async function getPlaytestState(): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_get_state() as PlaytestSession;
}

/** Keep the current opening hand and transition out of mulligan. */
export async function keepHand(): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_keep_hand() as PlaytestSession;
}

/** Take a London mulligan. */
export async function takePlaytestMulligan(): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_mulligan() as PlaytestSession;
}

/** Place a card on the bottom of the library (mulligan bottoming step). */
export async function bottomCard(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_bottom_card(cardId) as PlaytestSession;
}

/** Draw one card from the library. */
export async function drawOne(): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_draw() as PlaytestSession;
}

/** Draw N cards from the library. */
export async function drawN(n: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_draw_n(n) as PlaytestSession;
}

/** Play a land from hand to the battlefield. */
export async function playLand(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_play_land(cardId) as PlaytestSession;
}

/** Move a card from hand to the battlefield (permanents) or graveyard (spells). */
export async function castCard(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_cast(cardId) as PlaytestSession;
}

/** Discard a card from hand to the graveyard. */
export async function discardCard(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_discard(cardId) as PlaytestSession;
}

/** Tap a permanent on the battlefield. */
export async function tapPermanent(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_tap(cardId) as PlaytestSession;
}

/** Untap a permanent on the battlefield. */
export async function untapPermanent(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_untap(cardId) as PlaytestSession;
}

/** Move a permanent from battlefield to graveyard. */
export async function destroyPermanent(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_destroy(cardId) as PlaytestSession;
}

/** Move a permanent from battlefield to exile. */
export async function exilePermanent(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_exile(cardId) as PlaytestSession;
}

/** Bounce a permanent from battlefield back to hand. */
export async function bouncePermanent(cardId: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_bounce(cardId) as PlaytestSession;
}

/** Advance to the next turn (untap + draw). */
export async function advanceTurn(): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_advance_turn() as PlaytestSession;
}

/** Reset the session to a fresh game with the same deck and seed. */
export async function resetPlaytest(): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_reset() as PlaytestSession;
}

/** Reset with a new RNG seed. */
export async function resetWithSeed(seed: number): Promise<PlaytestSession> {
  const eng = await engine();
  return eng.playtest_reset_with_seed(seed) as PlaytestSession;
}

/** Clear the active session and free memory. */
export async function clearPlaytest(): Promise<void> {
  const eng = await engine();
  eng.playtest_clear();
}

// ── Monte Carlo simulation ────────────────────────────────────────────────────

/** Run a Monte Carlo hand-quality simulation over a deck. */
export async function runSimulation(
  cardNames: string[],
  config?: Partial<SimulationConfig>,
): Promise<SimulationResult> {
  await ensureCardDatabase();
  const eng = await engine();
  const fullConfig: SimulationConfig = {
    numGames: 200,
    numTurns: 10,
    baseSeed: 0xdeadbeef,
    goingFirst: true,
    autoKeep: false,
    ...config,
  };
  return eng.playtest_run_simulation(cardNames, fullConfig) as SimulationResult;
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/** True for card types that go to the battlefield when "cast" in playtest mode. */
export function isPermanentType(coreTypes: string[]): boolean {
  return coreTypes.some((t) =>
    ["Creature", "Artifact", "Enchantment", "Land", "Planeswalker", "Battle"].includes(t),
  );
}

/** True for lands (determines whether play_land or cast is called). */
export function isLandType(coreTypes: string[]): boolean {
  return coreTypes.includes("Land");
}

/** Derive a CSS color class for a card's mana cost string for display. */
export function cardColorClass(card: PlaytestCard): string {
  const colors = card.face.colors ?? [];
  if (colors.length === 0) return "text-slate-300";
  if (colors.length > 1) return "text-yellow-300";
  switch (colors[0]) {
    case "W": return "text-amber-100";
    case "U": return "text-blue-300";
    case "B": return "text-purple-300";
    case "R": return "text-red-400";
    case "G": return "text-green-400";
    default:  return "text-slate-300";
  }
}
