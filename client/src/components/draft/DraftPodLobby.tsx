/**
 * Draft Pod Lobby — 8-seat grid with bot-fill controls and start button.
 *
 * Displays the current pod state as an 8-seat grid where each seat shows:
 * - Host (seat 0): always filled, highlighted
 * - Connected guests: display name + connected indicator
 * - Empty seats: "Waiting..." or "Bot" if bot-fill is enabled
 *
 * Host-only controls:
 * - Bot-fill toggle (fill remaining seats with bots on start)
 * - Start Draft button (enabled when at least 2 seats are filled or bot-fill is on)
 * - Kick player buttons for connected guests
 *
 * Guest view:
 * - Read-only seat grid with their seat highlighted
 * - Waiting for host to start
 */

import { useCallback } from "react";

import type { SeatPublicView } from "../../adapter/draft-adapter";
import { menuButtonClass } from "../menu/buttonStyles";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { useDraftPodStore } from "../../stores/draftPodStore";

// ── Seat Card ─────────────────────────────────────────────────────────

interface SeatCardProps {
  seat: SeatPublicView;
  isHost: boolean;
  isLocalSeat: boolean;
  botFillEnabled: boolean;
  canKick: boolean;
  onKick: () => void;
}

function SeatCard({
  seat,
  isHost,
  isLocalSeat,
  botFillEnabled,
  canKick,
  onKick,
}: SeatCardProps) {
  const isEmpty = !seat.display_name;
  const seatLabel = isEmpty
    ? botFillEnabled
      ? "Bot"
      : "Waiting..."
    : seat.display_name;

  const borderColor = isLocalSeat
    ? "border-emerald-400/40"
    : seat.connected
      ? "border-white/20"
      : isEmpty
        ? "border-white/8"
        : "border-amber-400/30";

  const bgColor = isLocalSeat
    ? "bg-emerald-400/8"
    : seat.connected
      ? "bg-white/5"
      : isEmpty
        ? "bg-white/2"
        : "bg-amber-400/5";

  return (
    <div
      className={`relative flex flex-col items-center gap-2 rounded-xl border p-4 ${borderColor} ${bgColor}`}
    >
      {/* Seat number */}
      <div className="text-xs font-medium text-white/40">
        Seat {seat.seat_index + 1}
      </div>

      {/* Status indicator */}
      <div className="flex items-center gap-2">
        <div
          className={`h-2 w-2 rounded-full ${
            seat.connected
              ? "bg-emerald-400"
              : isEmpty && botFillEnabled
                ? "bg-blue-400/60"
                : isEmpty
                  ? "bg-white/20"
                  : "bg-amber-400"
          }`}
        />
        <span
          className={`text-sm font-medium ${
            isEmpty ? "italic text-white/40" : "text-white/80"
          }`}
        >
          {seatLabel}
        </span>
      </div>

      {/* Role badge */}
      {seat.seat_index === 0 && (
        <span className="rounded-full bg-emerald-400/15 px-2 py-0.5 text-[10px] font-semibold text-emerald-300">
          HOST
        </span>
      )}

      {/* Kick button (host only, not for self or empty seats) */}
      {canKick && !isEmpty && !isLocalSeat && isHost && (
        <button
          onClick={onKick}
          className="absolute right-2 top-2 rounded px-1.5 py-0.5 text-[10px] text-red-300/60 transition-colors hover:bg-red-400/10 hover:text-red-300"
        >
          Kick
        </button>
      )}
    </div>
  );
}

// ── DraftPodLobby ─────────────────────────────────────────────────────

interface DraftPodLobbyProps {
  /** Callback when host leaves the pod (navigates back). */
  onLeave: () => void;
}

export function DraftPodLobby({ onLeave }: DraftPodLobbyProps) {
  const role = useMultiplayerDraftStore((s) => s.role);
  const seats = useMultiplayerDraftStore((s) => s.seats);
  const joined = useMultiplayerDraftStore((s) => s.joined);
  const total = useMultiplayerDraftStore((s) => s.total);
  const roomCode = useMultiplayerDraftStore((s) => s.roomCode);
  const seatIndex = useMultiplayerDraftStore((s) => s.seatIndex);
  const error = useMultiplayerDraftStore((s) => s.error);
  const kickPlayer = useMultiplayerDraftStore((s) => s.kickPlayer);
  const leave = useMultiplayerDraftStore((s) => s.leave);

  const botFillEnabled = useDraftPodStore((s) => s.botFillEnabled);
  const toggleBotFill = useDraftPodStore((s) => s.toggleBotFill);
  const startDraft = useDraftPodStore((s) => s.startDraft);
  const config = useDraftPodStore((s) => s.config);

  const isHost = role === "host";
  const filledSeats = seats.filter((s) => s.display_name).length;
  const canStart = isHost && (filledSeats >= 2 || botFillEnabled);

  // Build a full 8-seat grid (pad with empty seats if the adapter
  // hasn't sent all seats yet)
  const displaySeats: SeatPublicView[] = [];
  for (let i = 0; i < (total || config.podSize); i++) {
    displaySeats.push(
      seats[i] ?? {
        seat_index: i,
        display_name: "",
        is_bot: false,
        connected: false,
        has_submitted_deck: false,
      },
    );
  }

  const handleLeave = useCallback(async () => {
    await leave();
    onLeave();
  }, [leave, onLeave]);

  const handleStart = useCallback(async () => {
    await startDraft();
  }, [startDraft]);

  const handleCopyCode = useCallback(() => {
    if (roomCode) {
      navigator.clipboard.writeText(roomCode).catch(() => {
        // Clipboard API may not be available
      });
    }
  }, [roomCode]);

  return (
    <div className="flex flex-col gap-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-2xl font-bold text-white">Draft Pod Lobby</h2>
          <p className="mt-1 text-sm text-white/50">
            {config.setName || config.setCode} &mdash;{" "}
            {config.kind} Draft
          </p>
        </div>

        {/* Room code display */}
        {roomCode && (
          <button
            onClick={handleCopyCode}
            className="group flex flex-col items-end gap-0.5"
            title="Click to copy room code"
          >
            <span className="text-xs text-white/40">Room Code</span>
            <span className="font-mono text-lg font-bold tracking-wider text-emerald-300 transition-colors group-hover:text-emerald-200">
              {roomCode}
            </span>
            <span className="text-[10px] text-white/30 transition-colors group-hover:text-white/50">
              Click to copy
            </span>
          </button>
        )}
      </div>

      {/* Seat count */}
      <div className="text-sm text-white/60">
        {joined || filledSeats} / {total || config.podSize} seats filled
      </div>

      {/* Seat grid — 4x2 for 8 seats */}
      <div className="grid grid-cols-4 gap-3">
        {displaySeats.map((seat) => (
          <SeatCard
            key={seat.seat_index}
            seat={seat}
            isHost={isHost}
            isLocalSeat={seat.seat_index === seatIndex}
            botFillEnabled={botFillEnabled}
            canKick={isHost && seat.seat_index !== 0}
            onKick={() => kickPlayer(seat.seat_index)}
          />
        ))}
      </div>

      {/* Error display */}
      {error && (
        <div className="rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300">
          {error}
        </div>
      )}

      {/* Host controls */}
      {isHost && (
        <div className="flex items-center gap-4">
          {/* Bot-fill toggle */}
          <label className="flex cursor-pointer items-center gap-2 text-sm text-white/70">
            <input
              type="checkbox"
              checked={botFillEnabled}
              onChange={toggleBotFill}
              className="accent-emerald-400"
            />
            Fill empty seats with bots
          </label>

          <div className="flex-1" />

          {/* Leave button */}
          <button
            onClick={handleLeave}
            className={menuButtonClass({ tone: "red", size: "sm" })}
          >
            Leave
          </button>

          {/* Start Draft button */}
          <button
            onClick={handleStart}
            disabled={!canStart}
            className={menuButtonClass({
              tone: "emerald",
              size: "md",
              disabled: !canStart,
            })}
          >
            Start Draft
          </button>
        </div>
      )}

      {/* Guest controls */}
      {!isHost && (
        <div className="flex items-center gap-4">
          <p className="text-sm italic text-white/50">
            Waiting for host to start the draft...
          </p>
          <div className="flex-1" />
          <button
            onClick={handleLeave}
            className={menuButtonClass({ tone: "red", size: "sm" })}
          >
            Leave
          </button>
        </div>
      )}
    </div>
  );
}
