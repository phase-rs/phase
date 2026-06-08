import { useCallback, useState } from "react";
import { useNavigate } from "react-router";

import type { DraftCardInstance, DraftPlayerView } from "../adapter/draft-adapter.ts";
import { DraftAdapter } from "../adapter/draft-adapter.ts";
import { LimitedDeckBuilder } from "../components/draft/LimitedDeckBuilder.tsx";
import { SetSelector } from "../components/draft/SetSelector.tsx";
import { useDraftStore } from "../stores/draftStore.ts";
import { useGameStore } from "../stores/gameStore.ts";

type SealedPhase = "setup" | "generating" | "deckbuilding" | "launching";

// A sealed pool is 6 packs × 15 cards ≈ 90 cards.
// One quick draft session gives 3 packs × 15 = 45 cards, so we run two back-to-back
// and combine their pools.
const SEALED_SESSIONS = 2;

async function buildSealedPool(
  setPoolJson: string,
  onProgress: (picked: number, total: number) => void,
): Promise<DraftPlayerView> {
  let combinedPool: DraftCardInstance[] = [];
  let lastView: DraftPlayerView | null = null;

  for (let session = 0; session < SEALED_SESSIONS; session++) {
    const adapter = new DraftAdapter();
    const seed = Math.floor(Math.random() * 0xffffffff);
    let view = await adapter.initialize(setPoolJson, /* difficulty= */ 0, seed);

    // Auto-pick every card until all packs in this session are exhausted.
    while (view.status === "Drafting" && view.current_pack !== null) {
      const pickedSoFar = session * 45 + view.pool.length;
      onProgress(pickedSoFar, SEALED_SESSIONS * 45);
      view = await adapter.autoPick();
    }

    combinedPool = [...combinedPool, ...view.pool];
    lastView = view;
  }

  if (!lastView) throw new Error("No draft sessions completed");

  return {
    ...lastView,
    status: "Deckbuilding",
    current_pack: null,
    pool: combinedPool,
  };
}

function ProgressBar({ picked, total }: { picked: number; total: number }) {
  const pct = total > 0 ? Math.min(100, Math.round((picked / total) * 100)) : 0;
  return (
    <div className="flex flex-col items-center gap-3">
      <p className="text-[#aaa] text-sm">Opening packs…</p>
      <div className="w-64 h-2 bg-[#333] rounded-full overflow-hidden">
        <div
          className="h-full bg-[#8b5cf6] transition-all duration-200 rounded-full"
          style={{ width: `${pct}%` }}
        />
      </div>
      <p className="text-xs text-[#666]">{picked} / {total} cards</p>
    </div>
  );
}

export function SealedPage() {
  const navigate = useNavigate();
  const [phase, setPhase] = useState<SealedPhase>("setup");
  const [sealedView, setSealedView] = useState<DraftPlayerView | null>(null);
  const [progress, setProgress] = useState({ picked: 0, total: SEALED_SESSIONS * 45 });
  const [error, setError] = useState<string | null>(null);

  const handleStartSealed = useCallback(
    async (setCode: string, setName: string) => {
      setPhase("generating");
      setError(null);

      let setPoolJson: string;
      try {
        const resp = await fetch(__DRAFT_POOLS_URL__);
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        const allPools: Record<string, unknown> = await resp.json();
        const setPool =
          allPools[setCode.toLowerCase()] ?? allPools[setCode.toUpperCase()];
        if (!setPool) throw new Error(`No pool data for ${setCode}`);
        setPoolJson = JSON.stringify(setPool);
      } catch (err) {
        setError(`Failed to load set pool: ${String(err)}`);
        setPhase("setup");
        return;
      }

      try {
        const finalView = await buildSealedPool(setPoolJson, (picked, total) => {
          setProgress({ picked, total });
        });

        // Seed the draft store so LimitedDeckBuilder can manage mainDeck/landCounts
        // using its normal store-connected flow.
        useDraftStore.setState({
          view: finalView,
          phase: "deckbuilding",
          mainDeck: [],
          landCounts: {},
          selectedSet: setCode,
          selectedSetName: setName,
          draftId: crypto.randomUUID(),
        });

        setSealedView(finalView);
        setPhase("deckbuilding");
      } catch (err) {
        setError(`Failed to generate sealed pool: ${String(err)}`);
        setPhase("setup");
      }
    },
    [],
  );

  const handleSubmitDeck = useCallback(async () => {
    setPhase("launching");
    try {
      const { mainDeck, landCounts } = useDraftStore.getState();

      const landCards = Object.entries(landCounts).flatMap(([name, count]) =>
        Array<string>(count as number).fill(name),
      );
      const fullDeck = [...mainDeck, ...landCards];

      if (fullDeck.length < 40) {
        alert(`Your deck needs at least 40 cards (currently ${fullDeck.length}).`);
        setPhase("deckbuilding");
        return;
      }

      const gameId = crypto.randomUUID();

      // Store the sealed deck under a well-known key for the game adapter.
      sessionStorage.setItem(
        `sealed-deck:${gameId}`,
        JSON.stringify(fullDeck),
      );

      useGameStore.setState({ gameId });
      navigate(
        `/game/${gameId}?mode=ai&difficulty=Medium&format=Limited&match=bo1&source=sealed`,
      );
    } catch (err) {
      setError(`Failed to launch game: ${String(err)}`);
      setPhase("deckbuilding");
    }
  }, [navigate]);

  return (
    <div className="min-h-screen bg-[#111] text-white flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-[#333] bg-[#161616]">
        <button
          onClick={() => navigate(-1)}
          className="text-sm text-[#888] hover:text-white transition-colors"
        >
          ← Back
        </button>
        <h1 className="text-base font-semibold text-[#ddd]">Sealed Deck</h1>
        <div className="w-16" />
      </div>

      <div className="flex-1 flex flex-col">
        {phase === "setup" && (
          <div className="flex-1 flex flex-col items-center justify-center p-4 gap-4">
            <div className="text-center mb-4">
              <h2 className="text-xl font-bold text-white mb-1">Build a Sealed Pool</h2>
              <p className="text-sm text-[#888]">
                Open {SEALED_SESSIONS * 3} packs (~{SEALED_SESSIONS * 45} cards) and build a 40-card deck.
              </p>
            </div>
            {error && (
              <p className="text-red-400 text-sm text-center max-w-sm">{error}</p>
            )}
            <SetSelector onStartDraft={handleStartSealed} />
          </div>
        )}

        {phase === "generating" && (
          <div className="flex-1 flex items-center justify-center">
            <ProgressBar picked={progress.picked} total={progress.total} />
          </div>
        )}

        {phase === "deckbuilding" && sealedView && (
          <div className="flex-1 flex flex-col">
            <div className="px-4 py-2 bg-[#1a1a1a] border-b border-[#333] text-xs text-[#888]">
              Sealed Pool · {sealedView.pool.length} cards · Build a 40-card deck
            </div>
            <LimitedDeckBuilder
              view={sealedView}
              onSubmitDeck={handleSubmitDeck}
            />
            {error && (
              <p className="p-3 text-red-400 text-xs">{error}</p>
            )}
          </div>
        )}

        {phase === "launching" && (
          <div className="flex-1 flex items-center justify-center">
            <p className="text-[#aaa]">Launching game…</p>
          </div>
        )}
      </div>
    </div>
  );
}
