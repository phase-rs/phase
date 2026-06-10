import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLocation, useNavigate, useSearchParams } from "react-router";

import type { FormatConfig, FormatGroup, GameFormat, MatchType } from "../adapter/types";
import { formatMetadata } from "../data/formatRegistry";
import { useAudioContext } from "../audio/useAudioContext";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { AiOpponentConfig } from "../components/menu/AiOpponentConfig";
import { FormatPicker } from "../components/menu/FormatPicker";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuPanel, MenuShell } from "../components/menu/MenuShell";
import { MyDecks, StatusBadge } from "../components/menu/MyDecks";
import { ModalPanelShell } from "../components/ui/ModalPanelShell";
import {
  COLOR_DOT_CLASS,
  getRepresentativeCard,
  getDeckCardCount,
} from "../components/menu/deckHelpers";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { ACTIVE_DECK_KEY, loadSavedDeckBracket, touchDeckPlayed } from "../constants/storage";
import { useCardImage } from "../hooks/useCardImage";
import { BRACKET_LABEL } from "../types/bracket";
import { effectiveAiDifficulty, isDeckCedhLegal } from "../services/cedhLock";
import { FORMAT_DEFAULTS } from "../stores/multiplayerStore";
import { usePreferencesStore } from "../stores/preferencesStore";
import { useCardDataStore } from "../stores/cardDataStore";
import { saveActiveGame, useGameStore } from "../stores/gameStore";
import type { DeckCompatibilityResult } from "../services/deckCompatibility";

// --- Format trigger styling ---
//
// The trigger is the same chip primitive that LobbyView already uses
// (bg-black/18 ring-1 ring-white/10 + kicker label + value). Group color
// shows up only as a small accent dot — the system rule is tone-on-fill
// or tone-on-text, never tone-on-border, so a tinted-border chip would
// have been a one-off here. Format selection opens the rich FormatPicker
// modal; the chip is just the trigger.

const GROUP_DOT_TONE: Record<FormatGroup, string> = {
  Constructed: "bg-indigo-300",
  Commander: "bg-amber-300",
  Limited: "bg-emerald-300",
  Multiplayer: "bg-emerald-300",
};

// --- Component ---

export function GameSetupPage() {
  const { t } = useTranslation("game");
  const navigate = useNavigate();
  const location = useLocation();
  const [searchParams] = useSearchParams();
  useAudioContext("menu");

  // Warm the shared card DB on mount so deck compat checks below are instant.
  // Idempotent — a no-op if the menu already warmed; closes the deep-link hole
  // when navigating straight to /setup.
  const cardStatus = useCardDataStore((s) => s.status);
  useEffect(() => {
    void useCardDataStore.getState().warm();
  }, []);

  // Format picker modal -- opened by the hero chip below the title. Mobile
  // gets a full-screen sheet via <ModalPanelShell>; desktop centers it.
  const [formatPickerOpen, setFormatPickerOpen] = useState(false);

  // Format & config state
  const [selectedFormat, setSelectedFormat] = useState<GameFormat | null>(null);
  const [formatConfig, setFormatConfig] = useState<FormatConfig | null>(null);
  const [playerCount, setPlayerCount] = useState(2);
  const [matchType, setMatchType] = useState<MatchType>("Bo1");
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  // We only ever read the active deck's compat (see `selectedCompat` below),
  // so MyDecks pushes up just that one entry instead of the entire map. Holding
  // the full map here previously caused a re-render every time *any* deck's
  // compat scanner result arrived — ~10/sec storm on the deck-select screen.
  const [selectedCompat, setSelectedCompat] = useState<DeckCompatibilityResult | null>(null);
  const [firstPlayer, setFirstPlayer] = useState<"random" | "play" | "draw">("random");
  const [legalAiDeckCount, setLegalAiDeckCount] = useState<number | null>(null);
  const [setupError, setSetupError] = useState<string | null>(() => {
    const state = location.state as { setupError?: string } | null;
    return state?.setupError ?? null;
  });

  // Preferences (persisted)
  const lastFormat = usePreferencesStore((s) => s.lastFormat);
  const lastMatchType = usePreferencesStore((s) => s.lastMatchType);
  const lastPlayerCount = usePreferencesStore((s) => s.lastPlayerCount);
  const setLastFormat = usePreferencesStore((s) => s.setLastFormat);
  const setLastMatchType = usePreferencesStore((s) => s.setLastMatchType);
  const setLastPlayerCount = usePreferencesStore((s) => s.setLastPlayerCount);

  // Restore last session on mount
  useEffect(() => {
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));

    // Allow direct format entry via ?format= search param
    const fmtParam = searchParams.get("format") as GameFormat | null;
    if (fmtParam && FORMAT_DEFAULTS[fmtParam]) {
      applyFormat(fmtParam);
      return;
    }

    // Restore last-used format, or default to Commander
    const fmt = lastFormat && FORMAT_DEFAULTS[lastFormat] ? lastFormat : "Commander";
    const defaults = FORMAT_DEFAULTS[fmt];
    setSelectedFormat(fmt);
    setFormatConfig(defaults);
    setPlayerCount(lastFormat ? lastPlayerCount : defaults.min_players);
    setMatchType(lastFormat ? lastMatchType : "Bo1");
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  function applyFormat(format: GameFormat) {
    const defaults = FORMAT_DEFAULTS[format];
    setSelectedFormat(format);
    setFormatConfig(defaults);
    setPlayerCount(defaults.min_players);
    setLastFormat(format);
    setLastPlayerCount(defaults.min_players);
    if (defaults.min_players !== 2) {
      setMatchType("Bo1");
      setLastMatchType("Bo1");
    }
    setSetupError(null);
  }

  // useCallback so the prop identity passed to MyDecks/SavedDeckTile2 stays
  // stable across this component's re-renders. Without it, every commit here
  // creates fresh closures, which React 19's profiler explicitly flags on
  // SavedDeckTile2 as `onTileClick`/`onEditDeck` "Referentially unequal
  // function closure" — causing all visible deck tiles to re-render on every
  // parent commit.
  const handleSelectDeck = useCallback((name: string) => {
    setActiveDeckName(name);
    localStorage.setItem(ACTIVE_DECK_KEY, name);
  }, []);

  const handleEditDeck = useCallback(
    (name: string) => {
      const returnTo = `${location.pathname}${location.search}`;
      const formatParam = selectedFormat ? `&format=${selectedFormat.toLowerCase()}` : "";
      navigate(
        `/deck-builder?deck=${encodeURIComponent(name)}${formatParam}&returnTo=${encodeURIComponent(returnTo)}`,
      );
    },
    [location.pathname, location.search, navigate, selectedFormat],
  );

  const handleStartAI = () => {
    if (!activeDeckName || !formatConfig) return;
    touchDeckPlayed(activeDeckName);
    const gameId = crypto.randomUUID();
    // Snapshot the per-seat AI config from preferences into the active-game
    // record. `AiOpponentConfig`'s `ensureAiSeatCount` effect normally syncs
    // the seat list before the user can click Start, but we re-invoke it
    // here defensively — zustand setters are synchronous, so this is a
    // no-op when the effect already ran and a correctness guarantee if the
    // click beat the effect to the commit boundary.
    const opponentCount = Math.max(1, playerCount - 1);
    const prefs = usePreferencesStore.getState();
    prefs.ensureAiSeatCount(opponentCount);
    const prefSeats = usePreferencesStore.getState().aiSeats.slice(0, opponentCount);
    // cEDH is a table-wide toggle, not a per-seat difficulty: when it's on,
    // every seat's engine difficulty resolves to "CEDH" (the per-seat value is
    // preserved in prefs for when cEDH is turned off).
    const cedhMode = prefs.cedhMode;
    const aiSeats = prefSeats.map((s) => ({
      difficulty: effectiveAiDifficulty(s.difficulty, cedhMode),
      deckId: s.deckId === "Random" ? null : s.deckId,
    }));
    const headDifficulty = aiSeats[0]?.difficulty ?? "Medium";
    saveActiveGame({ id: gameId, mode: "ai", difficulty: headDifficulty, aiSeats });
    useGameStore.setState({ gameId });
    const firstParam = firstPlayer !== "random" ? `&first=${firstPlayer}` : "";
    navigate(
      `/game/${gameId}?mode=ai&difficulty=${headDifficulty}&format=${formatConfig.format}&players=${playerCount}&match=${matchType.toLowerCase()}${firstParam}`,
    );
  };

  // Sidebar deck preview. `selectedCompat` is now state pushed up from MyDecks
  // (active-deck-only) rather than derived from a full compatibilities map.
  const noDeckSelected = !activeDeckName;
  const deckBlockedForSelectedFormat = selectedCompat?.selected_format_compatible === false;
  const noLegalAiDecks = legalAiDeckCount === 0;
  // Block start only while the card DB is actively loading — not on `error`/`idle`,
  // since initializeGame awaits ensureCardDb itself and an errored warm must not
  // trap the user on this screen.
  const cardDataLoading = cardStatus === "loading";
  const cannotStartAi = noDeckSelected || deckBlockedForSelectedFormat || noLegalAiDecks || cardDataLoading;

  // cEDH warning: shown when the human deck is not bracket 5 but the table is
  // in cEDH mode (all AI play cEDH).
  const cedhMode = usePreferencesStore((s) => s.cedhMode);
  const humanDeckBracket = activeDeckName ? loadSavedDeckBracket(activeDeckName) : null;
  const showCedhWarning =
    activeDeckName !== null &&
    cedhMode &&
    !isDeckCedhLegal(humanDeckBracket);
  const representativeCard = useMemo(
    () => (activeDeckName ? getRepresentativeCard(activeDeckName) : null),
    [activeDeckName],
  );
  const deckCardCount = useMemo(
    () => (activeDeckName ? getDeckCardCount(activeDeckName) : 0),
    [activeDeckName],
  );
  const { src: deckArtSrc } = useCardImage(representativeCard ?? "", { size: "art_crop" });
  const colors = selectedCompat?.color_identity ?? [];

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome onBack={() => navigate("/")} />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <MenuShell
        eyebrow={t("gameSetup.eyebrow")}
        title={t("gameSetup.title")}
        layout="stacked"
        aside={(() => {
          const meta = selectedFormat ? formatMetadata(selectedFormat) : null;
          const dotTone = meta ? GROUP_DOT_TONE[meta.group] : "bg-white/30";
          return (
            <div className="flex justify-center">
              <button
                type="button"
                onClick={() => setFormatPickerOpen(true)}
                aria-haspopup="dialog"
                aria-expanded={formatPickerOpen}
                aria-label={
                  meta
                    ? t("gameSetup.formatChip.ariaLabel", { label: meta.label, group: meta.group })
                    : t("gameSetup.formatChip.ariaLabelEmpty")
                }
                className="group flex min-h-[48px] items-center gap-3 rounded-[16px] bg-black/18 px-4 py-2.5 ring-1 ring-white/10 transition-colors hover:ring-white/20"
              >
                <span className="text-[0.62rem] font-medium uppercase tracking-[0.22em] text-slate-500">
                  {t("gameSetup.formatChip.kicker")}
                </span>
                <span className="flex items-center gap-2">
                  <span
                    className={`h-2 w-2 rounded-full ${dotTone}`}
                    aria-hidden="true"
                  />
                  <span className="text-base font-medium text-white">
                    {meta?.label ?? t("gameSetup.formatChip.choosePlaceholder")}
                  </span>
                </span>
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 20 20"
                  fill="currentColor"
                  className="h-4 w-4 text-slate-500 transition-colors group-hover:text-slate-300"
                  aria-hidden="true"
                >
                  <path
                    fillRule="evenodd"
                    d="M5.23 7.21a.75.75 0 0 1 1.06.02L10 11.06l3.71-3.83a.75.75 0 1 1 1.08 1.04l-4.25 4.39a.75.75 0 0 1-1.08 0L5.21 8.27a.75.75 0 0 1 .02-1.06Z"
                    clipRule="evenodd"
                  />
                </svg>
              </button>
            </div>
          );
        })()}
      >
        {/* `grid-cols-1` (= minmax(0,1fr)) is REQUIRED at mobile: without an
            explicit base template the grid falls back to an auto column that
            sizes to the deck grid's min-content (~628px) and overflows the
            viewport (clipped by the scene's overflow-hidden). The md template
            takes over for the deck-grid + sidebar split — splitting at md
            (768px) rather than lg keeps tablets on the compact 280px sidebar
            instead of ballooning the deck-preview art into a full-width hero. */}
        <div className="grid w-full grid-cols-1 gap-6 md:grid-cols-[minmax(0,1fr)_280px]">
          {/* Deck grid */}
          <MyDecks
            mode="select"
            selectedFormat={selectedFormat ?? undefined}
            selectedMatchType={matchType}
            onSelectDeck={handleSelectDeck}
            onEditDeck={handleEditDeck}
            activeDeckName={activeDeckName}
            bare
            onActiveDeckCompatChange={setSelectedCompat}
          />

          {/* Sidebar */}
          <div className="order-first md:sticky md:top-8 md:order-last md:self-start">
            <MenuPanel className="flex flex-col gap-4 px-4 py-4">
              {/* Deck preview */}
              {activeDeckName ? (
                <div>
                  <div className="aspect-[5/3] overflow-hidden rounded-xl bg-gray-800">
                    {deckArtSrc ? (
                      <img src={deckArtSrc} alt="" className="h-full w-full object-cover" />
                    ) : (
                      <div className="h-full w-full animate-pulse bg-gray-800" />
                    )}
                  </div>
                  <div className="mt-3 flex items-center gap-2">
                    <h3 className="min-w-0 flex-1 truncate text-base font-semibold text-white">
                      {activeDeckName}
                    </h3>
                    <button
                      type="button"
                      onClick={() => handleEditDeck(activeDeckName)}
                      className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-black/30 text-gray-300 transition-colors hover:bg-indigo-600 hover:text-white"
                      title={t("gameSetup.deckPreview.editDeck", { name: activeDeckName })}
                      aria-label={t("gameSetup.deckPreview.editDeck", { name: activeDeckName })}
                    >
                      <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
                        <path d="M11.013 1.427a1.75 1.75 0 0 1 2.474 2.474L6.226 11.16a2.25 2.25 0 0 1-.892.547l-2.115.705a.5.5 0 0 1-.632-.632l.705-2.115a2.25 2.25 0 0 1 .547-.892l7.174-7.346Z" />
                        <path d="M3.75 13.5a.75.75 0 0 0 0 1.5h8.5a.75.75 0 0 0 0-1.5h-8.5Z" />
                      </svg>
                    </button>
                  </div>
                  <div className="mt-1 flex items-center gap-2">
                    <div className="flex items-center gap-1 rounded-full bg-black/35 px-1.5 py-1 ring-1 ring-white/10">
                      {colors.map((c) => (
                        <span
                          key={c}
                          className={`inline-block h-2.5 w-2.5 rounded-full ${COLOR_DOT_CLASS[c] ?? "bg-gray-400"}`}
                        />
                      ))}
                      {colors.length === 0 && (
                        <span className="inline-block h-2.5 w-2.5 rounded-full bg-gray-500" />
                      )}
                    </div>
                    <span className="text-xs text-gray-300">{t("gameSetup.deckPreview.cardCount", { count: deckCardCount })}</span>
                  </div>
                  {selectedCompat && (
                    <div className="mt-2 flex flex-wrap gap-1">
                      {selectedCompat.standard.compatible && <StatusBadge label="STD" active />}
                      {selectedCompat.commander.compatible && <StatusBadge label="CMD" active />}
                      {selectedCompat.bo3_ready && <StatusBadge label="BO3" active />}
                      {selectedCompat.unknown_cards.length > 0 && (
                        <span
                          className="rounded bg-amber-500/80 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-black"
                          title={t("gameSetup.deckPreview.unknownCardsTitle", {
                            cards: selectedCompat.unknown_cards.join("\n"),
                          })}
                        >
                          {t("gameSetup.deckPreview.unknownBadge", {
                            count: selectedCompat.unknown_cards.length,
                          })}
                        </span>
                      )}
                    </div>
                  )}
                  {deckBlockedForSelectedFormat && (
                    <div className="mt-3 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
                      {selectedCompat.selected_format_reasons[0]
                        ?? t("gameSetup.deckNotLegal", { format: selectedFormat })}
                    </div>
                  )}

                  {showCedhWarning && (
                    <div
                      role="alert"
                      className="mt-3 rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-3 py-2 text-xs text-yellow-200"
                    >
                      {t("gameSetup.cedhWarning", {
                        bracket:
                          humanDeckBracket !== null
                            ? `${humanDeckBracket} (${BRACKET_LABEL[humanDeckBracket]})`
                            : t("gameSetup.cedhWarningUntagged"),
                      })}
                    </div>
                  )}
                </div>
              ) : (
                <div className="flex aspect-[5/3] flex-col items-center justify-center rounded-xl border border-dashed border-white/10 bg-black/12 text-center">
                  <svg aria-hidden="true" viewBox="0 0 24 24" className="h-10 w-10 fill-current text-slate-600">
                    <path d="M7 3h9a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Zm1 3v9h7V6H8Zm-2 15h11v-2H6v2Z" />
                  </svg>
                  <p className="mt-2 text-sm text-slate-500">{t("gameSetup.selectDeckPrompt")}</p>
                </div>
              )}

              {/* Separator */}
              <div className="border-t border-white/8" />

              {/* Config */}
              {formatConfig && (
                <div className="flex flex-col gap-3">
                  <label className="flex items-center justify-between">
                    <span className="text-xs text-slate-400">{t("gameSetup.config.startingLife")}</span>
                    <input
                      type="number"
                      value={formatConfig.starting_life}
                      onChange={(e) =>
                        setFormatConfig({ ...formatConfig, starting_life: Number(e.target.value) })
                      }
                      className="w-16 rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1 text-right text-sm text-white"
                    />
                  </label>

                  {!formatConfig.team_based && formatConfig.max_players > 2 && (
                    <label className="flex flex-col gap-1">
                      <div className="flex items-center justify-between">
                        <span className="text-xs text-slate-400">{t("gameSetup.config.players")}</span>
                        <span className="text-sm font-medium text-white">{playerCount}</span>
                      </div>
                      <input
                        type="range"
                        min={formatConfig.min_players}
                        max={formatConfig.max_players}
                        value={playerCount}
                        onChange={(e) => {
                          const next = Number(e.target.value);
                          setPlayerCount(next);
                          setLastPlayerCount(next);
                          if (next !== 2) {
                            setMatchType("Bo1");
                            setLastMatchType("Bo1");
                          }
                        }}
                        className="w-full"
                      />
                    </label>
                  )}

                  <div className="flex overflow-hidden rounded-lg border border-gray-700">
                    <button
                      type="button"
                      onClick={() => { setMatchType("Bo1"); setLastMatchType("Bo1"); }}
                      className={`flex-1 px-3 py-1.5 text-xs font-medium transition-colors ${
                        matchType === "Bo1"
                          ? "bg-indigo-600 text-white"
                          : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-200"
                      }`}
                    >
                      BO1
                    </button>
                    <button
                      type="button"
                      onClick={() => { setMatchType("Bo3"); setLastMatchType("Bo3"); }}
                      disabled={playerCount !== 2}
                      className={`flex-1 px-3 py-1.5 text-xs font-medium transition-colors ${
                        matchType === "Bo3"
                          ? "bg-indigo-600 text-white"
                          : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-200"
                      } ${playerCount !== 2 ? "cursor-not-allowed opacity-40" : ""}`}
                    >
                      BO3
                    </button>
                  </div>

                  <label className="flex flex-col gap-1">
                    <span className="text-xs text-slate-400">{t("gameSetup.config.whoGoesFirst")}</span>
                    <div className="flex overflow-hidden rounded-lg border border-gray-700">
                      {(["random", "play", "draw"] as const).map((opt) => (
                        <button
                          key={opt}
                          type="button"
                          onClick={() => setFirstPlayer(opt)}
                          className={`flex-1 px-3 py-1.5 text-xs font-medium capitalize transition-colors ${
                            firstPlayer === opt
                              ? "bg-indigo-600 text-white"
                              : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-200"
                          }`}
                        >
                          {t(`gameSetup.config.firstPlayer.${opt}`)}
                        </button>
                      ))}
                    </div>
                  </label>

                  {formatConfig.commander_damage_threshold != null && (
                    <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
                      {t("gameSetup.commanderNote", {
                        threshold: formatConfig.commander_damage_threshold,
                      })}
                    </div>
                  )}
                </div>
              )}

              {/* Separator */}
              <div className="border-t border-white/8" />

              {/* AI opponent configuration */}
              <AiOpponentConfig
                selectedFormat={formatConfig?.format}
                selectedMatchType={matchType}
                opponentCount={Math.max(1, playerCount - 1)}
                onCandidateCountChange={setLegalAiDeckCount}
              />

              {setupError && (
                <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
                  {setupError}
                </div>
              )}

              {noLegalAiDecks && (
                <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
                  {formatConfig?.format
                    ? t("gameSetup.noLegalAiDecks.withFormat", { format: formatConfig.format })
                    : t("gameSetup.noLegalAiDecks.generic")}
                </div>
              )}

              {/* Separator */}
              <div className="border-t border-white/8" />

              {/* Primary CTA — single dominant action on this page */}
              <button
                onClick={handleStartAI}
                disabled={cannotStartAi}
                className={menuButtonClass({
                  tone: "emerald",
                  size: "lg",
                  disabled: cannotStartAi,
                  // No `whitespace-nowrap`: the "Start Match (N opponents)" label
                  // grows with player count and would overflow the fixed 280px
                  // sidebar track, forcing page-wide horizontal scroll. Allow it
                  // to wrap within the column instead.
                  className: "w-full px-6 text-center",
                })}
              >
                {playerCount > 2
                  ? t("gameSetup.startMatchWithOpponents", { count: playerCount - 1 })
                  : t("gameSetup.startMatch")}
              </button>
            </MenuPanel>
          </div>
        </div>
      </MenuShell>

      <ModalPanelShell
        open={formatPickerOpen}
        eyebrow={t("gameSetup.eyebrow")}
        title={t("gameSetup.formatPicker.title")}
        subtitle={t("gameSetup.formatPicker.subtitle")}
        onClose={() => setFormatPickerOpen(false)}
        maxWidthClassName="max-w-3xl"
        bodyClassName="overflow-y-auto px-4 py-4 lg:px-6 lg:py-6"
      >
        <FormatPicker
          onFormatSelect={(format) => {
            applyFormat(format);
            setFormatPickerOpen(false);
          }}
        />
      </ModalPanelShell>
    </div>
  );
}
