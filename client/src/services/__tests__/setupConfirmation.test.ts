import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SETUP_BUFFER_MAX_TEXT_BYTES,
  SETUP_CONFIRMATION_TIMEOUT_MS,
  SetupBufferBudget,
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

  it("defers once, at the bound, to a decode in flight; a frame it delivers confirms", async () => {
    let finishDecode!: () => void;
    const decoding = new Promise<void>((resolve) => { finishDecode = resolve; });
    const confirmation = new SetupConfirmation(SETUP_CONFIRMATION_TIMEOUT_MS, () => decoding);
    await vi.advanceTimersByTimeAsync(SETUP_CONFIRMATION_TIMEOUT_MS + 30_000);
    expect(confirmation.current).toBeNull();
    finishDecode();
    confirmation.confirm();
    await expect(confirmation.settled).resolves.toBe("confirmed");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("bounds its one deferral itself when the decode it deferred to never settles", async () => {
    const confirmation = new SetupConfirmation(
      SETUP_CONFIRMATION_TIMEOUT_MS, () => new Promise<void>(() => undefined),
    );
    await vi.advanceTimersByTimeAsync(2 * SETUP_CONFIRMATION_TIMEOUT_MS - 1);
    expect(confirmation.current).toBeNull();
    await vi.advanceTimersByTimeAsync(1);
    await expect(confirmation.settled).resolves.toBe("timeout");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("times out once the decode it deferred to has answered without settling it", async () => {
    let finishDecode!: () => void;
    let calls = 0;
    const decoding = new Promise<void>((resolve) => { finishDecode = resolve; });
    const confirmation = new SetupConfirmation(SETUP_CONFIRMATION_TIMEOUT_MS, () => {
      calls++;
      return decoding;
    });
    await vi.advanceTimersByTimeAsync(SETUP_CONFIRMATION_TIMEOUT_MS);
    expect(confirmation.current).toBeNull();
    finishDecode();
    await vi.advanceTimersByTimeAsync(0);
    await expect(confirmation.settled).resolves.toBe("timeout");
    // A single deferral, not one per decode.
    expect(calls).toBe(1);
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

  it.each(["GameStarted", "GameCreated", "SessionAttached"])(
    "treats game %s as the positive bootstrap", (type) => {
      expect(classifySetupFrame("game", frame(type, {}))).toEqual({ kind: "positive", message: { type, data: {} } });
    });

  it("treats a StateWithheld only of the GameStarted as the positive bootstrap", () => {
    const withheld = (inner: unknown) => frame("StateWithheld", { state_revision: 2, withheld: inner, reason: "" });
    expect(classifySetupFrame("game", withheld({ type: "GameStarted", data: { your_player: 1 } })))
      .toMatchObject({ kind: "positive", message: { type: "StateWithheld" } });
    expect(classifySetupFrame("game", withheld({ type: "StateUpdate" }))).toEqual({ kind: "other" });
    expect(classifySetupFrame("game", frame("StateWithheld", {}))).toEqual({ kind: "other" });
  });

  it.each(["DraftStateUpdate", "DraftMatchStart"])("treats draft %s as the positive bootstrap", (type) => {
    expect(classifySetupFrame("draft", frame(type, {}))).toEqual({ kind: "positive", message: { type, data: {} } });
  });

  it("does not let one surface's bootstrap adopt the other's", () => {
    expect(classifySetupFrame("game", frame("DraftStateUpdate", {}))).toEqual({ kind: "other" });
    expect(classifySetupFrame("draft", frame("GameStarted", {}))).toEqual({ kind: "other" });
  });

  it.each(["game", "draft"] as const)("reads a %s Error as a refusal with its message and code", (surface) => {
    expect(classifySetupFrame(surface, frame("Error", { message: "no", code: "X" }))).toEqual({
      kind: "refusal",
      refusal: { message: "no", code: "X" },
      message: { type: "Error", data: { message: "no", code: "X" } },
    });
  });

  it("reads DraftActionRejected as the draft server's answer, not a refusal", () => {
    expect(classifySetupFrame("draft", frame("DraftActionRejected", { reason: "why" }))).toEqual({
      kind: "answered", message: { type: "DraftActionRejected", data: { reason: "why" } },
    });
    expect(classifySetupFrame("game", frame("DraftActionRejected", { reason: "why" }))).toEqual({ kind: "other" });
  });

  it.each(["Pong", "OpponentReconnected", "PlayerSlotsUpdate"])("leaves %s unclassified", (type) => {
    expect(classifySetupFrame("game", frame(type, {}))).toEqual({ kind: "other" });
  });

  it.each([
    ["game", '{"type":"GameStarted","data":{"state":'],
    ["game", '{"type":"Error","data":{"message":'],
    ["draft", '{"type":"DraftStateUpdate","data":{"view":'],
    ["draft", '{"type":"DraftActionRejected","data":'],
  ] as const)("leaves a truncated canonical %s frame unclassified: %s", (surface, text) => {
    expect(classifySetupFrame(surface, text)).toEqual({ kind: "other" });
  });

  it("leaves malformed or non-text frames unclassified", () => {
    expect(classifySetupFrame("game", "not json")).toEqual({ kind: "other" });
    expect(classifySetupFrame("game", new Uint8Array(2))).toEqual({ kind: "other" });
    expect(classifySetupFrame("game", "{}")).toEqual({ kind: "other" });
  });
});

describe("SetupConfirmation.observe", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  const frame = (type: string, data?: unknown) => JSON.stringify({ type, data });

  it("settles refused on an Error and keeps its reason", async () => {
    const confirmation = new SetupConfirmation();
    confirmation.observe("game", frame("Error", { message: "gone" }));
    await expect(confirmation.settled).resolves.toBe("refused");
    expect(confirmation.refusal).toEqual({ message: "gone" });
  });

  it("confirms on a draft DraftActionRejected, the server's answer on the candidate", async () => {
    const confirmation = new SetupConfirmation();
    confirmation.observe("draft", frame("DraftActionRejected", { reason: "Reconnect grace period expired" }));
    await expect(confirmation.settled).resolves.toBe("confirmed");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("classifies nothing once settled", async () => {
    const confirmation = new SetupConfirmation();
    confirmation.observe("game", frame("GameStarted", { state: {} }));
    await expect(confirmation.settled).resolves.toBe("confirmed");
    const parse = vi.spyOn(JSON, "parse");
    try {
      confirmation.observe("game", frame("Error", { message: "late" }));
      expect(parse).not.toHaveBeenCalled();
      expect(confirmation.refusal).toBeNull();
    } finally {
      parse.mockRestore();
    }
  });

  it("reads an unrelated canonical frame's tag without parsing it, and a bootstrap once", () => {
    const parse = vi.spyOn(JSON, "parse");
    try {
      expect(classifySetupFrame("game", frame("StateUpdate", { state: { objects: [] } }))).toEqual({ kind: "other" });
      expect(parse).not.toHaveBeenCalled();
      expect(classifySetupFrame("game", frame("GameStarted", { state: { objects: [] } })))
        .toMatchObject({ kind: "positive" });
      expect(parse).toHaveBeenCalledOnce();
    } finally {
      parse.mockRestore();
    }
  });

  it("hands the envelope it parsed to the caller, and nothing for a frame it did not settle on", () => {
    const confirmation = new SetupConfirmation();
    expect(confirmation.observe("game", frame("Pong", { timestamp: 1 }))).toBeUndefined();
    expect(confirmation.observe("game", frame("GameStarted", { state: {} })))
      .toEqual({ type: "GameStarted", data: { state: {} } });
    // Settled: later frames are left to the adopted socket's own parse.
    expect(confirmation.observe("game", frame("GameStarted", { state: {} }))).toBeUndefined();
  });

  it("ignores frames that are neither positive nor a refusal", () => {
    const confirmation = new SetupConfirmation();
    confirmation.observe("game", frame("Pong", { timestamp: 1 }));
    expect(confirmation.current).toBeNull();
    confirmation.dispose();
  });
});

describe("SetupBufferBudget", () => {
  const large = (type: string) => `{"type":"${type}","data":{"padding":"${"x".repeat(SETUP_BUFFER_MAX_TEXT_BYTES / 2)}"}}`;

  it.each([
    ["game", "GameStarted"], ["draft", "GameStarted"], ["draft", "DraftStateUpdate"], ["draft", "DraftJoined"],
  ] as const)("does not charge the %s surface's first %s, wherever it sits behind a prefix", (surface, type) => {
    const budget = new SetupBufferBudget(surface);
    expect(budget.admit('{"type":"SessionAttached","data":{}}')).toBe(true);
    expect(budget.admit('{"type":"PlayerSlotsUpdate","data":{"slots":[]}}')).toBe(true);
    expect(budget.admit(large(type))).toBe(true);
    // Later backlog is still admitted against an untouched byte budget.
    expect(budget.admit('{"type":"Pong","data":{}}')).toBe(true);
  });

  it("charges a repeated bootstrap tag and any other large frame as backlog", () => {
    const repeated = new SetupBufferBudget("game");
    expect(repeated.admit(large("GameStarted"))).toBe(true);
    expect(repeated.admit(large("GameStarted"))).toBe(false);
    expect(new SetupBufferBudget("game").admit(large("StateUpdate"))).toBe(false);
    expect(new SetupBufferBudget("game").admit(large("DraftStateUpdate"))).toBe(false);
  });
});
