/**
 * R2-#4: `hordeAnchorSeat` — the P2P side of the Horde-designation rule.
 *
 * The engine designates `archenemy..archenemy+N` as the Horde, and every
 * one of those seats must be AI. The anchor is the lowest seat starting a
 * contiguous AI run covering the roster; a layout with no such run yields
 * -1 and `startPregameGame` rejects it rather than reassigning a human
 * seat. Same rule as server-core's `horde_anchor` on the WS path.
 */
import { describe, expect, it } from "vitest";

import type { SeatKind } from "../../multiplayer/seatTypes";
import { hordeAnchorSeat } from "../p2p-adapter";

const HUMAN: SeatKind = { type: "JoinedHuman" };
const AI: SeatKind = { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } };

describe("hordeAnchorSeat", () => {
  it("anchors a single-Horde roster on the first AI seat", () => {
    expect(hordeAnchorSeat([{ type: "HostHuman" }, AI, HUMAN], 1)).toBe(1);
  });

  it("anchors a Two Towers roster on a contiguous AI run", () => {
    expect(hordeAnchorSeat([{ type: "HostHuman" }, AI, AI], 2)).toBe(1);
  });

  it("skips a split run to a later covering run", () => {
    expect(hordeAnchorSeat([{ type: "HostHuman" }, HUMAN, AI, AI], 2)).toBe(2);
  });

  it("rejects [human, AI, human] for Two Towers: no run covers the roster", () => {
    expect(hordeAnchorSeat([{ type: "HostHuman" }, AI, HUMAN], 2)).toBe(-1);
  });

  it("rejects a table with no AI seats at all", () => {
    expect(hordeAnchorSeat([{ type: "HostHuman" }, HUMAN, HUMAN], 1)).toBe(-1);
  });

  it("a trailing partial run does not anchor", () => {
    // Only seat 2 is AI: a run starting at 1 covers seat 2's human, and a
    // run starting at 2 falls off the table edge.
    expect(hordeAnchorSeat([{ type: "HostHuman" }, HUMAN, AI], 2)).toBe(-1);
  });
});
