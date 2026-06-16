/**
 * Zustand store for the active playtest session and Monte Carlo simulation results.
 *
 * The store is the single source of truth for all playtest UI state. Components
 * read session/simulation data here and dispatch actions through the async
 * action creators below, which delegate to the WASM bindings via
 * `playtestSession.ts`.
 */

import { create } from "zustand";
import type { PlaytestSession, SimulationResult, SimulationConfig } from "../services/playtestSession";
import * as pt from "../services/playtestSession";

// ── State shape ───────────────────────────────────────────────────────────────

export type PlaytestPhase =
  | "idle"          // No session active
  | "loading"       // Starting / resetting session
  | "mulligan"      // Choosing to keep or mulligan
  | "bottoming"     // Placing mulligan cards on bottom
  | "playing"       // Active turn — main phase
  | "cleanup"       // Must discard to hand size 7
  | "simulating"    // Running Monte Carlo
  | "error";        // Engine error

interface PlaytestStoreState {
  // ── Session ────────────────────────────────────────────────────────────────
  phase: PlaytestPhase;
  session: PlaytestSession | null;
  error: string | null;
  /** Card names currently loaded into the session (for reset). */
  deckNames: string[];
  /** Whether the active session was started going-first. */
  goingFirst: boolean;
  /** Current RNG seed (to allow displaying / re-seeding). */
  seed: number;

  // ── Pending interaction ────────────────────────────────────────────────────
  /** Card the user has selected (right-click context or long-press) for action. */
  selectedCardId: number | null;
  /** Which context menu is open: hand | battlefield | graveyard | exile | null */
  contextMenuZone: "hand" | "battlefield" | "graveyard" | "exile" | null;

  // ── Simulation ────────────────────────────────────────────────────────────
  simulationResult: SimulationResult | null;
  simulationConfig: SimulationConfig;

  // ── Actions ───────────────────────────────────────────────────────────────
  startSession: (cardNames: string[], seed?: number, goingFirst?: boolean) => Promise<void>;
  keepHand: () => Promise<void>;
  mulligan: () => Promise<void>;
  bottomCard: (cardId: number) => Promise<void>;
  drawOne: () => Promise<void>;
  drawN: (n: number) => Promise<void>;
  playLand: (cardId: number) => Promise<void>;
  castCard: (cardId: number) => Promise<void>;
  discardCard: (cardId: number) => Promise<void>;
  tapPermanent: (cardId: number) => Promise<void>;
  untapPermanent: (cardId: number) => Promise<void>;
  destroyPermanent: (cardId: number) => Promise<void>;
  exilePermanent: (cardId: number) => Promise<void>;
  bouncePermanent: (cardId: number) => Promise<void>;
  advanceTurn: () => Promise<void>;
  resetSession: () => Promise<void>;
  resetWithSeed: (seed: number) => Promise<void>;
  endSession: () => Promise<void>;
  runSimulation: (config?: Partial<SimulationConfig>) => Promise<void>;
  setSimulationConfig: (config: Partial<SimulationConfig>) => void;
  selectCard: (cardId: number | null, zone: PlaytestStoreState["contextMenuZone"]) => void;
  dismissContextMenu: () => void;
  clearError: () => void;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function derivePhase(session: PlaytestSession): PlaytestPhase {
  if (session.inMulligan) {
    return session.bottomingRequired > 0 ? "bottoming" : "mulligan";
  }
  if (session.hand.length > 7) return "cleanup";
  return "playing";
}

function applySession(
  session: PlaytestSession,
): Pick<PlaytestStoreState, "session" | "phase" | "error"> {
  return { session, phase: derivePhase(session), error: null };
}

// ── Store ─────────────────────────────────────────────────────────────────────

export const usePlaytestStore = create<PlaytestStoreState>((set, get) => ({
  // State
  phase: "idle",
  session: null,
  error: null,
  deckNames: [],
  goingFirst: true,
  seed: 0xdeadbeef,
  selectedCardId: null,
  contextMenuZone: null,
  simulationResult: null,
  simulationConfig: {
    numGames: 200,
    numTurns: 10,
    baseSeed: 0xdeadbeef,
    goingFirst: true,
    autoKeep: false,
  },

  // ── Session lifecycle ────────────────────────────────────────────────────

  startSession: async (cardNames, seed = 0xdeadbeef, goingFirst = true) => {
    set({ phase: "loading", error: null, deckNames: cardNames, goingFirst, seed });
    try {
      const session = await pt.startPlaytest(cardNames, seed, goingFirst);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  keepHand: async () => {
    try {
      const session = await pt.keepHand();
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  mulligan: async () => {
    try {
      const session = await pt.takePlaytestMulligan();
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  bottomCard: async (cardId) => {
    try {
      const session = await pt.bottomCard(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  drawOne: async () => {
    try {
      const session = await pt.drawOne();
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  drawN: async (n) => {
    try {
      const session = await pt.drawN(n);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  playLand: async (cardId) => {
    set({ selectedCardId: null, contextMenuZone: null });
    try {
      const session = await pt.playLand(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  castCard: async (cardId) => {
    set({ selectedCardId: null, contextMenuZone: null });
    try {
      const session = await pt.castCard(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  discardCard: async (cardId) => {
    set({ selectedCardId: null, contextMenuZone: null });
    try {
      const session = await pt.discardCard(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  tapPermanent: async (cardId) => {
    set({ selectedCardId: null, contextMenuZone: null });
    try {
      const session = await pt.tapPermanent(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  untapPermanent: async (cardId) => {
    set({ selectedCardId: null, contextMenuZone: null });
    try {
      const session = await pt.untapPermanent(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  destroyPermanent: async (cardId) => {
    set({ selectedCardId: null, contextMenuZone: null });
    try {
      const session = await pt.destroyPermanent(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  exilePermanent: async (cardId) => {
    set({ selectedCardId: null, contextMenuZone: null });
    try {
      const session = await pt.exilePermanent(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  bouncePermanent: async (cardId) => {
    set({ selectedCardId: null, contextMenuZone: null });
    try {
      const session = await pt.bouncePermanent(cardId);
      set({ ...applySession(session) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  advanceTurn: async () => {
    const { session } = get();
    if (session && session.hand.length > 7) {
      set({ error: `Must discard ${session.hand.length - 7} card(s) first (cleanup step)` });
      return;
    }
    try {
      const updated = await pt.advanceTurn();
      set({ ...applySession(updated) });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  resetSession: async () => {
    const { deckNames, seed, goingFirst } = get();
    set({ phase: "loading", error: null });
    try {
      const session = await pt.resetPlaytest();
      set({ ...applySession(session), seed, goingFirst, deckNames });
    } catch {
      // If the WASM session was cleared, restart from names.
      try {
        const session = await pt.startPlaytest(deckNames, seed, goingFirst);
        set({ ...applySession(session) });
      } catch (e2) {
        set({ phase: "error", error: String(e2) });
      }
    }
  },

  resetWithSeed: async (seed) => {
    const { deckNames, goingFirst } = get();
    set({ phase: "loading", error: null, seed });
    try {
      const session = await pt.resetWithSeed(seed);
      set({ ...applySession(session), seed, deckNames, goingFirst });
    } catch {
      try {
        const session = await pt.startPlaytest(deckNames, seed, goingFirst);
        set({ ...applySession(session) });
      } catch (e2) {
        set({ phase: "error", error: String(e2) });
      }
    }
  },

  endSession: async () => {
    try {
      await pt.clearPlaytest();
    } catch {
      // Ignore — state already gone.
    }
    set({
      phase: "idle",
      session: null,
      error: null,
      deckNames: [],
      selectedCardId: null,
      contextMenuZone: null,
    });
  },

  // ── Simulation ─────────────────────────────────────────────────────────────

  runSimulation: async (configOverrides) => {
    const { deckNames, simulationConfig } = get();
    if (deckNames.length === 0) {
      set({ error: "No deck loaded — start a session first" });
      return;
    }
    const config: SimulationConfig = { ...simulationConfig, ...configOverrides };
    set({ phase: "simulating", error: null, simulationConfig: config });
    try {
      const result = await pt.runSimulation(deckNames, config);
      set({ simulationResult: result, phase: get().session ? derivePhase(get().session!) : "idle" });
    } catch (e) {
      set({ phase: "error", error: String(e) });
    }
  },

  setSimulationConfig: (config) => {
    set((s) => ({ simulationConfig: { ...s.simulationConfig, ...config } }));
  },

  // ── UI helpers ─────────────────────────────────────────────────────────────

  selectCard: (cardId, zone) => {
    set({ selectedCardId: cardId, contextMenuZone: zone });
  },

  dismissContextMenu: () => {
    set({ selectedCardId: null, contextMenuZone: null });
  },

  clearError: () => set({ error: null }),
}));
