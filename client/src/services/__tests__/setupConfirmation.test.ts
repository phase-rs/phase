import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SETUP_CONFIRMATION_TIMEOUT_MS, SetupConfirmation } from "../setupConfirmation";

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
