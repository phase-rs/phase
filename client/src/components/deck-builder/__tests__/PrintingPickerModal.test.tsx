import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { PrintingPickerModal } from "../PrintingPickerModal";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";

// `vi.mock` is hoisted above the imports, so the fixture its factory closes over
// has to be hoisted too — a plain `const` would be in the temporal dead zone.
const { EN_ID, DE_ID, cardUrl } = vi.hoisted(() => {
  const EN_ID = "0dbac7ce-a6fa-466e-b6ba-173cf2dec98e";
  const DE_ID = "345a1cf0-e4de-42a9-9c72-ed16826b9067";
  // Real five-segment `cards.scryfall.io` shape: a shorter URL is not
  // localizable at all, so every assertion below would pass vacuously.
  const cardUrl = (id: string) =>
    `https://cards.scryfall.io/normal/front/${id[0]}/${id[1]}/${id}.jpg`;
  return { EN_ID, DE_ID, cardUrl };
});

// Only `getCardPrintings` is stubbed. The localization path under test —
// `resolvePrintingImageUrl`, `loadLocaleArt`, `isLocaleArtReady` — stays real and
// shares one module closure, so the map a load installs is the map a tile reads.
vi.mock("../../../services/scryfall.ts", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("../../../services/scryfall.ts")>();
  return {
    ...actual,
    getCardPrintings: vi.fn().mockResolvedValue([
      {
        id: EN_ID,
        set: "mid",
        set_name: "Innistrad: Midnight Hunt",
        collector_number: "7",
        released_at: "2021-09-24",
        border_color: "black",
        frame_effects: [],
        full_art: false,
        faces: [{ small: cardUrl(EN_ID), normal: cardUrl(EN_ID), art_crop: cardUrl(EN_ID) }],
      },
    ]),
  };
});

describe("PrintingPickerModal localized art", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    usePreferencesStore.getState().setLanguage("en");
  });

  it("swaps tile art when the locale map arrives after the modal mounts", async () => {
    usePreferencesStore.getState().setLanguage("de");

    // Hold the locale map in flight so the modal is forced through the state
    // this test exists for: mounted, localized language, no map yet.
    let settle: ((r: Response) => void) | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn(
        (url: string) => {
          if (url !== "/scryfall-images.de.json") {
            return Promise.resolve(new Response(null, { status: 404 }));
          }
          return new Promise<Response>((resolve) => {
            settle = resolve;
          });
        },
      ),
    );

    render(
      <PrintingPickerModal cardName="Card" oracleId="oracle-1" onClose={() => {}} />,
    );

    // Pending map: the tile renders English art rather than blocking on a
    // fetch that may 404. This is also the reach guard for the swap below —
    // without it, a tile that never rendered at all would satisfy the final
    // assertion by never having been English in the first place.
    const img = await screen.findByRole("img");
    expect(img).toHaveAttribute("src", cardUrl(EN_ID));

    settle!(
      new Response(JSON.stringify({
        [EN_ID]: {
          id: DE_ID,
          faces: [{ small: cardUrl(DE_ID), normal: cardUrl(DE_ID), art_crop: cardUrl(DE_ID) }],
        },
      }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );

    // The picker resolves tile URLs during render, so arrival of the map is
    // only visible if the component subscribed to it. Drop `useLocaleArt()`
    // from the modal and this assertion fails on stale English art.
    await waitFor(() => {
      expect(screen.getByRole("img")).toHaveAttribute("src", cardUrl(DE_ID));
    });
  });
});
