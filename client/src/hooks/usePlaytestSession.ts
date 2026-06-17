/**
 * Hook that binds UI components to the active playtest session.
 *
 * Returns the full session plus convenience derived values so components don't
 * have to re-derive them inline. All mutations dispatch through the store's
 * async action creators.
 */

import { usePlaytestStore } from "../stores/playtestStore";
import type { PlaytestCard, PlaytestPermanent, TurnSnapshot } from "../services/playtestSession";

export interface PlaytestSessionHook {
  // ── Status ────────────────────────────────────────────────────────────────
  phase: ReturnType<typeof usePlaytestStore.getState>["phase"];
  isLoading: boolean;
  error: string | null;
  clearError: () => void;

  // ── Session state ─────────────────────────────────────────────────────────
  turnNumber: number;
  hand: PlaytestCard[];
  battlefield: PlaytestPermanent[];
  library: PlaytestCard[];
  graveyard: PlaytestCard[];
  exile: PlaytestCard[];
  history: TurnSnapshot[];
  goingFirst: boolean;

  // ── Mulligan ──────────────────────────────────────────────────────────────
  inMulligan: boolean;
  mulliganCount: number;
  bottomingRequired: number;

  // ── Derived ───────────────────────────────────────────────────────────────
  landPlayedThisTurn: boolean;
  /** Engine-computed untapped mana sources available this turn. */
  availableMana: number;
  /** Engine-provided ids of non-land cards the human can afford to cast. */
  legalCastIds: number[];
  /** Engine-provided ids of lands in hand that are legally playable. */
  legalLandIds: number[];
  /** Cards in hand that are affordable to cast (engine-authoritative). */
  playableInHand: PlaytestCard[];
  /** Number of cards to discard for cleanup step. */
  discardNeeded: number;
  /** True when in cleanup step (hand > 7). */
  needsCleanup: boolean;

  // ── Selection ─────────────────────────────────────────────────────────────
  selectedCardId: number | null;
  contextMenuZone: "hand" | "battlefield" | "graveyard" | "exile" | null;
  selectCard: (id: number | null, zone: PlaytestSessionHook["contextMenuZone"]) => void;
  dismissContextMenu: () => void;

  // ── Actions ───────────────────────────────────────────────────────────────
  keepHand: () => Promise<void>;
  mulligan: () => Promise<void>;
  bottomCard: (id: number) => Promise<void>;
  playLand: (id: number) => Promise<void>;
  castCard: (id: number) => Promise<void>;
  discardCard: (id: number) => Promise<void>;
  tapPermanent: (id: number) => Promise<void>;
  untapPermanent: (id: number) => Promise<void>;
  destroyPermanent: (id: number) => Promise<void>;
  exilePermanent: (id: number) => Promise<void>;
  bouncePermanent: (id: number) => Promise<void>;
  drawOne: () => Promise<void>;
  advanceTurn: () => Promise<void>;
  resetSession: () => Promise<void>;
}

export function usePlaytestSession(): PlaytestSessionHook {
  const {
    phase,
    session,
    error,
    selectedCardId,
    contextMenuZone,
    clearError,
    keepHand,
    mulligan,
    bottomCard,
    playLand,
    castCard,
    discardCard,
    tapPermanent,
    untapPermanent,
    destroyPermanent,
    exilePermanent,
    bouncePermanent,
    drawOne,
    advanceTurn,
    resetSession,
    selectCard,
    dismissContextMenu,
  } = usePlaytestStore();

  const hand = session?.hand ?? [];
  const battlefield = session?.battlefield ?? [];
  const availableMana = session?.availableMana ?? 0;
  const legalCastIds = session?.legalCastIds ?? [];
  const legalLandIds = session?.legalLandIds ?? [];

  // Engine-authoritative: a card is playable iff the engine says so.
  const legalCastSet = new Set(legalCastIds);
  const playableInHand = hand.filter((c) => legalCastSet.has(c.id));

  const discardNeeded = Math.max(0, hand.length - 7);

  return {
    phase,
    isLoading: phase === "loading" || phase === "simulating",
    error,
    clearError,
    turnNumber: session?.turnNumber ?? 0,
    hand,
    battlefield,
    library: session?.library ?? [],
    graveyard: session?.graveyard ?? [],
    exile: session?.exile ?? [],
    history: session?.history ?? [],
    goingFirst: session?.goingFirst ?? true,
    inMulligan: session?.inMulligan ?? false,
    mulliganCount: session?.mulliganCount ?? 0,
    bottomingRequired: session?.bottomingRequired ?? 0,
    landPlayedThisTurn: session?.landPlayedThisTurn ?? false,
    availableMana,
    legalCastIds,
    legalLandIds,
    playableInHand,
    discardNeeded,
    needsCleanup: discardNeeded > 0,
    selectedCardId,
    contextMenuZone,
    selectCard,
    dismissContextMenu,
    keepHand,
    mulligan,
    bottomCard,
    playLand,
    castCard,
    discardCard,
    tapPermanent,
    untapPermanent,
    destroyPermanent,
    exilePermanent,
    bouncePermanent,
    drawOne,
    advanceTurn,
    resetSession,
  };
}

/** Helper: is a specific card ID the one currently selected? */
export function useIsCardSelected(cardId: number): boolean {
  return usePlaytestStore((s) => s.selectedCardId === cardId);
}

/** Helper: is a specific permanent tapped? */
export function useIsTapped(cardId: number): boolean {
  return (
    usePlaytestStore(
      (s) => s.session?.battlefield.find((p) => p.card.id === cardId)?.tapped,
    ) ?? false
  );
}
