import { listSavedDeckNames } from "../../constants/storage";
import { useNavigate, useLocation } from "react-router";
import { loadDeck } from "../menu/deckHelpers";

import {
  useMultiplayerStore,
  type DeckChoice,
  type PlayerSlot,
  type SeatMutation,
  type SeatKind,
} from "../../stores/multiplayerStore";

const AI_DIFFICULTIES = ["Easy", "Medium", "Hard", "VeryHard"] as const;
const RANDOM_DECK: DeckChoice = { type: "Random" };

function compatibleDeckChoices(
  minDeckSize: number,
  commandZone: boolean,
): Array<{ label: string; choice: DeckChoice }> {
  const choices: Array<{ label: string; choice: DeckChoice }> = [
    { label: "Random", choice: RANDOM_DECK },
  ];

  for (const deckName of listSavedDeckNames()) {
    const deck = loadDeck(deckName);
    if (!deck) continue;
    const mainCount = deck.main.reduce((sum, entry) => sum + entry.count, 0);
    const hasCommander = (deck.commander?.length ?? 0) > 0;
    if (mainCount < minDeckSize) continue;
    if (hasCommander !== commandZone) continue;
    choices.push({ label: deckName, choice: { type: "Named", data: deckName } });
  }

  return choices;
}

function seatLabel(kind: SeatKind): string {
  switch (kind.type) {
    case "HostHuman":
      return "Host";
    case "JoinedHuman":
      return "Player";
    case "WaitingHuman":
      return "Open";
    case "Ai":
      return `AI (${kind.data.difficulty})`;
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
  mutate,
}: {
  slot: PlayerSlot;
  minPlayers: number;
  seatCount: number;
  canEdit: boolean;
  deckChoices: Array<{ label: string; choice: DeckChoice }>;
  mutate: (mutation: SeatMutation) => void;
}) {
  const isOpen = slot.kind.type === "WaitingHuman";
  const kickLabel = slot.name || `Player ${slot.playerId + 1}`;
  const aiSeat = slot.kind.type === "Ai" ? slot.kind : null;
  return (
    <div className="py-1">
      <div className="flex items-center justify-between gap-2">
        <span className={`text-xs ${isOpen ? "italic text-slate-500" : "text-slate-300"}`}>
          {isOpen ? "Waiting…" : slot.name || `Seat ${slot.playerId}`}
        </span>
        <span className={`text-[10px] font-medium ${seatColor(slot.kind)}`}>
          {seatLabel(slot.kind)}
        </span>
      </div>
      {canEdit && slot.playerId !== 0 && (
        <div className="mt-1 flex flex-wrap gap-1">
          {slot.kind.type === "WaitingHuman" && (
            <>
              <button
                type="button"
                onClick={() =>
                  mutate({
                    type: "SetKind",
                    data: {
                      seatIndex: slot.playerId,
                      kind: {
                        type: "Ai",
                        data: { difficulty: "Medium", deck: RANDOM_DECK },
                      },
                    },
                  })
                }
                className="rounded border border-cyan-500/20 px-2 py-0.5 text-[10px] text-cyan-300"
              >
                Add AI
              </button>
              {seatCount > minPlayers && (
                <button
                  type="button"
                  onClick={() => mutate({ type: "Remove", data: { seatIndex: slot.playerId } })}
                  className="rounded border border-white/10 px-2 py-0.5 text-[10px] text-slate-400"
                >
                  Remove
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
                className="rounded border border-white/10 bg-slate-950 px-1 py-0.5 text-[10px] text-slate-200"
              >
                {AI_DIFFICULTIES.map((difficulty) => (
                  <option key={difficulty} value={difficulty}>
                    {difficulty}
                  </option>
                ))}
              </select>
              <select
                value={aiSeat?.data.deck.type === "Named" ? aiSeat.data.deck.data : ""}
                onChange={(e) =>
                  mutate({
                    type: "SetKind",
                    data: {
                      seatIndex: slot.playerId,
                      kind: {
                        type: "Ai",
                        data: {
                          difficulty: aiSeat?.data.difficulty ?? "Medium",
                          deck: e.target.value
                            ? { type: "Named", data: e.target.value }
                            : RANDOM_DECK,
                        },
                      },
                    },
                  })
                }
                className="rounded border border-white/10 bg-slate-950 px-1 py-0.5 text-[10px] text-slate-200"
              >
                {deckChoices.map(({ label, choice }) => (
                  <option
                    key={choice.type === "Named" ? choice.data : "Random"}
                    value={choice.type === "Named" ? choice.data : ""}
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
                className="rounded border border-white/10 px-2 py-0.5 text-[10px] text-slate-300"
              >
                Human
              </button>
              {seatCount > minPlayers && (
                <button
                  type="button"
                  onClick={() => mutate({ type: "Remove", data: { seatIndex: slot.playerId } })}
                  className="rounded border border-white/10 px-2 py-0.5 text-[10px] text-slate-400"
                >
                  Remove
                </button>
              )}
            </>
          )}
          {slot.kind.type === "JoinedHuman" && (
            <>
              <button
                type="button"
                onClick={() => {
                  if (!window.confirm(`Kick ${kickLabel} from the room?`)) return;
                  mutate({
                    type: "SetKind",
                    data: { seatIndex: slot.playerId, kind: { type: "WaitingHuman" } },
                  });
                }}
                className="rounded border border-amber-500/20 px-2 py-0.5 text-[10px] text-amber-300"
              >
                Kick
              </button>
              <button
                type="button"
                onClick={() => {
                  if (!window.confirm(`Replace ${kickLabel} with AI? This removes them from the room.`)) {
                    return;
                  }
                  mutate({
                    type: "SetKind",
                    data: {
                      seatIndex: slot.playerId,
                      kind: {
                        type: "Ai",
                        data: { difficulty: "Medium", deck: RANDOM_DECK },
                      },
                    },
                  });
                }}
                className="rounded border border-cyan-500/20 px-2 py-0.5 text-[10px] text-cyan-300"
              >
                Replace AI
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
  const navigate = useNavigate();
  const location = useLocation();

  if (hostingStatus === "idle") {
    return null;
  }

  const isConnecting = hostingStatus === "connecting";
  const minPlayers = hostSession?.formatConfig.min_players ?? 2;
  const deckChoices = compatibleDeckChoices(
    hostSession?.formatConfig.deck_size ?? 60,
    hostSession?.formatConfig.command_zone ?? false,
  );
  const waitingSeats = playerSlots.filter((slot) => slot.kind.type === "WaitingHuman");
  const occupiedSeats = playerSlots.length - waitingSeats.length;
  const canEditSeats = hostingStatus === "waiting";

  const startWithCurrentPlayers = () => {
    for (const slot of [...waitingSeats].sort((a, b) => b.playerId - a.playerId)) {
      seatMutate({ type: "Remove", data: { seatIndex: slot.playerId } });
    }
    seatMutate({ type: "Start" });
  };

  const fillWithAiAndStart = () => {
    for (const slot of waitingSeats) {
      seatMutate({
        type: "SetKind",
        data: {
          seatIndex: slot.playerId,
          kind: { type: "Ai", data: { difficulty: "Medium", deck: RANDOM_DECK } },
        },
      });
    }
    seatMutate({ type: "Start" });
  };

  return (
    <div
      className="fixed right-3 z-30 w-56"
      style={{ top: "calc(env(titlebar-area-height, 0px) + 0.75rem)" }}
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
              <span className="font-medium text-slate-400">Connecting…</span>
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
            aria-label="Cancel hosting"
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
                mutate={seatMutate}
              />
            ))}
          </div>
        )}
        {canEditSeats && hostSession && (
          <div className="border-t border-white/5 px-3 py-2">
            <div className="mb-2 text-[10px] uppercase tracking-wide text-slate-500">
              {occupiedSeats}/{playerSlots.length} seats occupied
            </div>
            <div className="flex flex-wrap gap-2">
              {waitingSeats.length === 0 ? (
                <button
                  type="button"
                  onClick={() => seatMutate({ type: "Start" })}
                  className="rounded border border-emerald-500/20 px-2 py-1 text-[11px] font-medium text-emerald-300"
                >
                  Start Game
                </button>
              ) : (
                <>
                  {occupiedSeats >= minPlayers && (
                    <button
                      type="button"
                      onClick={startWithCurrentPlayers}
                      className="rounded border border-emerald-500/20 px-2 py-1 text-[11px] font-medium text-emerald-300"
                    >
                      Start Now
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={fillWithAiAndStart}
                    className="rounded border border-cyan-500/20 px-2 py-1 text-[11px] font-medium text-cyan-300"
                  >
                    Fill With AI
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
