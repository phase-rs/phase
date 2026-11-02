import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../useCardImage.ts";
import { loadScryfallData } from "../../services/scryfall.ts";
import { assetKey, catalogRoot, decodeCandidateKey, packId } from "../../services/visualPacks/types.ts";
import type { CardImageSource } from "../../services/visualPacks/types.ts";
import type { VisualRepositoryRequest } from "../../services/visualPacks/repository.ts";
import { useConnectivityStore } from "../../stores/connectivityStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

const repository = vi.hoisted(() => ({ installed: false, resolve: vi.fn() }));
vi.mock("../../services/visualPacks/repository.ts", () => ({
  visualPackRepository: { currentRevision: () => "0", subscribe: () => () => {}, resolve: repository.resolve },
}));

const ORACLE = "33333333-3333-4333-8333-333333333333";
const FRONT = "Invasion of Alara";
const BACK = "Awaken the Maelstrom";
const PIN = "44444444-4444-4444-8444-444444444444";
const OTHER_PIN = "55555555-5555-4555-8555-555555555555";
const card = {
  oracle_id: ORACLE, name: `${FRONT} // ${BACK}`, face_names: [FRONT.toLowerCase(), BACK.toLowerCase()],
  layout: "transform", mana_cost: "{W}{U}{B}{R}{G}", cmc: 5, type_line: "Battle — Siege // Sorcery",
  colors: [], color_identity: [], keywords: [],
  faces: [{ normal: "https://img.example/default-front.jpg", orientation: "landscape" }, { normal: "https://img.example/default-back.jpg", orientation: "portrait" }],
};
const printing = (id: string) => ({
  id, set: "mom", set_name: "March of the Machine", collector_number: id === PIN ? "230" : "231",
  released_at: "2023-04-21", border_color: "black", frame_effects: [], full_art: false,
  faces: [{ normal: `https://img.example/${id}-front.jpg` }, { normal: `https://img.example/${id}-back.jpg` }],
});

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("warm per-face image orientation through the real service", () => {
  it("binds default, installed and explicit pin art to the engine face before the numeric index", async () => {
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    usePreferencesStore.setState({ language: "en", artChain: [], artOverrides: {} });
    vi.stubGlobal("fetch", vi.fn((url: string) => Promise.resolve(new Response(JSON.stringify(
      url === "/scryfall-data.json" ? { [ORACLE]: card, [FRONT.toLowerCase()]: card } : { [ORACLE]: [printing(PIN), printing(OTHER_PIN)] },
    )))));
    repository.resolve.mockImplementation(async (request: VisualRepositoryRequest) => {
      let sources: CardImageSource[] = [{ kind: "fallback", src: null }];
      if (repository.installed && !request.allowRemote) {
        const face = request.groups.flatMap((group) => group.requested).map(decodeCandidateKey)
          .find(([kind]) => kind === "oracle_face")?.[1][1];
        sources = [{ kind: "installed", src: face === BACK.toLowerCase() ? "/installed-back.jpg" : "/installed-front.jpg", assetKey: assetKey("asset:v1:canonical_card:warm"), packId: packId("deck_library"), catalogRoot: catalogRoot("a".repeat(64)) }, ...sources];
      } else if (request.allowRemote && request.remote) {
        sources = [{ kind: "remote", src: request.remote.src }, ...sources];
      }
      return { revision: "0", sources };
    });
    await expect(loadScryfallData()).resolves.not.toBeNull();
    const front = renderHook(() => useCardImage(FRONT, { oracleId: ORACLE, faceName: FRONT, faceIndex: 0 }));
    const back = renderHook(() => useCardImage(FRONT, { oracleId: ORACLE, faceName: BACK, faceIndex: 0 }));
    await waitFor(() => {
      expect(front.result.current.src).toBe("https://img.example/default-front.jpg");
      expect(front.result.current.isRotated).toBe(true);
      expect(back.result.current.src).toBe("https://img.example/default-back.jpg");
      expect(back.result.current.isRotated).toBe(false);
    });
    front.unmount(); back.unmount();
    repository.installed = true;
    const localFront = renderHook(() => useCardImage(FRONT, { oracleId: ORACLE, faceName: FRONT, size: "large" }));
    const localBack = renderHook(() => useCardImage(FRONT, { oracleId: ORACLE, faceName: BACK, faceIndex: 0, size: "large" }));
    await waitFor(() => {
      expect(localFront.result.current.src).toBe("/installed-front.jpg");
      expect(localFront.result.current.isRotated).toBe(true);
      expect(localFront.result.current.isLoading).toBe(false);
      expect(localBack.result.current.src).toBe("/installed-back.jpg");
      expect(localBack.result.current.isRotated).toBe(false);
    });
    localFront.unmount(); localBack.unmount(); repository.installed = false;
    act(() => usePreferencesStore.getState().setArtOverride(ORACLE, { scryfallId: OTHER_PIN, setCode: "mom", collectorNumber: "231" }));
    const pinnedFront = renderHook(() => useCardImage(FRONT, { oracleId: ORACLE, faceName: FRONT, scryfallId: PIN }));
    const pinnedBack = renderHook(() => useCardImage(FRONT, { oracleId: ORACLE, faceName: BACK, faceIndex: 0, scryfallId: PIN }));
    await waitFor(() => {
      expect(pinnedFront.result.current.src).toBe(`https://img.example/${PIN}-front.jpg`);
      expect(pinnedFront.result.current.isRotated).toBe(true);
      expect(pinnedFront.result.current.isLoading).toBe(false);
      expect(pinnedBack.result.current.src).toBe(`https://img.example/${PIN}-back.jpg`);
      expect(pinnedBack.result.current.isRotated).toBe(false);
      expect(pinnedBack.result.current.isLoading).toBe(false);
    });
    expect(repository.resolve).toHaveBeenCalledWith(expect.objectContaining({ allowRemote: true, remote: expect.objectContaining({ src: `https://img.example/${PIN}-back.jpg` }) }));
    usePreferencesStore.getState().clearAllArtOverrides();
  });
});
