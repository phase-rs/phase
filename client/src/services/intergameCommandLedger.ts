import type { DeckCardCount } from "../adapter/types";

/**
 * A pod-issued command is deliberately distinct from an engine action.  The
 * draft coordinator owns its lifecycle; the game adapter only receives a
 * command after the coordinator has made it executable.
 */
export type DraftIntergameCommandPayload =
  | { type: "SubmitSideboard"; main: DeckCardCount[]; sideboard: DeckCardCount[] }
  | { type: "ChoosePlayDraw"; playFirst: boolean };

export type DraftIntergameCommandStatus =
  | "Pending"
  | "Authorized"
  | "Executing"
  | "Receipted";

export interface DraftIntergameCommand {
  commandId: string;
  matchId: string;
  gameNumber: number;
  seat: number;
  payload: DraftIntergameCommandPayload;
  /** Held launch descriptor; its digest binds the command to this exact match. */
  launchPayload: unknown;
  /** Bound to the original match launch so commands cannot cross matches. */
  launchDigest: string;
  payloadDigest: string;
  status: DraftIntergameCommandStatus;
  receiptId?: string;
}

/** The immutable acknowledgement predicate echoed by the executor. */
export interface DraftIntergameCommandAck {
  commandId: string;
  matchId: string;
  gameNumber: number;
  seat: number;
  launchDigest: string;
  payloadDigest: string;
}

/** Stable enough for a persisted integrity binding, without treating it as crypto. */
export function draftIntergameDigest(value: unknown): string {
  const text = JSON.stringify(canonicalize(value));
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index++) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return `d${(hash >>> 0).toString(16).padStart(8, "0")}`;
}

export function commandAcknowledgement(command: DraftIntergameCommand): DraftIntergameCommandAck {
  return {
    commandId: command.commandId,
    matchId: command.matchId,
    gameNumber: command.gameNumber,
    seat: command.seat,
    launchDigest: command.launchDigest,
    payloadDigest: command.payloadDigest,
  };
}

export function matchesCommandAcknowledgement(
  command: DraftIntergameCommand,
  acknowledgement: DraftIntergameCommandAck,
): boolean {
  const expected = commandAcknowledgement(command);
  return expected.commandId === acknowledgement.commandId
    && expected.matchId === acknowledgement.matchId
    && expected.gameNumber === acknowledgement.gameNumber
    && expected.seat === acknowledgement.seat
    && expected.launchDigest === acknowledgement.launchDigest
    && expected.payloadDigest === acknowledgement.payloadDigest;
}

/**
 * This issuer is intentionally private to this module. A caller can present a
 * serializable command, but only a permit created by the matching controller
 * can cross the final local execution gate.
 */
const permits = new WeakMap<object, DraftIntergameCommandAck>();

export class IntergameCommandController {
  private readonly commands = new Map<string, DraftIntergameCommand>();

  constructor(commands: readonly DraftIntergameCommand[] = []) {
    for (const command of commands) this.commands.set(command.commandId, command);
  }

  snapshot(): DraftIntergameCommand[] {
    return [...this.commands.values()].map((command) => ({ ...command }));
  }

  hold(command: Omit<DraftIntergameCommand, "status" | "payloadDigest">): DraftIntergameCommand {
    const held: DraftIntergameCommand = {
      ...command,
      payloadDigest: draftIntergameDigest(command.payload),
      launchPayload: immutablePayload(command.launchPayload),
      status: "Pending",
    };
    this.commands.set(held.commandId, held);
    return held;
  }

  authorize(commandId: string, acknowledgement: DraftIntergameCommandAck): DraftIntergameCommand | null {
    const command = this.commands.get(commandId);
    if (!command || command.status !== "Pending" || !matchesCommandAcknowledgement(command, acknowledgement)) {
      return null;
    }
    const authorized = { ...command, status: "Authorized" as const };
    this.commands.set(commandId, authorized);
    return authorized;
  }

  /** Re-checks immutable launch + payload bindings immediately before the sink. */
  begin(commandId: string, acknowledgement: DraftIntergameCommandAck): object | null {
    const command = this.commands.get(commandId);
    if (!command || command.status !== "Authorized" || !matchesCommandAcknowledgement(command, acknowledgement)) {
      return null;
    }
    const executing = { ...command, status: "Executing" as const };
    this.commands.set(commandId, executing);
    const permit = {};
    permits.set(permit, acknowledgement);
    return permit;
  }

  receipt(commandId: string, acknowledgement: DraftIntergameCommandAck, receiptId: string): DraftIntergameCommand | null {
    const command = this.commands.get(commandId);
    if (!command || command.status !== "Executing" || !matchesCommandAcknowledgement(command, acknowledgement)) {
      return null;
    }
    const receipted = { ...command, status: "Receipted" as const, receiptId };
    this.commands.set(commandId, receipted);
    return receipted;
  }

  /** A recovered Executing command stays non-replayable until the receipt arrives. */
  recover(): void {
    for (const command of this.commands.values()) {
      if (command.status === "Executing") {
        this.commands.set(command.commandId, { ...command, status: "Receipted", receiptId: command.receiptId ?? "recovered" });
      }
    }
  }
}

export function consumeIntergamePermit(
  permit: object,
  acknowledgement: DraftIntergameCommandAck,
): boolean {
  const issued = permits.get(permit);
  if (!issued) return false;
  permits.delete(permit);
  return issued.commandId === acknowledgement.commandId
    && issued.matchId === acknowledgement.matchId
    && issued.gameNumber === acknowledgement.gameNumber
    && issued.seat === acknowledgement.seat
    && issued.launchDigest === acknowledgement.launchDigest
    && issued.payloadDigest === acknowledgement.payloadDigest;
}

function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, child]) => [key, canonicalize(child)]),
    );
  }
  return value;
}

function immutablePayload(value: unknown): unknown {
  return JSON.parse(JSON.stringify(value)) as unknown;
}
