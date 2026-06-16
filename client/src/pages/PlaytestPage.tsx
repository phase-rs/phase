/**
 * Deck Playtester page.
 *
 * Launched from the Deck Builder via `?deck=<name>` query parameters, or
 * directly from the /playtest route with a manually specified deck.
 *
 * Layout (lg+):
 *   ┌──────────────────────────────────────────┬──────────────────┐
 *   │  Header bar (phase + controls)           │                  │
 *   ├──────────────────────────────────────────┤   Sidebar        │
 *   │  Battlefield                             │   (stats, hist)  │
 *   ├──────────────────────────────────────────┤                  │
 *   │  Hand                                    │                  │
 *   └──────────────────────────────────────────┴──────────────────┘
 *
 * On mobile the sidebar collapses to a bottom sheet toggled by a button.
 */

import { useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { useTranslation } from "react-i18next";

import { usePlaytestStore } from "../stores/playtestStore";
import { usePlaytestSession } from "../hooks/usePlaytestSession";

import { PlaytestPhaseIndicator } from "../components/playtest/PlaytestPhaseIndicator";
import { PlaytestControls }        from "../components/playtest/PlaytestControls";
import { PlaytestHand }            from "../components/playtest/PlaytestHand";
import { PlaytestBattlefield }     from "../components/playtest/PlaytestBattlefield";
import { PlaytestSidebar }         from "../components/playtest/PlaytestSidebar";
import { SimulationPanel }         from "../components/playtest/SimulationPanel";

import { listSavedDeckNames } from "../constants/storage";
import { loadDeck } from "../components/menu/deckHelpers";

// ── Idle / setup screen ────────────────────────────────────────────────────────

function DeckPickerScreen({
  onStart,
}: {
  onStart: (names: string[], going_first: boolean) => void;
}) {
  const { t } = useTranslation("playtest");
  const savedDecks = listSavedDeckNames();
  const [selected, setSelected] = useState<string>(savedDecks[0] ?? "");
  const [goingFirst, setGoingFirst] = useState(true);
  const [loading, setLoading] = useState(false);

  async function handleStart() {
    if (!selected) return;
    setLoading(true);
    try {
      const deck = loadDeck(selected);
      if (!deck) { setLoading(false); return; }
      const names: string[] = deck.main.flatMap((e) =>
        Array(e.count).fill(e.name),
      );
      onStart(names, goingFirst);
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="flex min-h-screen flex-col items-center justify-center gap-6 p-6">
      <div className="w-full max-w-sm space-y-5 rounded-2xl border border-white/10 bg-gray-900 p-6">
        <h1 className="text-lg font-bold text-white">{t("setup.title")}</h1>
        <p className="text-sm text-slate-400">{t("setup.subtitle")}</p>

        {/* Deck selector */}
        <div className="space-y-1">
          <label className="text-xs text-slate-500">{t("setup.chooseDeck")}</label>
          {savedDecks.length === 0 ? (
            <p className="text-sm text-red-400">{t("setup.noDecks")}</p>
          ) : (
            <select
              value={selected}
              onChange={(e) => setSelected(e.target.value)}
              className="w-full rounded-xl border border-white/10 bg-black/20 px-3 py-2 text-sm text-white focus:border-blue-400 focus:outline-none"
            >
              {savedDecks.map((n) => (
                <option key={n} value={n}>{n}</option>
              ))}
            </select>
          )}
        </div>

        {/* Going first toggle */}
        <label className="flex items-center gap-3 text-sm text-slate-300">
          <input
            type="checkbox"
            checked={goingFirst}
            onChange={(e) => setGoingFirst(e.target.checked)}
            className="h-4 w-4 accent-blue-500"
          />
          {t("setup.goingFirst")}
        </label>

        <button
          type="button"
          onClick={handleStart}
          disabled={!selected || loading}
          className="w-full rounded-xl bg-blue-600 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-blue-500 disabled:opacity-50"
        >
          {loading ? t("setup.loading") : t("setup.start")}
        </button>
      </div>
    </div>
  );
}

// ── Error banner ──────────────────────────────────────────────────────────────

function ErrorBanner({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return (
    <div className="flex items-center gap-3 rounded-xl border border-red-500/30 bg-red-900/20 px-3 py-2 text-sm">
      <span className="min-w-0 flex-1 text-red-300">{message}</span>
      <button
        type="button"
        onClick={onDismiss}
        className="shrink-0 text-red-400 hover:text-red-200"
      >
        ✕
      </button>
    </div>
  );
}

// ── Main page ─────────────────────────────────────────────────────────────────

export function PlaytestPage() {
  const { t } = useTranslation("playtest");
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();

  const {
    phase,
    error,
    seed,
    goingFirst,
    startSession,
    resetWithSeed: storeResetWithSeed,
    endSession,
  } = usePlaytestStore();

  const pt = usePlaytestSession();
  const [showSimPanel, setShowSimPanel] = useState(false);
  const [showSidebar, setShowSidebar] = useState(true);
  const initialized = useRef(false);

  // Auto-start from ?deck= query param (launched from DeckBuilder).
  useEffect(() => {
    if (initialized.current) return;
    initialized.current = true;
    const deckName = searchParams.get("deck");
    if (!deckName) return;
    const deck = loadDeck(deckName);
    if (!deck) return;
    const names: string[] = deck.main.flatMap((e) => Array(e.count).fill(e.name));
    const first = searchParams.get("goingFirst") !== "false";
    void startSession(names, undefined, first);
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  function handleStart(names: string[], gf: boolean) {
    void startSession(names, undefined, gf);
  }

  function handleEndSession() {
    void endSession();
    navigate("/deck-builder");
  }

  function handleNewSeed(newSeed: number) {
    void storeResetWithSeed(newSeed);
  }

  // Idle — show deck picker.
  if (phase === "idle") {
    return <DeckPickerScreen onStart={handleStart} />;
  }

  // Loading spinner.
  if (phase === "loading") {
    return (
      <div className="flex min-h-screen items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-slate-600 border-t-blue-400" />
      </div>
    );
  }

  const hand = pt.hand;
  const bf = pt.battlefield;
  const gy = pt.graveyard;
  const ex = pt.exile;
  const lib = pt.library;

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-gray-950 text-white">
      {/* ── Header bar ──────────────────────────────────────────────────── */}
      <div className="shrink-0 space-y-2 border-b border-white/8 px-3 py-2">
        {error && <ErrorBanner message={error} onDismiss={pt.clearError} />}

        <div className="flex flex-wrap items-center gap-2">
          {/* Back button */}
          <button
            type="button"
            onClick={handleEndSession}
            className="shrink-0 text-sm text-slate-400 hover:text-white"
          >
            ← {t("header.back")}
          </button>

          <div className="min-w-0 flex-1">
            <PlaytestPhaseIndicator
              phase={phase}
              turnNumber={pt.turnNumber}
              goingFirst={goingFirst}
            />
          </div>

          {/* Sidebar toggle (mobile) */}
          <button
            type="button"
            onClick={() => setShowSidebar((v) => !v)}
            className="shrink-0 rounded-lg border border-white/10 bg-white/8 px-2 py-1.5 text-xs text-slate-300 lg:hidden"
          >
            {showSidebar ? t("header.hideSidebar") : t("header.showSidebar")}
          </button>
        </div>

        <PlaytestControls
          phase={phase}
          landPlayedThisTurn={pt.landPlayedThisTurn}
          needsCleanup={pt.needsCleanup}
          discardNeeded={pt.discardNeeded}
          librarySize={lib.length}
          onKeepHand={pt.keepHand}
          onMulligan={pt.mulligan}
          onDraw={pt.drawOne}
          onAdvanceTurn={pt.advanceTurn}
          onReset={pt.resetSession}
          onEndSession={handleEndSession}
          onOpenSimulation={() => setShowSimPanel(true)}
          isLoading={pt.isLoading}
        />
      </div>

      {/* ── Body ─────────────────────────────────────────────────────────── */}
      <div className="flex min-h-0 flex-1 overflow-hidden">
        {/* Main column */}
        <div className="flex min-w-0 flex-1 flex-col gap-3 overflow-y-auto p-3">
          {/* Battlefield */}
          <PlaytestBattlefield
            battlefield={bf}
            graveyard={gy}
            exile={ex}
            onTap={pt.tapPermanent}
            onUntap={pt.untapPermanent}
            onDestroy={pt.destroyPermanent}
            onExile={pt.exilePermanent}
            onBounce={pt.bouncePermanent}
          />

          {/* Hand */}
          <div>
            <div className="mb-1 flex items-center justify-between">
              <span className="text-[0.65rem] font-semibold uppercase tracking-wider text-slate-500">
                {t("hand.title", { count: hand.length })}
              </span>
              {pt.needsCleanup && (
                <span className="text-xs text-red-400">
                  {t("hand.discardNeeded", { count: pt.discardNeeded })}
                </span>
              )}
            </div>
            <PlaytestHand
              hand={hand}
              phase={phase}
              canPlayLand={!pt.landPlayedThisTurn}
              availableMana={pt.availableMana}
              selectedCardId={pt.selectedCardId}
              bottomingRequired={pt.bottomingRequired}
              onPlayLand={pt.playLand}
              onCast={pt.castCard}
              onDiscard={pt.discardCard}
              onBottom={pt.bottomCard}
              onSelect={(id) => pt.selectCard(id, "hand")}
            />
          </div>
        </div>

        {/* Sidebar */}
        {(showSidebar || window.innerWidth >= 1024) && (
          <div className="w-72 shrink-0 border-l border-white/8 bg-black/10">
            {showSimPanel ? (
              <SimulationPanel onClose={() => setShowSimPanel(false)} />
            ) : (
              <PlaytestSidebar
                hand={hand}
                battlefield={bf}
                graveyard={gy}
                exile={ex}
                library={lib}
                history={pt.history}
                turnNumber={pt.turnNumber}
                availableMana={pt.availableMana}
                mulliganCount={pt.mulliganCount}
                seed={seed}
                onNewSeed={handleNewSeed}
              />
            )}
          </div>
        )}
      </div>
    </div>
  );
}
