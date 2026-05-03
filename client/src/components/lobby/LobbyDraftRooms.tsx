import { useCallback, useState } from "react";

import type { LobbyGame } from "../../adapter/types";
import type { CreateDraftSettings } from "../../adapter/server-draft-adapter";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";

// ── Draft Room List ─────────────────────────────────────────────────────

interface LobbyDraftRoomsProps {
  /** Draft-filtered lobby games (caller is responsible for the filter). */
  draftRooms: LobbyGame[];
  /** Called when a P2P draft room's join button is clicked. The parent
   * handles P2P join flow (password prompt, PeerJS dial, etc.). */
  onJoinP2P?: (code: string) => void;
}

/**
 * Renders draft pod lobby entries with a badge distinguishing server-hosted
 * rooms from player-hosted (P2P) rooms. Server-hosted rooms wire directly
 * to `joinServerDraft` in the store; P2P rooms delegate to the caller's
 * `onJoinP2P` handler (which feeds into the existing P2P guest flow).
 */
export function LobbyDraftRooms({ draftRooms, onJoinP2P }: LobbyDraftRoomsProps) {
  const joinServerDraft = useMultiplayerStore((s) => s.joinServerDraft);
  const serverAddress = useMultiplayerStore((s) => s.serverAddress);
  const displayName = useMultiplayerStore((s) => s.displayName);

  const handleJoin = useCallback(
    (room: LobbyGame) => {
      if (room.is_p2p === true) {
        onJoinP2P?.(room.game_code);
      } else {
        void joinServerDraft(
          serverAddress,
          room.game_code,
          displayName || "Player",
        );
      }
    },
    [joinServerDraft, serverAddress, displayName, onJoinP2P],
  );

  if (draftRooms.length === 0) {
    return (
      <p className="text-sm text-zinc-500">No draft rooms available</p>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      {draftRooms.map((room) => (
        <div
          key={room.game_code}
          className="flex items-center justify-between rounded-[18px] border border-white/10 bg-black/18 px-4 py-3"
        >
          <div className="flex items-center gap-3">
            <span
              className={`flex-shrink-0 rounded px-1.5 py-0.5 text-xs font-semibold ${
                room.is_p2p === true
                  ? "bg-teal-500/20 text-teal-300"
                  : "bg-emerald-500/20 text-emerald-300"
              }`}
            >
              {room.is_p2p === true ? "Player-hosted" : "Server"}
            </span>
            {room.draft_metadata && (
              <>
                <span className="text-sm font-medium text-gray-200">
                  {room.draft_metadata.setCode}
                </span>
                <span className="text-xs text-gray-400">
                  {room.draft_metadata.draftKind}
                </span>
              </>
            )}
            {room.max_players != null && (
              <span className="text-xs text-gray-400">
                {room.current_players ?? 1}/{room.max_players}
              </span>
            )}
          </div>

          <div className="flex items-center gap-2">
            {room.has_password && (
              <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 20 20"
                fill="currentColor"
                className="h-4 w-4 text-amber-400"
                aria-label="Password protected"
              >
                <path
                  fillRule="evenodd"
                  d="M10 1a4.5 4.5 0 0 0-4.5 4.5V9H5a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-6a2 2 0 0 0-2-2h-.5V5.5A4.5 4.5 0 0 0 10 1Zm3 8V5.5a3 3 0 1 0-6 0V9h6Z"
                  clipRule="evenodd"
                />
              </svg>
            )}
            <span className="rounded-full border border-white/10 bg-black/18 px-2 py-0.5 font-mono text-xs tracking-wider text-emerald-400">
              {room.game_code}
            </span>
            <button
              onClick={() => handleJoin(room)}
              className={menuButtonClass({ tone: "cyan", size: "sm" })}
            >
              Join
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}

// ── Create Server Draft Form ────────────────────────────────────────────

const DEFAULT_POD_SIZE = 8;
const DEFAULT_TIMER_SECONDS = 75;

interface CreateServerDraftFormProps {
  onClose: () => void;
}

/**
 * Inline form for creating a new server-hosted draft pod. On submit, calls
 * `createServerDraft` from the store with the chosen settings.
 */
export function CreateServerDraftForm({ onClose }: CreateServerDraftFormProps) {
  const createServerDraft = useMultiplayerStore((s) => s.createServerDraft);
  const serverAddress = useMultiplayerStore((s) => s.serverAddress);
  const displayName = useMultiplayerStore((s) => s.displayName);

  const [setCode, setSetCode] = useState("FDN");
  const [kind, setKind] = useState<"Premier" | "Traditional">("Premier");
  const [podSize, setPodSize] = useState(DEFAULT_POD_SIZE);
  const [password, setPassword] = useState("");
  const [timerSeconds, setTimerSeconds] = useState(DEFAULT_TIMER_SECONDS);
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      setSubmitting(true);
      const settings: CreateDraftSettings = {
        displayName: displayName || "Player",
        setCode: setCode.toUpperCase(),
        kind,
        public: true,
        password: password || undefined,
        timerSeconds,
        tournamentFormat: kind === "Traditional" ? "Swiss" : "Swiss",
        podPolicy: "Competitive",
        podSize,
      };
      try {
        await createServerDraft(serverAddress, settings);
        onClose();
      } catch (err) {
        console.error("[CreateServerDraft] failed:", err);
      } finally {
        setSubmitting(false);
      }
    },
    [createServerDraft, serverAddress, displayName, setCode, kind, podSize, password, timerSeconds, onClose],
  );

  return (
    <form
      onSubmit={(e) => void handleSubmit(e)}
      className="space-y-4 rounded-[18px] border border-white/10 bg-black/30 p-4"
    >
      <h3 className="text-sm font-semibold text-white">Create Server Draft</h3>

      <div className="flex flex-wrap gap-3">
        {/* Set code */}
        <label className="flex flex-col gap-1">
          <span className="text-[0.62rem] uppercase tracking-wider text-gray-500">Set</span>
          <input
            type="text"
            value={setCode}
            onChange={(e) => setSetCode(e.target.value)}
            maxLength={5}
            className="w-20 rounded-lg bg-gray-800 px-2 py-1.5 font-mono text-sm text-white outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
          />
        </label>

        {/* Kind */}
        <label className="flex flex-col gap-1">
          <span className="text-[0.62rem] uppercase tracking-wider text-gray-500">Kind</span>
          <select
            value={kind}
            onChange={(e) => setKind(e.target.value as "Premier" | "Traditional")}
            className="rounded-lg bg-gray-800 px-2 py-1.5 text-sm text-white outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
          >
            <option value="Premier">Premier</option>
            <option value="Traditional">Traditional</option>
          </select>
        </label>

        {/* Pod size */}
        <label className="flex flex-col gap-1">
          <span className="text-[0.62rem] uppercase tracking-wider text-gray-500">Pod size</span>
          <input
            type="number"
            min={2}
            max={8}
            value={podSize}
            onChange={(e) => setPodSize(Number(e.target.value))}
            className="w-16 rounded-lg bg-gray-800 px-2 py-1.5 text-sm text-white outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
          />
        </label>

        {/* Timer */}
        <label className="flex flex-col gap-1">
          <span className="text-[0.62rem] uppercase tracking-wider text-gray-500">Timer (s)</span>
          <input
            type="number"
            min={15}
            max={300}
            value={timerSeconds}
            onChange={(e) => setTimerSeconds(Number(e.target.value))}
            className="w-20 rounded-lg bg-gray-800 px-2 py-1.5 text-sm text-white outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
          />
        </label>
      </div>

      {/* Password */}
      <label className="flex flex-col gap-1">
        <span className="text-[0.62rem] uppercase tracking-wider text-gray-500">
          Password (optional)
        </span>
        <input
          type="text"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder="Leave blank for public"
          className="rounded-lg bg-gray-800 px-2 py-1.5 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
        />
      </label>

      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onClose}
          className={menuButtonClass({ tone: "neutral", size: "sm" })}
        >
          Cancel
        </button>
        <button
          type="submit"
          disabled={submitting || !setCode.trim()}
          className={menuButtonClass({
            tone: "emerald",
            size: "sm",
            disabled: submitting || !setCode.trim(),
          })}
        >
          {submitting ? "Creating..." : "Create Draft"}
        </button>
      </div>
    </form>
  );
}
