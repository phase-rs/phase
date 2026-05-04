import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { useDraftStore } from "../stores/draftStore";
import { useGameStore } from "../stores/gameStore";
import { CardPreview } from "../components/card/CardPreview";
import { SetSelector } from "../components/draft/SetSelector";
import { PackDisplay } from "../components/draft/PackDisplay";
import { PoolPanel } from "../components/draft/PoolPanel";
import { DraftProgress } from "../components/draft/DraftProgress";
import { LimitedDeckBuilder } from "../components/draft/LimitedDeckBuilder";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { menuButtonClass } from "../components/menu/buttonStyles";

// ── Constants ──────────────────────────────────────────────────────────

const DIFFICULTY_NAMES = ["VeryEasy", "Easy", "Medium", "Hard", "VeryHard"] as const;

const DRAFT_DECK_SESSION_KEY = "phase:draft-deck";

// ── Helpers ────────────────────────────────────────────────────────────

/**
 * Store the pre-built deck data in sessionStorage so GameProvider can
 * pick it up when the game page loads. Draft decks bypass the normal
 * localStorage active-deck flow because both the human's deck and the
 * bot opponent's deck come from the draft adapter, not from saved decks.
 */
function storeDraftDeckData(
  gameId: string,
  playerDeck: string[],
  opponentDeck: string[],
): void {
  const data = {
    player: { main_deck: playerDeck, sideboard: [], commander: [] },
    opponent: { main_deck: opponentDeck, sideboard: [], commander: [] },
    ai_decks: [],
  };
  sessionStorage.setItem(
    `${DRAFT_DECK_SESSION_KEY}:${gameId}`,
    JSON.stringify(data),
  );
}

// ── Component ──────────────────────────────────────────────────────────

export function DraftPage() {
  const phase = useDraftStore((s) => s.phase);
  const reset = useDraftStore((s) => s.reset);
  const navigate = useNavigate();
  const [hoveredCardName, setHoveredCardName] = useState<string | null>(null);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      reset();
    };
  }, [reset]);

  const handleStartDraft = useCallback(
    async (setCode: string) => {
      const { difficulty, startDraft } = useDraftStore.getState();

      const resp = await fetch(__DRAFT_POOLS_URL__);
      if (!resp.ok) throw new Error(`Failed to load draft pools: ${resp.status}`);
      const allPools: Record<string, unknown> = await resp.json();
      const setPool = allPools[setCode.toLowerCase()] ?? allPools[setCode.toUpperCase()];
      if (!setPool) throw new Error(`No pool data for set: ${setCode}`);

      await startDraft(JSON.stringify(setPool), setCode, difficulty);
    },
    [],
  );

  const handleLaunchMatch = useCallback(async () => {
    const { mainDeck, landCounts, adapter, difficulty } = useDraftStore.getState();
    if (!adapter) return;

    // Build full deck: spells + expanded basic lands
    const landCards: string[] = [];
    for (const [name, count] of Object.entries(landCounts)) {
      for (let i = 0; i < count; i++) {
        landCards.push(name);
      }
    }
    const fullDeck = [...mainDeck, ...landCards];

    // Get a random bot's deck (seats 1-7)
    const botSeat = Math.floor(Math.random() * 7) + 1;
    const botDeck = await adapter.getBotDeck(botSeat);
    const botFullDeck = [
      ...botDeck.main_deck,
      ...Object.entries(botDeck.lands).flatMap(([name, count]) =>
        Array<string>(count).fill(name),
      ),
    ];

    // Generate game ID and store deck data for GameProvider
    const gameId = crypto.randomUUID();
    storeDraftDeckData(gameId, fullDeck, botFullDeck);

    // Per D-09: engine-wasm loads independently when GamePage mounts
    const headDifficulty = DIFFICULTY_NAMES[difficulty] ?? "Medium";
    useGameStore.setState({ gameId });
    navigate(
      `/game/${gameId}?mode=ai&difficulty=${headDifficulty}&format=Limited&match=bo1&source=draft`,
    );
  }, [navigate]);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <ScreenChrome onBack={phase === "setup" ? () => navigate("/") : undefined} />
      {phase === "drafting" && <CardPreview cardName={hoveredCardName} />}

      <div className="relative z-10 mx-auto flex w-full max-w-6xl flex-col px-6 py-16">
        {phase === "setup" && (
          <div className="mx-auto w-full max-w-4xl">
            <h1 className="mb-8 text-3xl font-bold text-white">Quick Draft</h1>
            <SetSelector onStartDraft={handleStartDraft} />
          </div>
        )}

        {phase === "drafting" && (
          <div className="flex gap-4">
            <div className="flex-1">
              <DraftProgress />
              <PackDisplay onCardHover={setHoveredCardName} />
            </div>
            <PoolPanel onCardHover={setHoveredCardName} />
          </div>
        )}

        {phase === "deckbuilding" && (
          <LimitedDeckBuilder />
        )}

        {phase === "launching" && (
          <div className="flex flex-col items-center justify-center gap-6 py-24">
            <div className="text-xl font-medium text-white">
              Your deck is ready!
            </div>
            <button
              onClick={handleLaunchMatch}
              className={menuButtonClass({ tone: "emerald", size: "lg" })}
            >
              Start Match
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
