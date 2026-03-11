import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { FormatConfig, GameFormat, PlayerId } from "../adapter/types";

type ConnectionStatus = "disconnected" | "connecting" | "connected";

export interface PlayerSlot {
  playerId: string;
  name: string;
  isReady: boolean;
  isAi: boolean;
  aiDifficulty: string;
  deckName: string | null;
}

interface MultiplayerState {
  playerId: string;
  displayName: string;
  serverAddress: string;
  connectionStatus: ConnectionStatus;
  activePlayerId: PlayerId | null;
  opponentDisplayName: string | null;
  toastMessage: string | null;
  formatConfig: FormatConfig | null;
  playerSlots: PlayerSlot[];
  spectators: string[];
  isSpectator: boolean;
}

interface MultiplayerActions {
  setDisplayName: (name: string) => void;
  setServerAddress: (address: string) => void;
  setConnectionStatus: (status: ConnectionStatus) => void;
  setActivePlayerId: (id: PlayerId | null) => void;
  setOpponentDisplayName: (name: string | null) => void;
  showToast: (message: string) => void;
  clearToast: () => void;
  setFormatConfig: (config: FormatConfig | null) => void;
  setPlayerSlots: (slots: PlayerSlot[]) => void;
  toggleReady: (playerId: string) => void;
  setSpectators: (names: string[]) => void;
  setIsSpectator: (value: boolean) => void;
}

export const FORMAT_DEFAULTS: Record<GameFormat, FormatConfig> = {
  Standard: {
    format: "Standard",
    starting_life: 20,
    min_players: 2,
    max_players: 2,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
  },
  Commander: {
    format: "Commander",
    starting_life: 40,
    min_players: 2,
    max_players: 4,
    deck_size: 100,
    singleton: true,
    command_zone: true,
    commander_damage_threshold: 21,
    range_of_influence: null,
    team_based: false,
  },
  FreeForAll: {
    format: "FreeForAll",
    starting_life: 20,
    min_players: 3,
    max_players: 6,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
  },
  TwoHeadedGiant: {
    format: "TwoHeadedGiant",
    starting_life: 30,
    min_players: 4,
    max_players: 4,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: true,
  },
};

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
      formatConfig: null,
      playerSlots: [],
      spectators: [],
      isSpectator: false,

      setDisplayName: (name) => set({ displayName: name }),
      setServerAddress: (address) => set({ serverAddress: address }),
      setConnectionStatus: (status) => set({ connectionStatus: status }),
      setActivePlayerId: (id) => set({ activePlayerId: id }),
      setOpponentDisplayName: (name) => set({ opponentDisplayName: name }),
      showToast: (message) => set({ toastMessage: message }),
      clearToast: () => set({ toastMessage: null }),
      setFormatConfig: (config) => set({ formatConfig: config }),
      setPlayerSlots: (slots) => set({ playerSlots: slots }),
      toggleReady: (playerId) =>
        set((state) => ({
          playerSlots: state.playerSlots.map((slot) =>
            slot.playerId === playerId ? { ...slot, isReady: !slot.isReady } : slot,
          ),
        })),
      setSpectators: (names) => set({ spectators: names }),
      setIsSpectator: (value) => set({ isSpectator: value }),
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
