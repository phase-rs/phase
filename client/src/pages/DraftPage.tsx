import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { useDraftStore } from "../stores/draftStore";
import { useGameStore } from "../stores/gameStore";
import {
  loadActiveQuickDraft,
  type ActiveQuickDraftMeta,
} from "../services/quickDraftPersistence";
import { CardPreview } from "../components/card/CardPreview";
import { DraftIntro } from "../components/draft/DraftIntro";
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

const SET_LABELS: Record<string, string> = {
  otj: "Outlaws of Thunder Junction",
  mkm: "Murders at Karlov Manor",
  lci: "The Lost Caverns of Ixalan",
  woe: "Wilds of Eldraine",
  mom: "March of the Machine",
  one: "Phyrexia: All Will Be One",
  bro: "The Brothers' War",
  dmu: "Dominaria United",
  snc: "Streets of New Capenna",
  dsk: "Duskmourn",
  blb: "Bloomburrow",
  fdn: "Foundations",
};

// ── Helpers ────────────────────────────────────────────────────────────

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

function formatSetLabel(code: string): string {
  return SET_LABELS[code.toLowerCase()] ?? code.toUpperCase();
}

function formatPhaseLabel(phase: string): string {
  return phase === "deckbuilding" ? "deck building" : "drafting";
}

// ── Component ──────────────────────────────────────────────────────────

type ResumeState = "checking" | "prompt" | "none";

export function DraftPage() {
  const phase = useDraftStore((s) => s.phase);
  const reset = useDraftStore((s) => s.reset);
  const navigate = useNavigate();
  const [hoveredCardName, setHoveredCardName] = useState<string | null>(null);
  const [introDismissed, setIntroDismissed] = useState(false);
  const [resumeState, setResumeState] = useState<ResumeState>("checking");
  const [savedDraftMeta, setSavedDraftMeta] = useState<ActiveQuickDraftMeta | null>(null);
  const [resumeLoading, setResumeLoading] = useState(false);

  useEffect(() => {
    const meta = loadActiveQuickDraft();
    if (meta) {
      setSavedDraftMeta(meta);
      setResumeState("prompt");
    } else {
      setResumeState("none");
    }
  }, []);

  useEffect(() => {
    return () => {
      reset();
    };
  }, [reset]);

  const handleResumeDraft = useCallback(async () => {
    setResumeLoading(true);
    try {
      await useDraftStore.getState().resumeDraft();
      setResumeState("none");
      setIntroDismissed(true);
    } catch {
      await useDraftStore.getState().abandonDraft();
      setResumeState("none");
    } finally {
      setResumeLoading(false);
    }
  }, []);

  const handleDiscardDraft = useCallback(async () => {
    await useDraftStore.getState().abandonDraft();
    setSavedDraftMeta(null);
    setResumeState("none");
  }, []);

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

    const landCards: string[] = [];
    for (const [name, count] of Object.entries(landCounts)) {
      for (let i = 0; i < count; i++) {
        landCards.push(name);
      }
    }
    const fullDeck = [...mainDeck, ...landCards];

    const botSeat = Math.floor(Math.random() * 7) + 1;
    const botDeck = await adapter.getBotDeck(botSeat);
    const botFullDeck = [
      ...botDeck.main_deck,
      ...Object.entries(botDeck.lands).flatMap(([name, count]) =>
        Array<string>(count).fill(name),
      ),
    ];

    const gameId = crypto.randomUUID();
    storeDraftDeckData(gameId, fullDeck, botFullDeck);

    const headDifficulty = DIFFICULTY_NAMES[difficulty] ?? "Medium";
    useGameStore.setState({ gameId });
    navigate(
      `/game/${gameId}?mode=ai&difficulty=${headDifficulty}&format=Limited&match=bo1&source=draft`,
    );
  }, [navigate]);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <ScreenChrome onBack={() => navigate("/")} />
      {phase === "drafting" && introDismissed && <CardPreview cardName={hoveredCardName} />}

      <div className="relative z-10 mx-auto flex w-full max-w-6xl flex-col px-6 py-16">
        {resumeState === "checking" && null}

        {resumeState === "prompt" && savedDraftMeta && (
          <div className="mx-auto w-full max-w-lg">
            <h1 className="mb-8 text-3xl font-bold text-white">Quick Draft</h1>
            <div className="rounded-[16px] border border-white/10 bg-white/[0.03] p-6">
              <div className="mb-1 text-sm font-medium text-white/50">Draft in progress</div>
              <div className="mb-4 text-lg font-semibold text-white">
                {formatSetLabel(savedDraftMeta.setCode)}
                <span className="ml-2 text-sm font-normal text-white/40">
                  — {formatPhaseLabel(savedDraftMeta.phase)}
                </span>
              </div>
              <div className="flex gap-3">
                <button
                  onClick={handleResumeDraft}
                  disabled={resumeLoading}
                  className={menuButtonClass({ tone: "emerald", size: "md" })}
                >
                  {resumeLoading ? "Loading…" : "Resume Draft"}
                </button>
                <button
                  onClick={handleDiscardDraft}
                  disabled={resumeLoading}
                  className={menuButtonClass({ tone: "neutral", size: "md" })}
                >
                  Start New
                </button>
              </div>
            </div>
          </div>
        )}

        {resumeState === "none" && phase === "setup" && (
          <div className="mx-auto w-full max-w-4xl">
            <h1 className="mb-8 text-3xl font-bold text-white">Quick Draft</h1>
            <SetSelector onStartDraft={handleStartDraft} />
          </div>
        )}

        {phase === "drafting" && !introDismissed && (
          <DraftIntro mode="quick" onContinue={() => setIntroDismissed(true)} />
        )}

        {phase === "drafting" && introDismissed && (
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
