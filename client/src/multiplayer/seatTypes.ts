import type { FormatConfig } from "../adapter/types";
import type { CommanderBracketTier } from "../types/bracketEstimate";

/** The `DeckChoice::DeckList` payload: the client mirror of the engine's
 *  `DeckData` (`crates/engine/src/starter_decks.rs`). Every list the engine
 *  defaults (`#[serde(default)]`) is optional here; an absent list is empty. */
export interface DeckListData {
  main_deck: string[];
  sideboard: string[];
  commander: string[];
  companion?: string[];
  attraction_deck?: string[];
  planar_deck?: string[];
  scheme_deck?: string[];
  contraption_deck?: string[];
  sticker_sheets?: string[];
  signature_spell?: string[];
  /** Declared Commander bracket tier; the engine defaults an omitted tier to Core. */
  bracket_tier?: CommanderBracketTier;
}

export type DeckChoice =
  | { type: "Random" }
  | { type: "Named"; data: string }
  | { type: "DeckList"; data: DeckListData };

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
  | {
      type: "SetKind";
      data: {
        seatIndex: number;
        kind: SeatKind;
        /** Live reducer precondition; mismatch succeeds without changing the seat. */
        expectedKind?: SeatKind;
      };
    }
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
