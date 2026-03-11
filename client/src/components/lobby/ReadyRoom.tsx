import { useState } from "react";

import type { PlayerSlot } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";

interface ReadyRoomProps {
  gameCode: string;
  playerSlots: PlayerSlot[];
  currentPlayerId: string;
  isHost: boolean;
  minPlayers: number;
  onToggleReady: () => void;
  onStartGame: () => void;
  onCancel: () => void;
  onSendChat: (message: string) => void;
  chatMessages: ChatMessage[];
}

export interface ChatMessage {
  sender: string;
  text: string;
  timestamp: number;
}

const DIFFICULTY_BADGE_COLORS: Record<string, string> = {
  VeryEasy: "bg-gray-500/20 text-gray-400",
  Easy: "bg-green-500/20 text-green-300",
  Medium: "bg-amber-500/20 text-amber-300",
  Hard: "bg-red-500/20 text-red-300",
  VeryHard: "bg-purple-500/20 text-purple-300",
};

export function ReadyRoom({
  gameCode,
  playerSlots,
  currentPlayerId,
  isHost,
  minPlayers,
  onToggleReady,
  onStartGame,
  onCancel,
  onSendChat,
  chatMessages,
}: ReadyRoomProps) {
  const [chatInput, setChatInput] = useState("");

  const readyHumanCount = playerSlots.filter((s) => !s.isAi && s.isReady).length;
  const humanCount = playerSlots.filter((s) => !s.isAi).length;
  const totalReady = readyHumanCount + playerSlots.filter((s) => s.isAi).length;
  const canStart = totalReady >= minPlayers;

  const currentSlot = playerSlots.find((s) => s.playerId === currentPlayerId);
  const isReady = currentSlot?.isReady ?? false;

  const handleSendChat = () => {
    const text = chatInput.trim();
    if (!text) return;
    onSendChat(text);
    setChatInput("");
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/80" />

      <div className="relative z-10 flex w-full max-w-md flex-col gap-5 rounded-xl bg-gray-900 p-8 shadow-2xl ring-1 ring-gray-700">
        {/* Header */}
        <div className="flex items-center justify-between">
          <h2 className="text-xl font-bold text-white">Ready Room</h2>
          <span className="rounded bg-gray-700 px-2 py-0.5 font-mono text-sm tracking-wider text-emerald-400">
            {gameCode}
          </span>
        </div>

        {/* Player list */}
        <div className="flex flex-col gap-1.5">
          <p className="text-xs font-medium uppercase tracking-wider text-gray-400">
            Players ({humanCount}/{playerSlots.length})
          </p>
          {playerSlots.map((slot, index) => (
            <div
              key={slot.playerId}
              className="flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-800/50 px-3 py-2"
            >
              {/* Ready indicator */}
              <div
                className={`h-2 w-2 rounded-full ${
                  slot.isAi || slot.isReady ? "bg-emerald-400" : "bg-gray-600"
                }`}
              />

              {/* Seat number */}
              <span className="text-xs font-medium text-gray-500">{index + 1}</span>

              {/* Name */}
              <span className="flex-1 truncate text-sm text-gray-200">
                {slot.isAi ? "AI" : slot.name || "Player"}
                {slot.playerId === currentPlayerId && (
                  <span className="ml-1 text-xs text-cyan-400">(you)</span>
                )}
              </span>

              {/* Deck name (only show for current player) */}
              {slot.playerId === currentPlayerId && slot.deckName && (
                <span className="truncate text-xs text-gray-500">{slot.deckName}</span>
              )}

              {/* AI difficulty badge */}
              {slot.isAi && (
                <span
                  className={`rounded px-1.5 py-0.5 text-xs font-medium ${
                    DIFFICULTY_BADGE_COLORS[slot.aiDifficulty] ?? DIFFICULTY_BADGE_COLORS.Medium
                  }`}
                >
                  {slot.aiDifficulty}
                </span>
              )}

              {/* Ready status text */}
              {!slot.isAi && (
                <span className={`text-xs ${slot.isReady ? "text-emerald-400" : "text-gray-500"}`}>
                  {slot.isReady ? "Ready" : "Not Ready"}
                </span>
              )}
            </div>
          ))}
        </div>

        {/* Chat area */}
        <div className="flex flex-col gap-1.5">
          <p className="text-xs font-medium uppercase tracking-wider text-gray-400">Chat</p>
          <div className="flex h-24 flex-col gap-0.5 overflow-y-auto rounded-lg border border-gray-700 bg-gray-800/30 p-2">
            {chatMessages.length === 0 ? (
              <p className="text-xs italic text-gray-600">No messages yet</p>
            ) : (
              chatMessages.map((msg, i) => (
                <p key={i} className="text-xs text-gray-300">
                  <span className="font-medium text-cyan-400">{msg.sender}:</span> {msg.text}
                </p>
              ))
            )}
          </div>
          <div className="flex gap-1.5">
            <input
              type="text"
              value={chatInput}
              onChange={(e) => setChatInput(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleSendChat()}
              placeholder="Type a message..."
              maxLength={200}
              className="flex-1 rounded-lg bg-gray-800 px-3 py-1.5 text-xs text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
            />
            <button
              onClick={handleSendChat}
              disabled={!chatInput.trim()}
              className={menuButtonClass({
                tone: "cyan",
                size: "sm",
                disabled: !chatInput.trim(),
              })}
            >
              Send
            </button>
          </div>
        </div>

        {/* Action buttons */}
        <div className="flex items-center justify-between">
          <button
            onClick={onCancel}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            Leave
          </button>

          <div className="flex gap-2">
            <button
              onClick={onToggleReady}
              className={menuButtonClass({
                tone: isReady ? "amber" : "emerald",
                size: "sm",
              })}
            >
              {isReady ? "Unready" : "Ready"}
            </button>

            {isHost && (
              <button
                onClick={onStartGame}
                disabled={!canStart}
                className={menuButtonClass({
                  tone: "emerald",
                  size: "md",
                  disabled: !canStart,
                })}
              >
                Start Game
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
