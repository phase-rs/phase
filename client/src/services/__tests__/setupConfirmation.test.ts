import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SETUP_CONFIRMATION_TIMEOUT_MS,
  SetupConfirmation,
  classifySetupFrame,
} from "../setupConfirmation";

describe("SetupConfirmation", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it("settles once with the first outcome and ignores later ones", async () => {
    const confirmation = new SetupConfirmation();
    confirmation.closed();
    confirmation.confirm();
    await expect(confirmation.settled).resolves.toBe("closed");
    expect(confirmation.current).toBe("closed");
  });

  it("times out after the bound and leaves no timer once settled", async () => {
    const confirmation = new SetupConfirmation();
    await vi.advanceTimersByTimeAsync(SETUP_CONFIRMATION_TIMEOUT_MS - 1);
    expect(confirmation.current).toBeNull();
    await vi.advanceTimersByTimeAsync(1);
    await expect(confirmation.settled).resolves.toBe("timeout");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("clears its timer when confirmed", async () => {
    const confirmation = new SetupConfirmation();
    confirmation.confirm();
    await expect(confirmation.settled).resolves.toBe("confirmed");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("settles on an abort only once the abort is watched", async () => {
    const controller = new AbortController();
    const confirmation = new SetupConfirmation();
    controller.abort();
    expect(confirmation.current).toBeNull();
    confirmation.watchAbort(controller.signal);
    await expect(confirmation.settled).resolves.toBe("aborted");
  });

  it("settles on a later abort of a watched signal", async () => {
    const controller = new AbortController();
    const confirmation = new SetupConfirmation();
    confirmation.watchAbort(controller.signal);
    expect(confirmation.current).toBeNull();
    controller.abort();
    await expect(confirmation.settled).resolves.toBe("aborted");
  });
});

describe("classifySetupFrame", () => {
  const frame = (type: string, data?: unknown) => JSON.stringify({ type, data });

  it.each(["GameStarted", "GameCreated", "SessionAttached", "StateWithheld"])(
    "treats game %s as the positive bootstrap", (type) => {
      expect(classifySetupFrame("game", frame(type, {}))).toEqual({ kind: "positive" });
    });

  it.each(["DraftStateUpdate", "DraftMatchStart"])("treats draft %s as the positive bootstrap", (type) => {
    expect(classifySetupFrame("draft", frame(type, {}))).toEqual({ kind: "positive" });
  });

  it("does not let one surface's bootstrap adopt the other's", () => {
    expect(classifySetupFrame("game", frame("DraftStateUpdate", {}))).toEqual({ kind: "other" });
    expect(classifySetupFrame("draft", frame("GameStarted", {}))).toEqual({ kind: "other" });
  });

  it("reads an Error as a refusal with its message and code", () => {
    expect(classifySetupFrame("game", frame("Error", { message: "no", code: "X" }))).toEqual({
      kind: "refusal", rejected: false, refusal: { message: "no", code: "X" },
    });
  });

  it("reads DraftActionRejected as a draft-only refusal carrying its reason", () => {
    expect(classifySetupFrame("draft", frame("DraftActionRejected", { reason: "why" }))).toEqual({
      kind: "refusal", rejected: true, refusal: { message: "why" },
    });
    expect(classifySetupFrame("game", frame("DraftActionRejected", { reason: "why" }))).toEqual({ kind: "other" });
  });

  it.each(["Pong", "OpponentReconnected", "PlayerSlotsUpdate"])("leaves %s unclassified", (type) => {
    expect(classifySetupFrame("game", frame(type, {}))).toEqual({ kind: "other" });
  });

  it("leaves malformed or non-text frames unclassified", () => {
    expect(classifySetupFrame("game", "not json")).toEqual({ kind: "other" });
    expect(classifySetupFrame("game", new Uint8Array(2))).toEqual({ kind: "other" });
    expect(classifySetupFrame("game", "{}")).toEqual({ kind: "other" });
  });
});
