import { describe, expect, it } from "vitest";

import type { ContestResult, ContestState } from "../../../adapter/types.ts";
import { localContestSeats, stareContestPose, stareElapsedMs, stareFinale, stareStrain } from "../staring.ts";

describe("stareElapsedMs", () => {
  it("is the snapshot's time since GO plus the local time since it arrived", () => {
    expect(stareElapsedMs({ clock_ms: 5_000, go_at_ms: 4_000 }, 250)).toBe(1_250);
  });

  it("stops at the pause, however long the outage runs", () => {
    const paused = { clock_ms: 9_000, go_at_ms: 4_000, paused_since_ms: 5_000 };
    expect(stareElapsedMs(paused, 0)).toBe(1_000);
    expect(stareElapsedMs(paused, 60_000)).toBe(1_000);
  });

  it("is zero without a GO, and before it", () => {
    expect(stareElapsedMs({ clock_ms: 5_000, go_at_ms: null }, 100)).toBe(0);
    expect(stareElapsedMs({ clock_ms: 1_000, go_at_ms: 4_000 }, 100)).toBe(0);
  });

  it("ignores a local clock that ran backwards", () => {
    expect(stareElapsedMs({ clock_ms: 5_000, go_at_ms: 4_000 }, -500)).toBe(1_000);
  });
});

describe("stareStrain", () => {
  it("starts relaxed, always climbs, and never peaks", () => {
    expect(stareStrain(0)).toBe(0);
    const curve = [1_000, 5_000, 20_000, 60_000, 300_000].map(stareStrain);
    expect(curve).toEqual([...curve].sort((a, b) => a - b));
    expect(new Set(curve).size).toBe(curve.length);
    expect(curve[curve.length - 1]).toBeLessThan(1);
  });

  it("is plainly visible within a contest against the AI's 2.5–9s stamina", () => {
    expect(stareStrain(5_000)).toBeGreaterThan(0.25);
  });
});

describe("stareFinale", () => {
  const decided = (winner: number, loser: number): ContestResult => ({
    Outcome: { Decided: { winner, loser, reason: "Inactivity" } },
  });
  const staring = (result: ContestResult) => ({
    kind: "Staring" as const,
    participants: [0, 1],
    result,
  });

  it("blinks the eye when the contestant at this screen won", () => {
    expect(stareFinale(staring(decided(0, 1)), [0])).toBe("EyeBlinks");
  });

  it("blinks the viewer when the contestant at this screen lost", () => {
    expect(stareFinale(staring(decided(1, 0)), [0])).toBe("ViewerBlinks");
  });

  it("blinks both on a tie and neither on a cancellation", () => {
    expect(stareFinale(staring({ Outcome: "Tie" }), [0])).toBe("BothBlink");
    expect(stareFinale(staring({ Cancelled: "TimeLimit" }), [0])).toBeNull();
  });

  it.each<[string, number[]]>([
    ["a spectator", []],
    ["a seat outside the contest", [5]],
    ["hotseat's two players", [0, 1]],
  ])("always blinks the eye for %s, who have no one viewer to blink", (_name, seats) => {
    expect(stareFinale(staring(decided(0, 1)), seats)).toBe("EyeBlinks");
    expect(stareFinale(staring(decided(1, 0)), seats)).toBe("EyeBlinks");
    expect(stareFinale(staring({ Outcome: "Tie" }), seats)).toBe("EyeBlinks");
  });

  it("belongs to staring contests only", () => {
    expect(stareFinale({ ...staring(decided(0, 1)), kind: "BreathHolding" }, [0])).toBeNull();
  });
});

describe("localContestSeats", () => {
  it("is this client's own seat, if it has one in the contest", () => {
    expect(localContestSeats([0, 1], "ai", 1, [0])).toEqual([1]);
    expect(localContestSeats([0, 1], "ai", 5, [])).toEqual([]);
  });

  it("is every human in hotseat", () => {
    expect(localContestSeats([0, 1], "local", 0, [])).toEqual([0, 1]);
    expect(localContestSeats([0, 1], "local", 0, [1])).toEqual([0]);
  });
});

describe("stareContestPose", () => {
  const contest = (phase: ContestState["phase"], paused_since_ms?: number) =>
    ({ phase, paused_since_ms }) as ContestState;

  it("waits until GO, stares from then on, and a pause changes neither", () => {
    expect(stareContestPose(contest("Pending"))).toBe("Preparing");
    expect(stareContestPose(contest("Preparing"))).toBe("Preparing");
    expect(stareContestPose(contest("Active"))).toBe("Staring");
    expect(stareContestPose(contest("Resolving"))).toBe("Staring");
    expect(stareContestPose(contest("Active", 1_000))).toBe("Staring");
    expect(stareContestPose(contest("Preparing", 1_000))).toBe("Preparing");
  });
});
