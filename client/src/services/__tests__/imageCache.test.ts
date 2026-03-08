import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("idb-keyval", () => {
  const store = new Map<string, unknown>();
  return {
    get: vi.fn((key: string) => Promise.resolve(store.get(key) ?? undefined)),
    set: vi.fn((key: string, value: unknown) => {
      store.set(key, value);
      return Promise.resolve();
    }),
    _store: store,
  };
});

import { get, set } from "idb-keyval";
import { getCachedImage, cacheImage, revokeImageUrl } from "../imageCache.ts";

// Access internal store for cleanup
const mockStore = (
  await vi.importMock<{ _store: Map<string, unknown> }>("idb-keyval")
)._store;

describe("imageCache", () => {
  beforeEach(() => {
    mockStore.clear();
    vi.clearAllMocks();
    vi.stubGlobal("URL", {
      createObjectURL: vi.fn(() => "blob:mock-url"),
      revokeObjectURL: vi.fn(),
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("getCachedImage returns null on cache miss", async () => {
    const result = await getCachedImage("Lightning Bolt", "normal");
    expect(result).toBeNull();
    expect(get).toHaveBeenCalledWith("scryfall:Lightning Bolt:normal");
  });

  it("cacheImage stores blob and getCachedImage retrieves it", async () => {
    const blob = new Blob(["fake-image"], { type: "image/png" });
    await cacheImage("Lightning Bolt", "normal", blob);

    expect(set).toHaveBeenCalledWith(
      "scryfall:Lightning Bolt:normal",
      blob,
    );

    const result = await getCachedImage("Lightning Bolt", "normal");
    expect(result).toBe("blob:mock-url");
  });

  it("getCachedImage returns object URL from cached blob", async () => {
    const blob = new Blob(["img-data"], { type: "image/jpeg" });
    mockStore.set("scryfall:Counterspell:large", blob);

    const result = await getCachedImage("Counterspell", "large");
    expect(result).toBe("blob:mock-url");
    expect(URL.createObjectURL).toHaveBeenCalledWith(blob);
  });

  it("revokeImageUrl calls URL.revokeObjectURL", () => {
    revokeImageUrl("blob:some-url");
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:some-url");
  });
});
