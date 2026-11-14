import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useCardHover } from "../../../hooks/useCardHover.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { CardReportDialog } from "../CardReportDialog.tsx";
import { CardCoverageDashboard } from "../../controls/CardCoverageDashboard.tsx";
import { CardTextboxPreview } from "../../modal/CardTextboxPreview.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({ useCardImage: vi.fn() }));
vi.mock("../../../hooks/useCardHover.ts", () => ({
  useCardHover: vi.fn(() => ({ handlers: {}, firedRef: { current: false } })),
}));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useCardParseDetails: vi.fn(() => []),
}));
vi.mock("../../../hooks/useCardReport.ts", () => ({
  useCardReport: vi.fn(() => ({ sent: false, report: vi.fn() })),
}));

const mockUseCardImage = vi.mocked(useCardImage);
const mockUseCardHover = vi.mocked(useCardHover);

beforeEach(() => {
  useUiStore.setState({ cardReportDialogOpen: false });
  useGameStore.setState({ gameState: null, gameMode: "local" });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("visual-pack visible previews", () => {
  it("keeps normal rungs coupled and advances the exact textbox source", () => {
    const advanceFailedSource = vi.fn();
    mockUseCardImage.mockReturnValue({
      src: "installed-normal.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: { small: "installed-small.png", normal: "installed-normal.png" },
      advanceFailedSource,
    });

    render(<CardTextboxPreview cardName="Public rules" />);
    const image = document.querySelector("img");
    expect(image).toHaveAttribute(
      "srcset",
      "installed-small.png 146w, installed-normal.png 488w",
    );
    fireEvent.error(image!);
    expect(advanceFailedSource).toHaveBeenCalledWith("installed-normal.png");
  });

  it("retains the public name after settled exhaustion", () => {
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<CardTextboxPreview cardName="Public rules" />);
    expect(screen.getByRole("img", { name: "Public rules" })).toHaveTextContent("Public rules");
  });

  it("keeps report rows on current face identity with card-crop versus token-normal intent", () => {
    const card = buildGameObject({
      id: 41,
      owner: 0,
      controller: 0,
      zone: "Battlefield",
      name: "Current Face",
      display_visible_to_viewer: true,
      printed_ref: { oracle_id: "current-oracle", face_name: "Current Face" } as never,
    });
    const token = buildGameObject({
      id: 42,
      owner: 0,
      controller: 0,
      zone: "Battlefield",
      name: "Public Token",
      display_source: "Token",
      display_visible_to_viewer: true,
      token_image_ref: {
        scryfall_id: "token-printing",
        scryfall_oracle_id: "token-oracle",
        face_name: "Public Token",
        preset_id: "token-preset",
      },
    });
    useGameStore.setState({
      gameMode: "local",
      gameState: buildGameState({
        players: buildPlayers([0, 1]),
        objects: buildObjectMap(card, token),
        battlefield: [card.id, token.id],
        seat_order: [0, 1],
      }),
    });
    useUiStore.setState({ cardReportDialogOpen: true });
    const advanceFailedSource = vi.fn();
    mockUseCardImage.mockImplementation((name) => ({
      src: `${name}.png`,
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: name === "Public Token"
        ? { small: "token-small.png", normal: "Public Token.png" }
        : undefined,
      advanceFailedSource,
    }));

    render(<CardReportDialog />);

    expect(mockUseCardImage).toHaveBeenCalledWith(
      "Current Face",
      expect.objectContaining({
        size: "art_crop",
        oracleId: "current-oracle",
        faceName: "Current Face",
        isToken: false,
      }),
    );
    expect(mockUseCardImage).toHaveBeenCalledWith(
      "Public Token",
      expect.objectContaining({
        size: "normal",
        isToken: true,
        tokenImageRef: expect.objectContaining({ scryfall_id: "token-printing" }),
      }),
    );
    expect(mockUseCardHover).toHaveBeenCalledWith(41);
    expect(document.querySelector('img[src="Public Token.png"]')).toHaveAttribute(
      "srcset",
      "token-small.png 146w, Public Token.png 488w",
    );
    fireEvent.error(document.querySelector('img[src="Current Face.png"]')!);
    expect(advanceFailedSource).toHaveBeenCalledWith("Current Face.png");
  });

  it("renders coverage rungs", async () => {
    vi.stubGlobal("__COVERAGE_DATA_URL__", "/coverage.json");
    vi.stubGlobal("fetch", vi.fn(async () => ({
      ok: true,
      json: async () => ({
        total_cards: 1,
        supported_cards: 1,
        coverage_pct: 100,
        cards: [{
          card_name: "Coverage Card",
          set_code: "TST",
          supported: true,
          oracle_text: "Public rules",
          parse_details: [],
        }],
      }),
    })));
    const advanceFailedSource = vi.fn();
    mockUseCardImage.mockReturnValue({
      src: "coverage-normal.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: { small: "coverage-small.png", normal: "coverage-normal.png" },
      advanceFailedSource,
    });

    render(<CardCoverageDashboard />);
    fireEvent.click(await screen.findByRole("button", { name: /Coverage Card/i }));
    const image = await screen.findByRole("img", { name: "Coverage Card" });
    expect(image).toHaveAttribute(
      "srcset",
      "coverage-small.png 146w, coverage-normal.png 488w",
    );
    fireEvent.error(image);
    expect(advanceFailedSource).toHaveBeenCalledWith("coverage-normal.png");

  });
});
