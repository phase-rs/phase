import { beforeEach, describe, expect, it, vi } from "vitest";

const warmCardDatabase = vi.fn();
vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ warmCardDatabase }),
}));

import { useCardDataStore } from "../cardDataStore";

describe("cardDataStore.warm", () => {
  beforeEach(() => {
    warmCardDatabase.mockReset();
    useCardDataStore.setState({ status: "idle" });
  });

  it("transitions idle → loading → ready and loads exactly once", async () => {
    warmCardDatabase.mockResolvedValue(undefined);
    const inflight = useCardDataStore.getState().warm();
    expect(useCardDataStore.getState().status).toBe("loading");
    await inflight;
    expect(useCardDataStore.getState().status).toBe("ready");
    expect(warmCardDatabase).toHaveBeenCalledOnce();
  });

  it("is idempotent once ready (no second load)", async () => {
    warmCardDatabase.mockResolvedValue(undefined);
    await useCardDataStore.getState().warm();
    await useCardDataStore.getState().warm();
    expect(warmCardDatabase).toHaveBeenCalledOnce();
  });

  it("dedupes concurrent callers into one load", async () => {
    warmCardDatabase.mockResolvedValue(undefined);
    await Promise.all([
      useCardDataStore.getState().warm(),
      useCardDataStore.getState().warm(),
    ]);
    expect(warmCardDatabase).toHaveBeenCalledOnce();
  });

  it("sets error status when the warm fails (does not throw)", async () => {
    warmCardDatabase.mockRejectedValue(new Error("boom"));
    await expect(useCardDataStore.getState().warm()).resolves.toBeUndefined();
    expect(useCardDataStore.getState().status).toBe("error");
  });

  it("retries after an error and can reach ready", async () => {
    warmCardDatabase
      .mockRejectedValueOnce(new Error("boom"))
      .mockResolvedValue(undefined);
    await useCardDataStore.getState().warm();
    expect(useCardDataStore.getState().status).toBe("error");
    await useCardDataStore.getState().warm();
    expect(useCardDataStore.getState().status).toBe("ready");
    expect(warmCardDatabase).toHaveBeenCalledTimes(2);
  });
});
