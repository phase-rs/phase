/**
 * `remapArchenemySeat` — the P2P side of the host-picked archenemy seat.
 *
 * The host picks a lobby seat at setup, but a partial start (`startNow`
 * with waiting seats) compacts unfilled seats out of the engine order, so
 * lobby seat N is engine seat (N minus unfilled seats below N). The remap
 * is identity when the room is full; a picked seat with no player (waiting
 * or past the table edge) yields -1 and `startPregameGame` rejects it
 * rather than designating the wrong player.
 */
import { describe, expect, it } from "vitest";

import type { SeatKind } from "../../multiplayer/seatTypes";
import { remapArchenemySeat } from "../p2p-adapter";

const HOST: SeatKind = { type: "HostHuman" };
const HUMAN: SeatKind = { type: "JoinedHuman" };
const WAITING: SeatKind = { type: "WaitingHuman" };
const AI: SeatKind = { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } };

describe("remapArchenemySeat", () => {
  it("is identity when the room is full", () => {
    const seats = [HOST, HUMAN, HUMAN, AI];
    expect(remapArchenemySeat(seats, 0)).toBe(0);
    expect(remapArchenemySeat(seats, 1)).toBe(1);
    expect(remapArchenemySeat(seats, 2)).toBe(2);
    expect(remapArchenemySeat(seats, 3)).toBe(3);
  });

  it("maps the host to engine seat 0 even with waiting seats below", () => {
    expect(remapArchenemySeat([HOST, WAITING, HUMAN], 0)).toBe(0);
  });

  it("shifts past a waiting seat below the picked seat", () => {
    // Engine order is [host, lobby seat 2]: the picked lobby seat 2 is
    // engine seat 1.
    expect(remapArchenemySeat([HOST, WAITING, HUMAN], 2)).toBe(1);
  });

  it("counts AI seats as filled when shifting", () => {
    // Engine order is [host, AI, lobby seat 3]: the picked lobby seat 3
    // is engine seat 2.
    expect(remapArchenemySeat([HOST, AI, WAITING, HUMAN], 3)).toBe(2);
  });

  it("rejects a picked seat that is still waiting", () => {
    expect(remapArchenemySeat([HOST, HUMAN, WAITING], 2)).toBe(-1);
  });

  it("rejects a picked seat past the table edge", () => {
    expect(remapArchenemySeat([HOST, HUMAN], 2)).toBe(-1);
    expect(remapArchenemySeat([HOST, HUMAN], -1)).toBe(-1);
  });
});
