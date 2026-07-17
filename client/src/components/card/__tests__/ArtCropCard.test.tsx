import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { ArtCropCard } from "../ArtCropCard.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({ src: null, isLoading: true })),
}));

const mockUseCardImage = vi.mocked(useCardImage);

function transformedPermanent(): GameObject {
  return {
    id: 101,
    card_id: 201,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: true,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Kuruk, the Mastodon",
    power: 7,
    toughness: 7,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elephant"] },
    mana_cost: { type: "Cost", shards: [], generic: 0 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Green"],
    available_mana_pips: [],
    base_power: 7,
    base_toughness: 7,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: 1,
    entered_battlefield_turn: 1,
    is_commander: false,
    commander_tax: 0,
    unimplemented_mechanics: [],
    back_face: {
      name: "The Legend of Kuruk",
      power: null,
      toughness: null,
      card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Saga"] },
      mana_cost: { type: "Cost", shards: [], generic: 4 },
      keywords: [],
      abilities: [],
      color: ["Green"],
    },
  };
}

describe("ArtCropCard", () => {
  beforeEach(() => {
    const permanent = transformedPermanent();
    mockUseCardImage.mockClear();
    useGameStore.setState({
      gameState: {
        objects: { [permanent.id]: permanent },
      } as never,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("uses the front-face lookup key and back face index for transformed permanents", () => {
    render(<ArtCropCard objectId={101} />);

    expect(mockUseCardImage).toHaveBeenCalledWith(
      "The Legend of Kuruk",
      expect.objectContaining({
        size: "art_crop",
        faceIndex: 1,
      }),
    );
  });

  it("renders the card back for face-down permanents", () => {
    const permanent = {
      ...transformedPermanent(),
      face_down: true,
      name: "Hidden Sorcery",
      transformed: false,
      back_face: null,
      color: [],
      base_color: [],
    };

    useGameStore.setState({
      gameState: {
        objects: { [permanent.id]: permanent },
      } as never,
    });

    render(<ArtCropCard objectId={101} />);

    expect(screen.getByAltText("Face-down card")).toBeInTheDocument();
    expect(mockUseCardImage).toHaveBeenCalledWith(
      "",
      expect.objectContaining({
        size: "art_crop",
        oracleId: undefined,
        faceName: undefined,
      }),
    );
  });

  it("keeps loyalty and P/T readable for planeswalkers and creature planeswalkers", () => {
    mockUseCardImage.mockReturnValue({
      src: "card.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    const planeswalker = {
      ...transformedPermanent(),
      name: "Jace, Test Walker",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      loyalty: 4,
      card_types: { supertypes: [], core_types: ["Planeswalker"], subtypes: [] },
    };
    useGameStore.setState({
      gameState: { objects: { [planeswalker.id]: planeswalker } } as never,
    });

    const { unmount } = render(<ArtCropCard objectId={101} />);
    const loyaltyBadge = screen.getByRole("img", { name: "4" });
    expect(loyaltyBadge).toHaveStyle({ position: "absolute", bottom: "-5px", right: "-5px" });
    expect(screen.queryByText("/")).not.toBeInTheDocument();
    unmount();

    const creaturePlaneswalker = {
      ...planeswalker,
      power: 4,
      toughness: 4,
      base_power: 4,
      base_toughness: 4,
      card_types: { supertypes: [], core_types: ["Creature", "Planeswalker"], subtypes: [] },
    };
    useGameStore.setState({
      gameState: { objects: { [creaturePlaneswalker.id]: creaturePlaneswalker } } as never,
    });

    render(<ArtCropCard objectId={101} />);
    expect(screen.getByRole("img", { name: "4" })).toHaveStyle({ position: "absolute", bottom: "-5px", left: "-5px" });
    expect(screen.getByText("/")).toBeInTheDocument();
  });
});
