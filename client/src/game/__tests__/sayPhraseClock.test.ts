import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction } from "../../adapter/types";
import { sayPhraseHostClockMs, stampSayPhraseAtHost } from "../sayPhraseClock";

describe("spoken-payment host clock", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("reads whole epoch milliseconds", () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_730_000_000_123);
    expect(sayPhraseHostClockMs()).toBe(1_730_000_000_123);
    expect(Number.isSafeInteger(sayPhraseHostClockMs())).toBe(true);
  });

  it("replaces a guest's claimed instant with the host's, floored", () => {
    const claimed: GameAction = {
      type: "SayPhrase",
      data: { payment: 3, seq: 2, at_ms: 1, report: "Saying" },
    };
    expect(stampSayPhraseAtHost(claimed, 1_730_000_000_456.9)).toEqual({
      type: "SayPhrase",
      data: { payment: 3, seq: 2, at_ms: 1_730_000_000_456, report: "Saying" },
    });
  });

  it("leaves every other action untouched", () => {
    const pass: GameAction = { type: "PassPriority" };
    expect(stampSayPhraseAtHost(pass, 99)).toBe(pass);
  });
});
