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
    vi.doUnmock("../../services/visualPacks/repository.ts");
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("uses imported source printing art by default when no art chain is configured", async () => {
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: {
        currentRevision: () => "0",
        subscribe: () => () => {},
        resolve: vi.fn(async ({ remote }: { remote: { src: string } }) => ({
          revision: "0",
          sources: [{ kind: "remote" as const, src: remote.src }, { kind: "fallback" as const, src: null }],
        })),
      },
    }));
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
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: {
        currentRevision: () => "0",
        subscribe: () => () => {},
        resolve: vi.fn(async ({ remote }: { remote: { src: string } }) => ({
          revision: "0",
          sources: [{ kind: "remote" as const, src: remote.src }, { kind: "fallback" as const, src: null }],
        })),
      },
    }));
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
    const fetchTokenImageAssetByRef = vi.fn().mockRejectedValue(new Error("missing image"));
    const fetchTokenImageUrl = vi.fn().mockResolvedValue("https://img.example/food.jpg");
    vi.doMock("../../services/scryfall.ts", () => ({
      deriveImageUrl: (url: string) => url,
      fetchCardImageAsset: vi.fn(),
      fetchCardImageAssetByOracleId: vi.fn(),
      fetchCardImageByOracleId: vi.fn(),
      fetchCardImageUrl: vi.fn(),
      fetchTokenImageAssetByRef,
      fetchTokenImageUrl,
      findPrintingById: vi.fn(),
      getCardPrintings: vi.fn().mockResolvedValue([]),
      imageUrlSize: vi.fn().mockReturnValue(null),
      isCardImageFlipLayoutSync: vi.fn().mockReturnValue(false),
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
    type TestAsset = {
      src: string;
      isRotated: boolean;
      source: { kind: "remote"; src: string };
      semantic: { faceIndex: number };
    };
    let resolveFirst: ((asset: TestAsset) => void) | undefined;
    let resolveSecond: ((asset: TestAsset) => void) | undefined;
    const fetchCardImageAsset = vi.fn((name: string) =>
      new Promise<TestAsset>((resolve) => {
        if (name === "First Card") resolveFirst = resolve;
        else resolveSecond = resolve;
      })
    );
    vi.doMock("../../services/scryfall.ts", () => ({
      deriveImageUrl: (url: string) => url,
      fetchCardImageAsset,
      fetchCardImageAssetByOracleId: vi.fn(),
      fetchTokenImageAssetByRef: vi.fn(),
      fetchTokenImageUrl: vi.fn(),
      findPrintingById: vi.fn(),
      getCardPrintings: vi.fn().mockResolvedValue([]),
      imageUrlSize: vi.fn().mockReturnValue(null),
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
      resolveFirst?.({
        src: "first.png",
        isRotated: false,
        source: { kind: "remote", src: "first.png" },
        semantic: { faceIndex: 0 },
      });
    });
    expect(result.current.src).toBe("first.png");

    rerender({ name: "Second Card" });
    expect(result.current.src).toBeNull();
    expect(result.current.isLoading).toBe(true);

    await act(async () => {
      resolveSecond?.({
        src: "second.png",
        isRotated: false,
        source: { kind: "remote", src: "second.png" },
        semantic: { faceIndex: 0 },
      });
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
    const fetchTokenImageAssetByRef = vi.fn().mockResolvedValue(null);
    const fetchTokenImageUrl = vi.fn().mockResolvedValue(null);
    vi.doMock("../../services/scryfall.ts", () => ({
      deriveImageUrl: (url: string) => url,
      fetchCardImageAsset: vi.fn(),
      fetchCardImageAssetByOracleId: vi.fn(),
      fetchCardImageByOracleId: vi.fn(),
      fetchCardImageUrl: vi.fn(),
      fetchTokenImageAssetByRef,
      fetchTokenImageUrl,
      findPrintingById: vi.fn(),
      getCardPrintings: vi.fn().mockResolvedValue([]),
      imageUrlSize: vi.fn().mockReturnValue(null),
      isCardImageFlipLayoutSync: vi.fn().mockReturnValue(false),
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
    expect(fetchTokenImageAssetByRef).not.toHaveBeenCalled();
    expect(fetchTokenImageUrl).not.toHaveBeenCalled();
  });

  it("uses the token catalog's resolved DFC face index for installed candidates", async () => {
    const resolve = vi.fn(async ({ remote }: {
      groups: Array<{ requested: string[] }>;
      remote: { src: string };
    }) => ({
      revision: "0",
      sources: [{ kind: "remote" as const, src: remote.src }, { kind: "fallback" as const, src: null }],
    }));
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: {
        currentRevision: () => "0",
        subscribe: () => () => {},
        resolve,
      },
    }));
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      if (url === "/scryfall-token-images.json") {
        return Promise.resolve(jsonResponse({
          "scryfall:44444444-4444-4444-8444-444444444444": {
            scryfall_id: "44444444-4444-4444-8444-444444444444",
            oracle_id: "55555555-5555-4555-8555-555555555555",
            face_names: ["front token", "back token"],
            faces: [
              { normal: "https://img.example/front.jpg", art_crop: "https://img.example/front-art.jpg" },
              { normal: "https://img.example/back.jpg", art_crop: "https://img.example/back-art.jpg" },
            ],
            layout: "transform",
            name: "Front Token // Back Token",
            mana_cost: "",
            cmc: 0,
            type_line: "Token Creature",
            colors: [],
            color_identity: [],
            keywords: [],
          },
        }));
      }
      return Promise.resolve(jsonResponse({}));
    }));

    const { decodeCandidateKey } = await import("../../services/visualPacks/candidateKeys.ts");
    const { useCardImage } = await import("../useCardImage");
    const { result } = renderHook(() => useCardImage("Back Token", {
      faceIndex: 0,
      isToken: true,
      tokenImageRef: {
        scryfall_id: "44444444-4444-4444-8444-444444444444",
        scryfall_oracle_id: "55555555-5555-4555-8555-555555555555",
        face_name: "back token",
        preset_id: "two-face-fixture",
      },
    }));

    await waitFor(() => expect(result.current.src).toBe("https://img.example/back.jpg"));
    const request = resolve.mock.calls[0][0];
    const [, tuple] = decodeCandidateKey(request.groups[0].requested[0]);
    expect(tuple[1]).toBe(1);
  });

  it("omits installed token art crops and keeps the remote crop fallback", async () => {
    const resolve = vi.fn(async ({ groups, remote }: { groups: unknown[]; remote: { src: string } }) => ({
      revision: "0",
      sources: groups.length === 0
        ? [{ kind: "remote" as const, src: remote.src }, { kind: "fallback" as const, src: null }]
        : [{ kind: "installed" as const, src: "must-not-be-used" }],
    }));
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: {
        currentRevision: () => "0",
        subscribe: () => () => {},
        resolve,
      },
    }));
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      if (url === "/scryfall-token-images.json") {
        return Promise.resolve(jsonResponse({
          "scryfall:44444444-4444-4444-8444-444444444444": {
            scryfall_id: "44444444-4444-4444-8444-444444444444",
            oracle_id: "55555555-5555-4555-8555-555555555555",
            face_names: ["token"],
            faces: [{ normal: "https://img.example/token.jpg", art_crop: "https://img.example/token-art.jpg" }],
            layout: "token",
            name: "Token",
            mana_cost: "",
            cmc: 0,
            type_line: "Token Creature",
            colors: [],
            color_identity: [],
            keywords: [],
          },
        }));
      }
      return Promise.resolve(jsonResponse({}));
    }));

    const { useCardImage } = await import("../useCardImage");
    const { result } = renderHook(() => useCardImage("Token", {
      isToken: true,
      size: "art_crop",
      tokenImageRef: {
        scryfall_id: "44444444-4444-4444-8444-444444444444",
        scryfall_oracle_id: "55555555-5555-4555-8555-555555555555",
        face_name: "token",
        preset_id: "fixture",
      },
    }));

    await waitFor(() => expect(result.current.src).toBe("https://img.example/token-art.jpg"));
    expect(resolve).toHaveBeenCalledWith(expect.objectContaining({ groups: [] }));
  });

  it("prefers installed candidates, advances exactly once, and invalidates on revision", async () => {
    let revision = "0";
    let revisionListener: (() => void) | undefined;
    const resolve = vi.fn(async ({ remote }: { remote: { src: string } }) => ({
      revision,
      sources: [
        {
          kind: "installed" as const,
          src: `http://visual-pack.localhost/installed-${revision}`,
          rungs: {
            small: "http://visual-pack.localhost/small",
            normal: "http://visual-pack.localhost/normal",
          },
          assetKey: "asset:v1:canonical_card:QQ",
          packId: "core",
          catalogRoot: "a".repeat(64),
        },
        { kind: "remote" as const, src: remote.src },
        { kind: "fallback" as const, src: null },
      ],
    }));
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: {
        currentRevision: () => revision,
        subscribe: (listener: () => void) => {
          revisionListener = listener;
          return () => {};
        },
        resolve,
      },
    }));
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      if (url === "/scryfall-data.json") {
        return Promise.resolve(jsonResponse({
          "offline card": {
            oracle_id: "11111111-1111-4111-8111-111111111111",
            face_names: ["offline card"],
            faces: [{ normal: "https://cards.scryfall.io/normal/front/a/a/offline.jpg", art_crop: "https://cards.scryfall.io/art_crop/front/a/a/offline.jpg" }],
            name: "Offline Card",
            mana_cost: "{1}",
            cmc: 1,
            type_line: "Artifact",
            colors: [],
            color_identity: [],
            keywords: [],
          },
        }));
      }
      return Promise.resolve(jsonResponse({}));
    }));

    const { useCardImage } = await import("../useCardImage");
    const { result } = renderHook(() => useCardImage("Offline Card"));
    await waitFor(() => expect(result.current.src).toBe("http://visual-pack.localhost/installed-0"));
    expect(result.current.rungs).toEqual({
      small: "http://visual-pack.localhost/small",
      normal: "http://visual-pack.localhost/normal",
    });

    act(() => result.current.advanceFailedSource?.("http://visual-pack.localhost/installed-0"));
    expect(result.current.src).toBe("https://cards.scryfall.io/normal/front/a/a/offline.jpg");
    act(() => result.current.advanceFailedSource?.("http://visual-pack.localhost/installed-0"));
    expect(result.current.src).toBe("https://cards.scryfall.io/normal/front/a/a/offline.jpg");
    act(() => result.current.advanceFailedSource?.("https://cards.scryfall.io/normal/front/a/a/offline.jpg"));
    expect(result.current.src).toBeNull();

    const resolvesBeforeRevision = resolve.mock.calls.length;
    revision = "2";
    act(() => revisionListener?.());
    await waitFor(() => expect(result.current.src).toBe("http://visual-pack.localhost/installed-2"));
    expect(resolve.mock.calls.length).toBeGreaterThan(resolvesBeforeRevision);
  });

  it("resolves the fixed back without face identity and advances installed to remote to null", async () => {
    let revision = "0";
    let revisionListener: (() => void) | undefined;
    const resolve = vi.fn(async ({ groups, rung, remote }) => ({
      revision,
      sources: [
        {
          kind: "installed" as const,
          src: `http://visual-pack.localhost/back-${revision}`,
          assetKey: "asset:v1:card_back:W10",
          packId: "core",
          catalogRoot: "a".repeat(64),
        },
        { kind: "remote" as const, src: remote.src },
        { kind: "fallback" as const, src: null },
      ],
      request: { groups, rung, remote },
    }));
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: {
        currentRevision: () => revision,
        subscribe: (listener: () => void) => {
          revisionListener = listener;
          return () => {};
        },
        resolve,
      },
    }));
    const forbiddenFetch = vi.fn(() => Promise.reject(new Error("network forbidden")));
    vi.stubGlobal("fetch", forbiddenFetch);

    const { useCardBackImage } = await import("../useCardImage");
    const { result } = renderHook(() => useCardBackImage());

    await waitFor(() => expect(result.current.src).toBe("http://visual-pack.localhost/back-0"));
    expect(forbiddenFetch).not.toHaveBeenCalled();
    expect(resolve).toHaveBeenCalledWith({
      groups: [{
        requested: ["candidate:v1:card_back:WyJjYXJkX2JhY2siLFtdXQo"],
      }],
      rung: "normal",
      remote: {
        src: "https://backs.scryfall.io/normal/0/a/0aeebaf5-8c7d-4636-9e82-8c27447861f7.jpg",
      },
    });

    act(() => result.current.advanceFailedSource?.("stale-installed"));
    expect(result.current.src).toBe("http://visual-pack.localhost/back-0");
    act(() => result.current.advanceFailedSource?.("http://visual-pack.localhost/back-0"));
    const remote = result.current.src;
    expect(remote).toMatch(/^https:\/\/backs\.scryfall\.io\//);
    act(() => result.current.advanceFailedSource?.("http://visual-pack.localhost/back-0"));
    expect(result.current.src).toBe(remote);
    act(() => result.current.advanceFailedSource?.(remote!));
    expect(result.current.src).toBeNull();

    revision = "1";
    act(() => revisionListener?.());
    await waitFor(() => expect(result.current.src).toBe("http://visual-pack.localhost/back-1"));
  });

  // Regression anchor for the art-selection chain. Before these, the suite
  // configured only an EMPTY `artChain`, so `applyChain`/`applyChainEntry`
  // could be changed arbitrarily without a single test turning red. They must
  // stay at the `useCardImage` level: an extracted-function unit test asserts
  // the extracted function against itself and cannot show that the hook still
  // reaches it.
  describe("art precedence", () => {
    // `dmu` is printings[0] (what a `newest` entry would pick) and `sld` is the
    // only borderless one, so every expectation below distinguishes the entry
    // under test from its neighbours. The scryfall-data face URL is a third,
    // distinct value, so "the chain did nothing" is also distinguishable.
    const PRINTINGS = [
      {
        id: "dmu-bolt",
        set: "dmu",
        set_name: "Dominaria United",
        collector_number: "137",
        released_at: "2022-09-09",
        border_color: "black",
        frame_effects: [],
        full_art: false,
        faces: [{ normal: "https://img.example/dmu.jpg", art_crop: "https://img.example/dmu-art.jpg" }],
      },
      {
        id: "sld-bolt",
        set: "sld",
        set_name: "Secret Lair Drop",
        collector_number: "1",
        released_at: "2023-04-01",
        border_color: "borderless",
        frame_effects: [],
        full_art: false,
        faces: [{ normal: "https://img.example/sld.jpg", art_crop: "https://img.example/sld-art.jpg" }],
      },
    ];

    function stubPrintingsFixture(): void {
      vi.doMock("../../services/visualPacks/repository.ts", () => ({
        visualPackRepository: {
          currentRevision: () => "0",
          subscribe: () => () => {},
          resolve: vi.fn(async ({ remote }: { remote: { src: string } }) => ({
            revision: "0",
            sources: [{ kind: "remote" as const, src: remote.src }, { kind: "fallback" as const, src: null }],
          })),
        },
      }));
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
          return Promise.resolve(jsonResponse({ "oracle-bolt": PRINTINGS }));
        }
        return Promise.resolve(jsonResponse({}));
      }));
    }

    it("walks past a chain entry that matches no printing and applies the next one", async () => {
      stubPrintingsFixture();

      const { usePreferencesStore } = await import("../../stores/preferencesStore");
      usePreferencesStore.getState().clearAllArtOverrides();
      usePreferencesStore.getState().setArtChain([
        { type: "set", setCode: "zzz", label: "Nonexistent Set" },
        { type: "prefer_borderless" },
      ]);

      const { useCardImage } = await import("../useCardImage");
      const { result } = renderHook(() => useCardImage("Lightning Bolt"));

      await waitFor(() => expect(result.current.src).toBe("https://img.example/sld.jpg"));
    });

    it("honors a set entry ahead of the rest of the chain", async () => {
      stubPrintingsFixture();

      const { usePreferencesStore } = await import("../../stores/preferencesStore");
      usePreferencesStore.getState().clearAllArtOverrides();
      usePreferencesStore.getState().setArtChain([
        { type: "set", setCode: "dmu", label: "Dominaria United" },
        { type: "prefer_borderless" },
      ]);

      const { useCardImage } = await import("../useCardImage");
      const { result } = renderHook(() => useCardImage("Lightning Bolt"));

      await waitFor(() => expect(result.current.src).toBe("https://img.example/dmu.jpg"));
    });

    it("resolves a source_printing chain entry from the deck's printing", async () => {
      stubPrintingsFixture();

      const { usePreferencesStore } = await import("../../stores/preferencesStore");
      usePreferencesStore.getState().clearAllArtOverrides();
      // `newest` sits behind `source_printing` and would pick `dmu`, so an sld
      // result can only come from the source-printing entry consuming the
      // caller's `sourcePrinting`.
      usePreferencesStore.getState().setArtChain([
        { type: "source_printing" },
        { type: "newest" },
      ]);

      const { useCardImage } = await import("../useCardImage");
      const { result } = renderHook(() =>
        useCardImage("Lightning Bolt", {
          sourcePrinting: { setCode: "SLD", collectorNumber: "1" },
        })
      );

      await waitFor(() => expect(result.current.src).toBe("https://img.example/sld.jpg"));
    });

    // Precedence branch 4: with an EMPTY chain, a `sourcePrinting` still wins
    // over the canonical scryfall-data art. This is the default configuration,
    // so any planner that models only the chain disagrees with the renderer
    // for every default-config user.
    it("uses the source printing when the chain is empty", async () => {
      stubPrintingsFixture();

      const { usePreferencesStore } = await import("../../stores/preferencesStore");
      usePreferencesStore.getState().clearAllArtOverrides();
      usePreferencesStore.getState().setArtChain([]);

      const { useCardImage } = await import("../useCardImage");
      const { result } = renderHook(() =>
        useCardImage("Lightning Bolt", {
          sourcePrinting: { setCode: "SLD", collectorNumber: "1" },
        })
      );

      await waitFor(() => expect(result.current.src).toBe("https://img.example/sld.jpg"));
    });

    // Precedence branch 2: a per-card override outranks the whole chain.
    it("prefers an art override over the chain", async () => {
      stubPrintingsFixture();

      const { usePreferencesStore } = await import("../../stores/preferencesStore");
      usePreferencesStore.getState().clearAllArtOverrides();
      usePreferencesStore.getState().setArtChain([{ type: "prefer_borderless" }]);
      usePreferencesStore.getState().setArtOverride("oracle-bolt", {
        scryfallId: "dmu-bolt",
        setCode: "dmu",
        collectorNumber: "137",
      });

      const { useCardImage } = await import("../useCardImage");
      const { result } = renderHook(() => useCardImage("Lightning Bolt"));

      await waitFor(() => expect(result.current.src).toBe("https://img.example/dmu.jpg"));
      usePreferencesStore.getState().clearAllArtOverrides();
    });

    // Pins the renderer's behavior that `services/artSelection.ts` must model:
    // `else if (artOverrides[oracleId])` gates on key PRESENCE, so a pin whose
    // scryfallId is no longer in the printings data consumes the branch and
    // yields no override URL — but with an EMPTY chain the async path is still
    // handed the source printing, so the deck's art is what renders. Neither
    // the canonical art nor nothing.
    it("still shows the deck's printing when a stale art override resolves to no printing", async () => {
      stubPrintingsFixture();

      const { usePreferencesStore } = await import("../../stores/preferencesStore");
      usePreferencesStore.getState().clearAllArtOverrides();
      usePreferencesStore.getState().setArtChain([]);
      usePreferencesStore.getState().setArtOverride("oracle-bolt", {
        scryfallId: "printing-that-no-longer-exists",
        setCode: "xxx",
        collectorNumber: "1",
      });

      const { useCardImage } = await import("../useCardImage");
      const { result } = renderHook(() =>
        useCardImage("Lightning Bolt", {
          sourcePrinting: { setCode: "SLD", collectorNumber: "1" },
        })
      );

      await waitFor(() => expect(result.current.src).toBe("https://img.example/sld.jpg"));
      usePreferencesStore.getState().clearAllArtOverrides();
    });
  });
});
