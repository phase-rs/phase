import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Regression tests for #11008: art that failed to load stayed failed for the
// life of the page. The hook now offers a reload that only the player triggers.

const SCULPTOR = "oracle-etherium-sculptor";
const SCULPTOR_ART = "https://img.example/oracle-etherium-sculptor.jpg";
const MITE = "oracle-haywire-mite";
const MITE_ART = "https://img.example/oracle-haywire-mite.jpg";

/** Real hook over a remote-only resolution: no installed art, and every oracle
 *  id resolves to a remote image named after it. */
async function setup() {
  const fetchCardImageAssetByOracleId = vi.fn(async (oracleId: string) => ({
    src: `https://img.example/${oracleId}.jpg`,
    isRotated: false,
    semantic: { oracleId, faceIndex: 0, alias: oracleId },
  }));
  vi.doMock("../../services/scryfall.ts", () => ({
    deriveImageUrl: (url: string) => url,
    fetchCardImageAsset: vi.fn(),
    fetchCardImageAssetByOracleId,
    fetchTokenImageAssetByRef: vi.fn(),
    // Kibo's Banana (#6156): a token with no paper printing finds no art.
    fetchTokenImageUrl: vi.fn().mockRejectedValue(new Error("no printing")),
    findPrintingById: vi.fn(),
    getCardPrintings: vi.fn().mockResolvedValue([]),
    imageUrlSize: vi.fn(() => null),
    isCardImageFlipLayoutSync: vi.fn(() => false),
    isCardImageRotatedSync: vi.fn(() => false),
    isLocaleArtReady: vi.fn(() => true),
    loadLocaleArt: vi.fn(),
    loadScryfallData: vi.fn().mockResolvedValue(null),
    resolveFaceIndexSync: vi.fn(() => null),
    resolveOracleIdSync: vi.fn(() => null),
    resolvePrintingImageUrl: vi.fn(),
  }));
  vi.doMock("../../services/visualPacks/repository.ts", () => ({
    visualPackRepository: {
      currentRevision: () => "0",
      subscribe: () => () => {},
      resolve: vi.fn(async ({ allowRemote, remote }: { allowRemote: boolean; remote?: { src: string } }) => ({
        revision: "0",
        sources: allowRemote && remote
          ? [{ kind: "remote" as const, src: remote.src }, { kind: "fallback" as const, src: null }]
          : [{ kind: "fallback" as const, src: null }],
      })),
    },
  }));
  const { useCardImage } = await import("../useCardImage.ts");
  const { useConnectivityStore } = await import("../../stores/connectivityStore.ts");
  useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  return { useCardImage, useConnectivityStore, fetchCardImageAssetByOracleId };
}

/** Lets every pending promise chain in the hook run to completion. */
async function settle() {
  await act(async () => {
    for (let index = 0; index < 20; index += 1) await Promise.resolve();
  });
}

describe("useCardImage art reload (#11008)", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.restoreAllMocks();
    vi.doUnmock("../../services/scryfall.ts");
    vi.doUnmock("../../services/visualPacks/repository.ts");
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("keeps failed art failed until the reload, which loads it again", async () => {
    const env = await setup();
    const rendered: Array<string | null> = [];
    const mount = () => renderHook(() => {
      const image = env.useCardImage("Etherium Sculptor", { oracleId: SCULPTOR, size: "art_crop" });
      rendered.push(image.src);
      return image;
    });

    const first = mount();
    await waitFor(() => expect(first.result.current.src).toBe(SCULPTOR_ART));
    // Fake timers from the failure on, so a retry it scheduled would run below.
    vi.useFakeTimers();
    act(() => first.result.current.advanceFailedSource?.(SCULPTOR_ART));
    expect(first.result.current).toMatchObject({ src: null, isLoading: false });
    expect(first.result.current.reload).toBeTypeOf("function");

    // No automatic retry: ten minutes passing, regaining focus and a remount
    // all keep the failure, and the failed image is never offered again.
    rendered.length = 0;
    act(() => {
      vi.advanceTimersByTime(10 * 60_000);
      window.dispatchEvent(new Event("focus"));
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await settle();
    vi.useRealTimers();
    first.unmount();
    const second = mount();
    await settle();
    expect(second.result.current).toMatchObject({ src: null, isLoading: false });
    expect(rendered).not.toContain(SCULPTOR_ART);

    // The player's tap clears the cached failure and requests the art again.
    const lookups = env.fetchCardImageAssetByOracleId.mock.calls.length;
    act(() => second.result.current.reload?.());
    await waitFor(() => expect(second.result.current.src).toBe(SCULPTOR_ART));
    expect(env.fetchCardImageAssetByOracleId.mock.calls.length).toBeGreaterThan(lookups);
  });

  it("reloads only the card that was tapped", async () => {
    const env = await setup();
    const sculptor = renderHook(() => env.useCardImage("Etherium Sculptor", { oracleId: SCULPTOR }));
    const mite = renderHook(() => env.useCardImage("Haywire Mite", { oracleId: MITE }));
    await waitFor(() => expect(sculptor.result.current.src).toBe(SCULPTOR_ART));
    await waitFor(() => expect(mite.result.current.src).toBe(MITE_ART));
    act(() => sculptor.result.current.advanceFailedSource?.(SCULPTOR_ART));
    act(() => mite.result.current.advanceFailedSource?.(MITE_ART));
    expect(sculptor.result.current.src).toBeNull();
    expect(mite.result.current.src).toBeNull();

    act(() => sculptor.result.current.reload?.());
    await waitFor(() => expect(sculptor.result.current.src).toBe(SCULPTOR_ART));
    await settle();
    expect(mite.result.current).toMatchObject({ src: null, isLoading: false });
  });

  it("offers no reload while offline, where the lookup never goes remote", async () => {
    const env = await setup();
    env.useConnectivityStore.setState({ forcedOffline: true });
    const { result } = renderHook(() => env.useCardImage("Etherium Sculptor", { oracleId: SCULPTOR }));

    // Reach guard: the offline lookup settles on the artless tile.
    await waitFor(() => expect(result.current).toMatchObject({ src: null, isLoading: false }));
    expect(result.current.reload).toBeUndefined();

    // Online, the same card offers the reload once its art has failed.
    act(() => env.useConnectivityStore.setState({ forcedOffline: false }));
    await waitFor(() => expect(result.current.src).toBe(SCULPTOR_ART));
    act(() => result.current.advanceFailedSource?.(SCULPTOR_ART));
    expect(result.current).toMatchObject({ src: null, isLoading: false });
    expect(result.current.reload).toBeTypeOf("function");
  });

  it("offers no reload for a token whose art has no oracle id to scope it", async () => {
    const env = await setup();
    const { result } = renderHook(() => env.useCardImage("Banana", { isToken: true }));

    // Reach guard: the lookup settles on the artless tile first.
    await waitFor(() => expect(result.current).toMatchObject({ src: null, isLoading: false }));
    expect(result.current.reload).toBeUndefined();
  });
});
