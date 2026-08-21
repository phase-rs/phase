import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

function jsonResponse(data: unknown): Response {
  return new Response(JSON.stringify(data), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

describe("useCardImage", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.restoreAllMocks();
    vi.doUnmock("../../services/scryfall.ts");
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("uses imported source printing art by default when no art chain is configured", async () => {
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      if (url === "/scryfall-data.json") {
        return Promise.resolve(jsonResponse({
          "lightning bolt": {
            oracle_id: "oracle-bolt",
            face_names: ["lightning bolt"],
            faces: [{ normal: "https://img.example/default.jpg", art_crop: "https://img.example/default-art.jpg" }],
            name: "Lightning Bolt",
            mana_cost: "{R}",
            cmc: 1,
            type_line: "Instant",
            colors: ["R"],
            color_identity: ["R"],
            keywords: [],
          },
        }));
      }
      if (url === "/scryfall-printings.json") {
        return Promise.resolve(jsonResponse({
          "oracle-bolt": [
            {
              id: "dmu-bolt",
              set: "dmu",
              set_name: "Dominaria United",
              collector_number: "137",
              border_color: "black",
              frame_effects: [],
              full_art: false,
              faces: [{ normal: "https://img.example/dmu.jpg", art_crop: "https://img.example/dmu-art.jpg" }],
            },
          ],
        }));
      }
      return Promise.resolve(jsonResponse({}));
    }));

    const { usePreferencesStore } = await import("../../stores/preferencesStore");
    usePreferencesStore.getState().setArtChain([]);
    usePreferencesStore.getState().clearAllArtOverrides();

    const { useCardImage } = await import("../useCardImage");
    const { result } = renderHook(() =>
      useCardImage("Lightning Bolt", {
        sourcePrinting: { setCode: "DMU", collectorNumber: "137" },
      })
    );

    await waitFor(() => {
      expect(result.current.src).toBe("https://img.example/dmu.jpg");
    });
  });

  it("marks split-layout card images as rotated", async () => {
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      if (url === "/scryfall-data.json") {
        return Promise.resolve(jsonResponse({
          "walk-in closet": {
            oracle_id: "oracle-room",
            face_names: ["walk-in closet", "forgotten cellar"],
            faces: [
              { normal: "https://img.example/room.jpg", art_crop: "https://img.example/room-art.jpg" },
              { normal: "https://img.example/room.jpg", art_crop: "https://img.example/room-art.jpg" },
            ],
            layout: "split",
            name: "Walk-In Closet // Forgotten Cellar",
            mana_cost: "{2}{G} // {3}{G}{G}",
            cmc: 8,
            type_line: "Enchantment — Room // Enchantment — Room",
            colors: ["G"],
            color_identity: ["G"],
            keywords: [],
          },
        }));
      }
      return Promise.resolve(jsonResponse({}));
    }));

    const { useCardImage } = await import("../useCardImage");
    const { result } = renderHook(() => useCardImage("Walk-In Closet", { size: "normal" }));

    await waitFor(() => {
      expect(result.current.src).toBe("https://img.example/room.jpg");
    });
    expect(result.current.isRotated).toBe(true);
  });

  it("falls back to token search when exact token image metadata is unusable", async () => {
    const fetchTokenImageByRef = vi.fn().mockRejectedValue(new Error("missing image"));
    const fetchTokenImageUrl = vi.fn().mockResolvedValue("https://img.example/food.jpg");
    vi.doMock("../../services/scryfall.ts", () => ({
      fetchCardImageAsset: vi.fn(),
      fetchCardImageAssetByOracleId: vi.fn(),
      fetchCardImageByOracleId: vi.fn(),
      fetchCardImageUrl: vi.fn(),
      fetchTokenImageByRef,
      fetchTokenImageUrl,
      findPrintingById: vi.fn(),
      getCardPrintings: vi.fn().mockResolvedValue([]),
      isCardImageRotatedSync: vi.fn().mockReturnValue(false),
      // Report the art locale as already resolved so the hook's background
      // loader short-circuits — this test is about token fallback, not
      // localization.
      isLocaleArtReady: vi.fn().mockReturnValue(true),
      loadLocaleArt: vi.fn().mockResolvedValue(new Map()),
      resolveFaceIndexSync: vi.fn().mockReturnValue(null),
      resolveOracleIdSync: vi.fn().mockReturnValue(null),
      resolvePrintingImageUrl: vi.fn(),
    }));

    const { useCardImage } = await import("../useCardImage");
    const { result } = renderHook(() =>
      useCardImage("Food", {
        isToken: true,
        tokenImageRef: {
          scryfall_id: "food-token-id",
          scryfall_oracle_id: "food-oracle-id",
          preset_id: "food-preset-id",
        },
      }),
    );

    await waitFor(() => {
      expect(result.current.src).toBe("https://img.example/food.jpg");
    });
    expect(fetchTokenImageUrl).toHaveBeenCalledWith("Food", "normal", {
      colors: undefined,
      hasAbilities: undefined,
      power: null,
      subtypes: undefined,
      toughness: null,
    });
  });

  it("never returns the previous card's image after a rapid request change", async () => {
    let resolveFirst: ((asset: { src: string; isRotated: boolean }) => void) | undefined;
    let resolveSecond: ((asset: { src: string; isRotated: boolean }) => void) | undefined;
    const fetchCardImageAsset = vi.fn((name: string) =>
      new Promise<{ src: string; isRotated: boolean }>((resolve) => {
        if (name === "First Card") resolveFirst = resolve;
        else resolveSecond = resolve;
      })
    );
    vi.doMock("../../services/scryfall.ts", () => ({
      fetchCardImageAsset,
      fetchCardImageAssetByOracleId: vi.fn(),
      fetchTokenImageByRef: vi.fn(),
      fetchTokenImageUrl: vi.fn(),
      findPrintingById: vi.fn(),
      getCardPrintings: vi.fn().mockResolvedValue([]),
      isCardImageFlipLayoutSync: vi.fn().mockReturnValue(false),
      isCardImageRotatedSync: vi.fn().mockReturnValue(false),
      // See the note on the token-fallback mock above: the art locale is
      // reported ready so the background loader never runs here.
      isLocaleArtReady: vi.fn().mockReturnValue(true),
      loadLocaleArt: vi.fn().mockResolvedValue(new Map()),
      pickOldestPrinting: vi.fn(),
      resolveFaceIndexSync: vi.fn().mockReturnValue(null),
      resolveOracleIdSync: vi.fn().mockReturnValue(null),
      resolvePrintingImageUrl: vi.fn(),
    }));

    const { useCardImage } = await import("../useCardImage");
    const { result, rerender } = renderHook(
      ({ name }) => useCardImage(name),
      { initialProps: { name: "First Card" } },
    );

    await act(async () => {
      resolveFirst?.({ src: "first.png", isRotated: false });
    });
    expect(result.current.src).toBe("first.png");

    rerender({ name: "Second Card" });
    expect(result.current.src).toBeNull();
    expect(result.current.isLoading).toBe(true);

    await act(async () => {
      resolveSecond?.({ src: "second.png", isRotated: false });
    });
    expect(result.current.src).toBe("second.png");
  });
  it("resolves a face-down marker from tokenImageRef alone — no name, no oracle id (#7549)", async () => {
    // The #7535 marker request shape: cardName "" and only the ref naming the
    // printing. The hook must NOT short-circuit on the empty name — that
    // short-circuit is exactly what kept the merged marker feature from ever
    // loading in the live client (the component tests stubbed this hook, so
    // only a REAL-hook regression can hold the line).
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      if (url === "/scryfall-token-images.json") {
        return Promise.resolve(jsonResponse({
          "oracle:8f92f8d7-ec89-426f-86dc-fbc259eb5559:morph": {
            scryfall_id: "morph-token-dtk",
            oracle_id: "8f92f8d7-ec89-426f-86dc-fbc259eb5559",
            face_names: ["morph"],
            faces: [{ normal: "https://img.example/morph.jpg", art_crop: "https://img.example/morph-art.jpg" }],
            name: "Morph",
            layout: "token",
          },
        }));
      }
      return Promise.resolve(jsonResponse({}));
    }));

    const { useCardImage } = await import("../useCardImage");
    const { result } = renderHook(() =>
      useCardImage("", {
        size: "normal",
        isToken: true,
        tokenImageRef: {
          scryfall_id: "",
          scryfall_oracle_id: "8f92f8d7-ec89-426f-86dc-fbc259eb5559",
          face_name: "morph",
          preset_id: "face-down-morph",
        },
      }),
    );

    await waitFor(() => expect(result.current.src).toBe("https://img.example/morph.jpg"));
  });

  it("fetches nothing for a token ref that names no printing — both ids empty (#7550 review)", async () => {
    // A `TokenImageRef` is only a pointer when it carries at least one id.
    // With BOTH `scryfall_id` and `scryfall_oracle_id` empty there is nothing
    // to resolve — the request must short-circuit exactly like the empty-name
    // case, not fall through to a `fetchTokenImageUrl("")` junk search.
    const fetchTokenImageByRef = vi.fn().mockResolvedValue(null);
    const fetchTokenImageUrl = vi.fn().mockResolvedValue(null);
    vi.doMock("../../services/scryfall.ts", () => ({
      fetchCardImageAsset: vi.fn(),
      fetchCardImageAssetByOracleId: vi.fn(),
      fetchCardImageByOracleId: vi.fn(),
      fetchCardImageUrl: vi.fn(),
      fetchTokenImageByRef,
      fetchTokenImageUrl,
      findPrintingById: vi.fn(),
      getCardPrintings: vi.fn().mockResolvedValue([]),
      isCardImageRotatedSync: vi.fn().mockReturnValue(false),
      isLocaleArtReady: vi.fn().mockReturnValue(true),
      loadLocaleArt: vi.fn().mockResolvedValue(new Map()),
      resolveFaceIndexSync: vi.fn().mockReturnValue(null),
      resolveOracleIdSync: vi.fn().mockReturnValue(null),
      resolvePrintingImageUrl: vi.fn(),
    }));

    const { useCardImage } = await import("../useCardImage");
    const { result } = renderHook(() =>
      useCardImage("", {
        size: "normal",
        isToken: true,
        tokenImageRef: {
          scryfall_id: "",
          scryfall_oracle_id: "",
          preset_id: "face-down-morph",
        },
      }),
    );

    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.src).toBeNull();
    expect(fetchTokenImageByRef).not.toHaveBeenCalled();
    expect(fetchTokenImageUrl).not.toHaveBeenCalled();
  });

});
