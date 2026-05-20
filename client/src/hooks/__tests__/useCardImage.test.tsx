import { renderHook, waitFor } from "@testing-library/react";
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
});
