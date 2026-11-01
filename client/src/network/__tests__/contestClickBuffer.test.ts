import { describe, expect, it } from "vitest";

import {
  CLICK_BURST,
  ContestClickBuffer,
  MAX_PENDING_CLICKS_PER_SEAT,
  isContestSequence,
} from "../contestClickBuffer";

const CONTEST = { id: 4, players: [0, 1] as const };

function runningBuffer(): ContestClickBuffer {
  const buffer = new ContestClickBuffer();
  buffer.setContest(CONTEST);
  return buffer;
}

describe("isContestSequence", () => {
  it.each([0, 1, 41, 0xffff_ffff])("accepts the u32 %d", (value) => {
    expect(isContestSequence(value)).toBe(true);
  });

  it.each([-1, 1.5, 0x1_0000_0000, Number.NaN, Number.POSITIVE_INFINITY, "7", null, undefined])(
    "rejects %s, which the engine's u32 decode would refuse",
    (value) => {
      expect(isContestSequence(value)).toBe(false);
    },
  );
});

describe("ContestClickBuffer", () => {
  it("keeps nothing when no contest is running (ordinary play cannot grow the queue)", () => {
    const buffer = new ContestClickBuffer();
    for (let seq = 1; seq <= 10_000; seq++) {
      expect(buffer.offer(1, { contest: 4, seq }, seq)).toBe(false);
    }
    expect(buffer.drain()).toEqual([]);
  });

  it("accepts a click from a contestant for the running contest and stamps it", () => {
    const buffer = runningBuffer();
    expect(buffer.offer(1, { contest: 4, seq: 1 }, 1_250)).toBe(true);
    expect(buffer.drain()).toEqual([{ contest: 4, player: 1, seq: 1, at_ms: 1_250 }]);
    expect(buffer.drain()).toEqual([]);
  });

  it("drops a seat that is not one of the two contestants", () => {
    const buffer = runningBuffer();
    expect(buffer.offer(2, { contest: 4, seq: 1 }, 1_000)).toBe(false);
    expect(buffer.drain()).toEqual([]);
  });

  it("drops a frame addressed to another contest", () => {
    const buffer = runningBuffer();
    expect(buffer.offer(1, { contest: 3, seq: 1 }, 1_000)).toBe(false);
    expect(buffer.offer(1, { contest: "4", seq: 1 }, 1_000)).toBe(false);
    expect(buffer.drain()).toEqual([]);
  });

  it("isolates malformed frames: a bad sequence never reaches the batch or blocks good clicks", () => {
    const buffer = runningBuffer();
    const frames: unknown[] = [-1, 1.5, 0x1_0000_0000, Number.NaN, "3", null];
    frames.forEach((seq, index) => {
      expect(buffer.offer(1, { contest: 4, seq }, 1_000 + index)).toBe(false);
      expect(buffer.offer(1, { contest: 4, seq: index + 1 }, 1_000 + index)).toBe(true);
    });
    expect(buffer.drain().map((click) => click.seq)).toEqual([1, 2, 3, 4, 5, 6]);
  });

  it("bounds a flood inside a contest by rate and by queue length", () => {
    const buffer = runningBuffer();
    let kept = 0;
    // A thousand clicks in the same millisecond: only the burst allowance survives.
    for (let seq = 1; seq <= 1_000; seq++) {
      if (buffer.offer(1, { contest: 4, seq }, 5_000)) kept++;
    }
    expect(kept).toBe(Math.min(CLICK_BURST, MAX_PENDING_CLICKS_PER_SEAT));
    expect(buffer.drain()).toHaveLength(kept);
  });

  it("does not let one seat's flood starve the other seat", () => {
    const buffer = runningBuffer();
    for (let seq = 1; seq <= 1_000; seq++) buffer.offer(1, { contest: 4, seq }, 5_000);
    expect(buffer.offer(0, { contest: 4, seq: 1 }, 5_000)).toBe(true);
  });

  it("refills the rate allowance as time passes", () => {
    const buffer = runningBuffer();
    for (let seq = 1; seq <= CLICK_BURST; seq++) buffer.offer(1, { contest: 4, seq }, 0);
    buffer.drain();
    expect(buffer.offer(1, { contest: 4, seq: 100 }, 0)).toBe(false);
    expect(buffer.offer(1, { contest: 4, seq: 101 }, 1_000)).toBe(true);
  });

  it("discards what was buffered for a previous contest when the next one starts", () => {
    const buffer = runningBuffer();
    buffer.offer(1, { contest: 4, seq: 1 }, 1_000);
    buffer.setContest({ id: 5, players: [0, 1] });
    expect(buffer.drain()).toEqual([]);
    expect(buffer.offer(1, { contest: 4, seq: 2 }, 1_001)).toBe(false);
    expect(buffer.offer(1, { contest: 5, seq: 1 }, 1_002)).toBe(true);
  });

  it("keeps buffered clicks when the same contest is re-reported", () => {
    const buffer = runningBuffer();
    buffer.offer(1, { contest: 4, seq: 1 }, 1_000);
    buffer.setContest(CONTEST);
    expect(buffer.drain()).toHaveLength(1);
  });

  it("stops accepting once the contest is over", () => {
    const buffer = runningBuffer();
    buffer.setContest(null);
    expect(buffer.offer(1, { contest: 4, seq: 1 }, 1_000)).toBe(false);
  });
});
