import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { CARD_BACK_URL } from "../../../services/scryfall.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardPreview } from "../CardPreview.tsx";

// The mock src ENCODES which lookup produced it: an oracle-id lookup stamps
// the id, a marker/token lookup stamps the ref's oracle id, a bare name
// lookup stamps the name. The hidden-information assertions below read that
// stamp back — a leaked `printed_ref` becomes a visible "secret-oracle" src.
vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn((
    cardName: string,
    options?: {
      oracleId?: string;
      tokenImageRef?: { scryfall_oracle_id?: string | null } | null;
    },
  ) => ({
    src: `${options?.oracleId ?? (options?.tokenImageRef ? `ref:${options.tokenImageRef.scryfall_oracle_id}` : cardName)}.png`,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

// The mobile overlay is the subject: force the mobile branch.
vi.mock("../../../hooks/useIsMobile.ts", () => ({
  useIsMobile: () => true,
}));

const SECRET_ORACLE = "secret-oracle-id";

function hiddenFaceDown(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObject({
    id: 101,
    card_id: 1,
    zone: "Battlefield",
    name: "",
    face_down: true,
    // The leak input under test: a wire that still carries the hidden card's
    // printing (the engine clears it today — morph.rs pins that — but the
    // display must not rely on it; a stale save or future field is enough).
    printed_ref: { oracle_id: SECRET_ORACLE, face_name: "Hooded Hydra" } as never,
    ...overrides,
  });
}

function inspect(object: GameObject): void {
  useGameStore.setState({
    gameState: buildGameState({
      objects: buildObjectMap(object),
      next_object_id: 102,
      battlefield: [object.id],
      next_timestamp: 2,
    }),
    spellCosts: {},
  });
  useUiStore.setState({ inspectedObjectId: object.id });
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useGameStore.setState({ gameState: null, spellCosts: {} });
  useUiStore.getState().dismissPreview();
  useUiStore.setState({ inspectedObjectId: null });
});

describe("CardPreview mobile face-down (hidden information, #7551 review)", () => {
  it("previews an opponent's Morph as the marker — never the printed ref", () => {
    inspect(hiddenFaceDown({ face_down_cause: "Morph" as never }));

    const { container } = render(<CardPreview cardName="Morph" objectId={101} />);

    const srcs = [...container.querySelectorAll("img")].map((img) => img.getAttribute("src"));
    expect(srcs.some((src) => src?.includes(SECRET_ORACLE))).toBe(false);
    // The marker ref names the Morph token's oracle id — that IS the image.
    expect(srcs.some((src) => src?.startsWith("ref:"))).toBe(true);
    for (const call of vi.mocked(useCardImage).mock.calls) {
      expect(call[1]?.oracleId).not.toBe(SECRET_ORACLE);
      expect(call[1]?.faceName).not.toBe("Hooded Hydra");
    }
  });

  it("previews a markerless face-down as the plain back — no name or ref lookup", () => {
    inspect(hiddenFaceDown({ face_down_cause: "TurnedFaceDown" as never }));

    const { container } = render(<CardPreview cardName="Face-down card" objectId={101} />);

    const srcs = [...container.querySelectorAll("img")].map((img) => img.getAttribute("src"));
    expect(srcs).toContain(CARD_BACK_URL);
    expect(srcs.some((src) => src?.includes(SECRET_ORACLE))).toBe(false);
    // The generic label must never become a card-name search either.
    expect(srcs.some((src) => src?.includes("Face-down card.png"))).toBe(false);
    for (const call of vi.mocked(useCardImage).mock.calls) {
      expect(call[1]?.oracleId).not.toBe(SECRET_ORACLE);
    }
  });
});
