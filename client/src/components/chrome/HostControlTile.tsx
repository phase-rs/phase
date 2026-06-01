import { useEffect } from "react";
import { useNavigate, useLocation } from "react-router";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import {
  useMultiplayerStore,
  type DeckChoice,
  type PlayerSlot,
  type SeatMutation,
  type SeatKind,
} from "../../stores/multiplayerStore";
import { useAiDeckCatalog } from "../../services/aiDeckCatalog";
import { expandParsedDeck } from "../../services/deckParser";

const AI_DIFFICULTIES = ["Easy", "Medium", "Hard", "VeryHard"] as const;
const RANDOM_DECK: DeckChoice = { type: "Random" };

function deckChoiceKey(choice: DeckChoice): string {
  switch (choice.type) {
    case "Random":
      return "Random";
    case "Named":
      return `Named:${choice.data}`;
    case "DeckList":
      return JSON.stringify(choice.data);
  }
}

function seatLabel(kind: SeatKind, t: TFunction): string {
  switch (kind.type) {
    case "HostHuman":
      return t("hostControl.seatHost");
    case "JoinedHuman":
      return t("hostControl.seatPlayer");
    case "WaitingHuman":
      return t("hostControl.seatOpen");
    case "Ai":
      return t("hostControl.seatAi", { difficulty: kind.data.difficulty });
  }
}

function seatColor(kind: SeatKind): string {
  switch (kind.type) {
    case "HostHuman":
      return "text-amber-400";
    case "JoinedHuman":
      return "text-emerald-400";
    case "WaitingHuman":
      return "text-slate-500";
    case "Ai":
      return "text-cyan-400";
  }
}

function SeatRow({
  slot,
  minPlayers,
  seatCount,
  canEdit,
  deckChoices,
  pickRandomAiDeck,
  mutate,
}: {
  slot: PlayerSlot;
  minPlayers: number;
  seatCount: number;
  canEdit: boolean;
  deckChoices: Array<{ id: string; label: string; choice: DeckChoice }>;
  pickRandomAiDeck: () => DeckChoice | null;
  mutate: (mutation: SeatMutation) => void;
}) {
  const { t } = useTranslation();
  const isOpen = slot.kind.type === "WaitingHuman";
  const kickLabel = slot.name || t("hostControl.fallbackPlayerName", { number: slot.playerId + 1 });
  const aiSeat = slot.kind.type === "Ai" ? slot.kind : null;
  const selectedDeckKey = aiSeat ? deckChoiceKey(aiSeat.data.deck) : "";
  return (
    <div className="py-1">
      <div className="flex items-center justify-between gap-2">
        <span className={`text-sm ${isOpen ? "italic text-slate-500" : "text-slate-300"}`}>
          {isOpen ? t("hostControl.waiting") : slot.name || t("hostControl.seatLabel", { id: slot.playerId })}
        </span>
        <span className={`text-xs font-medium ${seatColor(slot.kind)}`}>
          {seatLabel(slot.kind, t)}
        </span>
      </div>
      {canEdit && slot.playerId !== 0 && (
        <div className="mt-1 flex flex-wrap gap-1">
          {slot.kind.type === "WaitingHuman" && (
            <>
              <button
                type="button"
                disabled={deckChoices.length === 0}
                onClick={() => {
                  const deck = pickRandomAiDeck();
                  if (!deck) return;
                  mutate({
                    type: "SetKind",
                    data: {
                      seatIndex: slot.playerId,
                      kind: {
                        type: "Ai",
                        data: { difficulty: "Medium", deck },
                      },
                    },
                  });
                }}
                className="rounded border border-cyan-500/20 px-2 py-0.5 text-xs text-cyan-300 disabled:cursor-not-allowed disabled:opacity-50"
              >
                {t("hostControl.addAi")}
              </button>
              {seatCount > minPlayers && (
                <button
                  type="button"
                  onClick={() => mutate({ type: "Remove", data: { seatIndex: slot.playerId } })}
                  className="rounded border border-white/10 px-2 py-0.5 text-xs text-slate-400"
                >
                  {t("hostControl.remove")}
                </button>
              )}
            </>
          )}
          {slot.kind.type === "Ai" && (
            <>
              <select
                value={slot.kind.data.difficulty}
                onChange={(e) =>
                  mutate({
                    type: "SetKind",
                    data: {
                      seatIndex: slot.playerId,
                      kind: {
                        type: "Ai",
                        data: {
                          difficulty: e.target.value,
                          deck: slot.kind.type === "Ai" ? slot.kind.data.deck : RANDOM_DECK,
                        },
                      },
                    },
                  })
                }
                className="rounded border border-white/10 bg-slate-950 px-1 py-0.5 text-xs text-slate-200"
              >
                {AI_DIFFICULTIES.map((difficulty) => (
                  <option key={difficulty} value={difficulty}>
                    {t(`menu:aiDifficulty.levels.${difficulty}`)}
                  </option>
                ))}
              </select>
              <select
                value={selectedDeckKey}
                onChange={(e) =>
                  mutate({
                    type: "SetKind",
                    data: {
                      seatIndex: slot.playerId,
                      kind: {
                        type: "Ai",
                        data: {
                          difficulty: aiSeat?.data.difficulty ?? "Medium",
                          deck: deckChoices.find(({ choice }) => deckChoiceKey(choice) === e.target.value)
                            ?.choice ?? aiSeat?.data.deck ?? RANDOM_DECK,
                        },
                      },
                    },
                  })
                }
                className="rounded border border-white/10 bg-slate-950 px-1 py-0.5 text-xs text-slate-200"
              >
                {deckChoices.map(({ id, label, choice }) => (
                  <option
                    key={id}
                    value={deckChoiceKey(choice)}
                  >
                    {label}
                  </option>
                ))}
              </select>
              <button
                type="button"
                onClick={() =>
                  mutate({
                    type: "SetKind",
                    data: { seatIndex: slot.playerId, kind: { type: "WaitingHuman" } },
                  })
                }
                className="rounded border border-white/10 px-2 py-0.5 text-xs text-slate-300"
              >
                {t("hostControl.human")}
              </button>
              {seatCount > minPlayers && (
                <button
                  type="button"
                  onClick={() => mutate({ type: "Remove", data: { seatIndex: slot.playerId } })}
                  className="rounded border border-white/10 px-2 py-0.5 text-xs text-slate-400"
                >
                  {t("hostControl.remove")}
                </button>
              )}
            </>
          )}
          {slot.kind.type === "JoinedHuman" && (
            <>
              <button
                type="button"
                onClick={() => {
                  if (!window.confirm(t("hostControl.kickConfirm", { name: kickLabel }))) return;
                  mutate({
                    type: "SetKind",
                    data: { seatIndex: slot.playerId, kind: { type: "WaitingHuman" } },
                  });
                }}
                className="rounded border border-amber-500/20 px-2 py-0.5 text-xs text-amber-300"
              >
                {t("hostControl.kick")}
              </button>
              <button
                type="button"
                disabled={deckChoices.length === 0}
                onClick={() => {
                  if (!window.confirm(t("hostControl.replaceConfirm", { name: kickLabel }))) {
                    return;
                  }
                  const deck = pickRandomAiDeck();
                  if (!deck) return;
                  mutate({
                    type: "SetKind",
                    data: {
                      seatIndex: slot.playerId,
                      kind: {
                        type: "Ai",
                        data: { difficulty: "Medium", deck },
                      },
                    },
                  });
                }}
                className="rounded border border-cyan-500/20 px-2 py-0.5 text-xs text-cyan-300 disabled:cursor-not-allowed disabled:opacity-50"
              >
                {t("hostControl.replaceAi")}
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}

export function HostControlTile() {
  const hostGameCode = useMultiplayerStore((s) => s.hostGameCode);
  const hostingStatus = useMultiplayerStore((s) => s.hostingStatus);
  const cancelHosting = useMultiplayerStore((s) => s.cancelHosting);
  const playerSlots = useMultiplayerStore((s) => s.playerSlots);
  const hostSession = useMultiplayerStore((s) => s.hostSession);
  const seatMutate = useMultiplayerStore((s) => s.seatMutate);
  const seatMutateAsync = useMultiplayerStore((s) => s.seatMutateAsync);
  const startLobbyWithCurrentPlayers = useMultiplayerStore(
    (s) => s.startLobbyWithCurrentPlayers,
  );
  const showToast = useMultiplayerStore((s) => s.showToast);
  const navigate = useNavigate();
  const location = useLocation();
  const { t } = useTranslation();
  const aiDeckCatalog = useAiDeckCatalog({
    selectedFormat: hostSession?.formatConfig.format,
    selectedMatchType: hostSession?.matchType,
  });

  const isConnecting = hostingStatus === "connecting";
  const minPlayers = hostSession?.formatConfig.min_players ?? 2;
  const deckChoices = aiDeckCatalog.candidates.map((candidate) => ({
    id: candidate.id,
    label: candidate.name,
    choice: { type: "DeckList" as const, data: expandParsedDeck(candidate.deck) },
  }));
  // Pick a random format-legal deck for each fresh AI-seat assignment.
  // Previously this defaulted to `deckChoices[0]`, which meant every AI seat
  // got the same deck — uninteresting and exposed catalog ordering. The
  // engine's `DeckChoice::Random` can't be used here because it pulls from
  // `STARTER_DECKS` (Standard-format hardcoded list) and would produce an
  // illegal deck for any non-Standard format. `aiDeckCatalog.candidates` is
  // already filtered by `selectedFormat`, so picking from it gives a random
  // format-legal deck.
  const pickRandomAiDeck = (): DeckChoice | null => {
    if (deckChoices.length === 0) return null;
    const idx = Math.floor(Math.random() * deckChoices.length);
    return deckChoices[idx].choice;
  };
  // Used only for the dropdown's fallback/disabled state — buttons that
  // assign a deck call `pickRandomAiDeck()` per-seat instead.
  const haveAnyDeck = deckChoices.length > 0;
  const waitingSeats = playerSlots.filter((slot) => slot.kind.type === "WaitingHuman");
  const occupiedSeats = playerSlots.length - waitingSeats.length;
  const canEditSeats = hostingStatus === "waiting";

  useEffect(() => {
    if (!canEditSeats || !haveAnyDeck) return;
    for (const slot of playerSlots) {
      if (slot.kind.type !== "Ai" || slot.kind.data.deck.type === "DeckList") continue;
      // Each "promote non-DeckList AI seat to a real deck" decision gets a
      // fresh random pick from the format-legal catalog.
      const deck = pickRandomAiDeck();
      if (!deck) continue;
      seatMutate({
        type: "SetKind",
        data: {
          seatIndex: slot.playerId,
          kind: {
            type: "Ai",
            data: {
              difficulty: slot.kind.data.difficulty,
              deck,
            },
          },
        },
      });
    }
    // `pickRandomAiDeck` is intentionally omitted from deps — it closes over
    // `deckChoices` which is already a dep (via `haveAnyDeck`), and including
    // the function would cause an effect storm on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canEditSeats, haveAnyDeck, playerSlots, seatMutate]);

  if (hostingStatus === "idle") {
    return null;
  }

  const fillWithAiAndStart = () => {
    if (!haveAnyDeck) return;
    void (async () => {
      for (const slot of waitingSeats) {
        const deck = pickRandomAiDeck();
        if (!deck) return;
        await seatMutateAsync({
          type: "SetKind",
          data: {
            seatIndex: slot.playerId,
            kind: { type: "Ai", data: { difficulty: "Medium", deck } },
          },
        });
      }
      await seatMutateAsync({ type: "Start" });
    })().catch((err) => {
      showToast(err instanceof Error ? err.message : String(err));
    });
  };

  return (
    <div
      className="fixed inset-x-3 top-[calc(env(safe-area-inset-top)+4.75rem)] z-40 sm:left-auto sm:right-3 sm:top-[calc(env(titlebar-area-height,0px)+0.75rem)] sm:w-72"
    >
      <div className="rounded-xl border border-white/10 bg-black/70 shadow-lg shadow-black/40 backdrop-blur-md">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-white/5 px-3 py-2">
          <button
            type="button"
            onClick={() => {
              if (location.pathname.startsWith("/game/") && hostGameCode) {
                void navigator.clipboard?.writeText(hostGameCode);
              } else {
                navigate("/multiplayer");
              }
            }}
            className="flex items-center gap-2 text-xs text-slate-300 transition-colors hover:text-white"
          >
            <span className="relative flex h-2 w-2">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75" />
              <span className="relative inline-flex h-2 w-2 rounded-full bg-emerald-400" />
            </span>
            {isConnecting ? (
              <span className="font-medium text-slate-400">{t("hostControl.connecting")}</span>
            ) : (
              <>
                <span className="font-mono tracking-wider text-emerald-400">
                  {hostGameCode}
                </span>
                {hostSession && (
                  <span className="text-slate-500">
                    {hostSession.formatConfig.format}
                  </span>
                )}
              </>
            )}
          </button>
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              cancelHosting();
              if (location.pathname.startsWith("/game/")) {
                navigate("/multiplayer");
              }
            }}
            className="text-slate-500 transition-colors hover:text-rose-400"
            aria-label={t("hostControl.cancelHosting")}
          >
            ✕
          </button>
        </div>

        {/* Seat list — read-only in Phase 1 */}
        {playerSlots.length > 0 && (
          <div className="px-3 py-2">
            {playerSlots.map((slot) => (
              <SeatRow
                key={slot.playerId}
                slot={slot}
                minPlayers={minPlayers}
                seatCount={playerSlots.length}
                canEdit={canEditSeats}
                deckChoices={deckChoices}
                pickRandomAiDeck={pickRandomAiDeck}
                mutate={seatMutate}
              />
            ))}
          </div>
        )}
        {canEditSeats && hostSession && (
          <div className="border-t border-white/5 px-3 py-2">
            <div className="mb-2 text-xs uppercase tracking-wide text-slate-500">
              {t("hostControl.seatsOccupied", { occupied: occupiedSeats, total: playerSlots.length })}
            </div>
            <div className="flex flex-wrap gap-2">
              {waitingSeats.length === 0 ? (
                <button
                  type="button"
                  onClick={() => seatMutate({ type: "Start" })}
                  className="rounded border border-emerald-500/20 px-2 py-1 text-xs font-medium text-emerald-300"
                >
                  {t("hostControl.startGame")}
                </button>
              ) : (
                <>
                  {occupiedSeats >= minPlayers && (
                    <button
                      type="button"
                      onClick={() => {
                        void startLobbyWithCurrentPlayers().catch((err) => {
                          showToast(err instanceof Error ? err.message : String(err));
                        });
                      }}
                      className="rounded border border-emerald-500/20 px-2 py-1 text-xs font-medium text-emerald-300"
                    >
                      {t("hostControl.startNow")}
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={fillWithAiAndStart}
                    disabled={!haveAnyDeck}
                    className="rounded border border-cyan-500/20 px-2 py-1 text-xs font-medium text-cyan-300 disabled:cursor-not-allowed disabled:opacity-50"
                  >
                    {t("hostControl.fillWithAi")}
                  </button>
                </>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
