// Revert witness for the owner-printing change: it renders the real CardImage through the real
// image hook and metadata caches and asserts the observable result (the owner's printing URL).
// It imports nothing the change introduced, so against the pre-change code it still compiles and
// fails on this assertion (the baseline ignores the extra owner props and shows the canonical image).
// It lives in `tests/` rather than `__tests__/` on purpose: the PR checker's revert proof reverts
// every non-test file and excludes `**/tests/**`, which keeps this witness runnable on the reverted tree.
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type * as Platform from "../../../services/platform.ts";

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
}));
vi.mock("../../../services/platform.ts", async (original) => ({
  ...await original<typeof Platform>(),
  loadVisualPackBackend: async () => null,
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.resetModules();
  localStorage.clear();
});

it("renders the owner's printing rather than the canonical image", async () => {
  vi.resetModules();
  localStorage.clear();
  const oracleId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const ownerUrl = "https://img.example/owner-printing.jpg";
  const entry = {
    oracle_id: oracleId, name: "Twin", face_names: ["twin"], layout: "normal",
    mana_cost: "{1}", cmc: 1, type_line: "Creature", colors: [], color_identity: [],
    keywords: [], faces: [{ normal: "/canonical.jpg" }],
  };
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
    const path = new URL(String(input), "http://localhost/").pathname;
    const data = path === "/scryfall-data.json"
      ? { twin: entry, [oracleId]: entry }
      : path === "/scryfall-printings.json"
        ? { [oracleId]: [{
          id: "11111111-1111-4111-8111-111111111111",
          set: "own", collector_number: "1", set_name: "Owner", released_at: "2026-01-01",
          border_color: "black", frame_effects: [], full_art: false,
          faces: [{ normal: ownerUrl }],
        }] }
        : null;
    if (data === null) throw new Error(`Unexpected metadata request ${String(input)}`);
    return new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
  }));
  // Reload after resetModules so the real metadata caches and consumer share an isolated graph.
  const preferences = (await import("../../../stores/preferencesStore.ts")).usePreferencesStore;
  preferences.setState({ ...preferences.getInitialState(), language: "en", artOverrides: {}, artChain: [] });
  const connectivity = (await import("../../../stores/connectivityStore.ts")).useConnectivityStore;
  connectivity.setState({ forcedOffline: false, browserOnline: true });
  await (await import("../../../services/scryfall.ts")).loadScryfallData();
  const { CardImage } = await import("../CardImage.tsx");
  // Structural props use only pre-existing imports; the baseline safely ignores the extra data.
  const props = {
    cardName: "Twin",
    ownerPrinting: { set_code: "own", collector_number: "1" },
    ownerPrintedRef: { oracle_id: oracleId, face_name: "Twin" },
  };
  const surface = render(<CardImage {...props} />);
  await waitFor(() => expect(surface.getByRole("img")).toHaveAttribute("src", ownerUrl));
});
