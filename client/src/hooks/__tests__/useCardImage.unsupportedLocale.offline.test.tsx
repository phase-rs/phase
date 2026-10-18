import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { decodeCandidateKey } from "../../services/visualPacks/candidateKeys.ts";

/**
 * Production regression for the shipped-UI-locale vs packed-content-locale bug
 * found in review on #9796.
 *
 * The reported path: a player selects `zh`, an English printing is installed in
 * a pack that is NOT `deck_library`, and the app is offline. `cardCandidateGroups`
 * built a localized candidate key for `zh` unconditionally, `encodeCandidateKey`
 * rejected it (`invalid locale` — packs carry six locales, `zh` is not one), the
 * hook's catch degraded to `[]`, and the installed English candidate was lost.
 * Offline, there is no remote to fall back to, so the card rendered with no art
 * even though its art was installed.
 *
 * The builder-level invariant is pinned separately in
 * `candidateKeys.packedLocales.test.ts`. This test exists because that one cannot
 * show the consequence: it stops at the groups, and the loss happened in the
 * hook's offline resolution.
 */
const PRINTING = "11111111-abcd-4111-8111-111111111111";

describe("useCardImage with a UI locale that packs do not carry", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("keeps the installed English candidate offline when the locale is unsupported (zh)", async () => {
    // Record what the repository was actually asked for, so the assertion is
    // about the request the hook makes rather than only its result.
    const requestedKinds: string[][] = [];
    const resolve = vi.fn(async ({ groups }: { groups: Array<{ requested: string[] }> }) => {
      const kinds = groups.map((group) => decodeCandidateKey(group.requested[0])[0]);
      requestedKinds.push(kinds);
      // Serve an installed asset only for the English printing candidate, which
      // is what a `printing/complete` pack holds.
      return {
        revision: "0",
        sources: kinds.includes("english_printing")
          ? [{ kind: "installed" as const, src: "installed-english.jpg", assetKey: "k" as never, packId: "p" as never, catalogRoot: "r" as never }]
          : [{ kind: "fallback" as const, src: null }],
      };
    });
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: { currentRevision: () => "0", subscribe: () => () => {}, resolve },
    }));

    // No remote work at all: the failure mode is specifically that offline
    // resolution had nothing installed to reach.
    const fetchMock = vi.fn();
    global.fetch = fetchMock as unknown as typeof global.fetch;

    vi.doMock("../../services/scryfall.ts", () => ({
      deriveImageUrl: (url: string) => url,
      fetchCardImageAsset: vi.fn(),
      fetchCardImageAssetByOracleId: vi.fn().mockResolvedValue({
        src: "https://cards.scryfall.io/normal/front/1/1/english.jpg",
        isRotated: false,
        semantic: { oracleId: "oracle-zh", faceIndex: 0, alias: "shared card" },
      }),
      fetchTokenImageAssetByRef: vi.fn(),
      fetchTokenImageUrl: vi.fn(),
      findPrintingById: vi.fn(),
      getCardPrintings: vi.fn().mockResolvedValue([]),
      imageUrlSize: vi.fn(() => null),
      isCardImageFlipLayoutSync: vi.fn(() => false),
      isCardImageRotatedSync: vi.fn(() => false),
      isLocaleArtReady: vi.fn(() => true),
      loadLocaleArt: vi.fn(),
      loadScryfallData: vi.fn().mockResolvedValue(null),
      providerCompletionSources: vi.fn(() => []),
      resolveFaceIndexSync: vi.fn(() => null),
      resolveOracleIdSync: vi.fn(() => null),
      resolvePrintingImagePair: vi.fn(() => null),
    }));

    const { useConnectivityStore } = await import("../../stores/connectivityStore.ts");
    useConnectivityStore.setState({ forcedOffline: true, browserOnline: true });
    const { usePreferencesStore } = await import("../../stores/preferencesStore.ts");
    usePreferencesStore.setState({ language: "zh" });

    const { useCardImage } = await import("../useCardImage.ts");
    const { result } = renderHook(() => useCardImage("Shared Card", {
      oracleId: "oracle-zh",
      faceIndex: 0,
      scryfallId: PRINTING,
    }));

    await waitFor(() => expect(resolve).toHaveBeenCalled());

    // The request must retain the English group. This is the assertion that
    // fails when the `zh` localized key is built, throws, and empties the set.
    expect(requestedKinds[0]).toContain("english_printing");
    expect(requestedKinds[0]).not.toContain("localized_printing");

    // ...and the installed asset must actually reach the rendered source.
    await waitFor(() => expect(result.current.src).toBe("installed-english.jpg"));
    expect(fetchMock).not.toHaveBeenCalled();

    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    usePreferencesStore.setState({ language: "en" });
  });
});
