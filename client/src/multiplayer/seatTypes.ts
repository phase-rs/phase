import type { FormatConfig } from "../adapter/types";

export type DeckChoice =
  | { type: "Random" }
  | { type: "Named"; data: string }
  | {
      type: "DeckList";
      data: {
        main_deck: string[];
        sideboard: string[];
        commander: string[];
        planar_deck?: string[];
        scheme_deck?: string[];
      };
    };

export type SeatKind =
  | { type: "HostHuman" }
  | { type: "JoinedHuman" }
  | { type: "WaitingHuman" }
  | { type: "Ai"; data: { difficulty: string; deck: DeckChoice } };

export interface SeatTeamInfo {
  teamIndex: number;
  positionInTeam: number;
}

export interface PlayerSlot {
  playerId: number;
  name: string;
  kind: SeatKind;
  teamInfo?: SeatTeamInfo | null;
  reserved?: boolean;
  reservationExpiresAtMs?: number | null;
}

export type SeatMutation =
  | { type: "SetKind"; data: { seatIndex: number; kind: SeatKind } }
  | { type: "Remove"; data: { seatIndex: number } }
  | { type: "Start" };

export interface SeatState {
  seats: SeatKind[];
  tokens: string[];
  format: FormatConfig;
  gameStarted: boolean;
}

export interface SeatView {
  seats: SeatKind[];
  format: FormatConfig;
  teamInfo?: Array<SeatTeamInfo | null>;
  isFull: boolean;
  gameStarted: boolean;
}

export interface SeatDelta {
  mutatedSeats: number[];
  invalidatedTokens: string[];
  removedAi: number[];
  newAi: Array<[number, string, unknown]>;
  renumbering: { removedIndex: number; remapping: Array<[number, number]> } | null;
  nowStarted: boolean;
}

export interface SeatMutationResult {
  state: SeatState;
  delta: SeatDelta;
}
