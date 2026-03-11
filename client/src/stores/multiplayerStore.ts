import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { PlayerId } from "../adapter/types";

type ConnectionStatus = "disconnected" | "connecting" | "connected";

interface MultiplayerState {
  playerId: string;
  displayName: string;
  serverAddress: string;
  connectionStatus: ConnectionStatus;
  activePlayerId: PlayerId | null;
  opponentDisplayName: string | null;
  toastMessage: string | null;
}

interface MultiplayerActions {
  setDisplayName: (name: string) => void;
  setServerAddress: (address: string) => void;
  setConnectionStatus: (status: ConnectionStatus) => void;
  setActivePlayerId: (id: PlayerId | null) => void;
  setOpponentDisplayName: (name: string | null) => void;
  showToast: (message: string) => void;
  clearToast: () => void;
}

export const useMultiplayerStore = create<MultiplayerState & MultiplayerActions>()(
  persist(
    (set) => ({
      playerId: crypto.randomUUID(),
      displayName: "",
      serverAddress: "ws://localhost:9374/ws",
      connectionStatus: "disconnected",
      activePlayerId: null,
      opponentDisplayName: null,
      toastMessage: null,

      setDisplayName: (name) => set({ displayName: name }),
      setServerAddress: (address) => set({ serverAddress: address }),
      setConnectionStatus: (status) => set({ connectionStatus: status }),
      setActivePlayerId: (id) => set({ activePlayerId: id }),
      setOpponentDisplayName: (name) => set({ opponentDisplayName: name }),
      showToast: (message) => set({ toastMessage: message }),
      clearToast: () => set({ toastMessage: null }),
    }),
    {
      name: "phase-multiplayer",
      partialize: (state) => ({
        playerId: state.playerId,
        displayName: state.displayName,
        serverAddress: state.serverAddress,
      }),
    },
  ),
);
