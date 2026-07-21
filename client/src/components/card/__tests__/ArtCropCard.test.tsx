import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
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

  it("renders a name tile for an artless token instead of a blank square (issue #6156)", () => {
    // `art_crop` is the DEFAULT battlefield display, so this is the render path
    // most players actually see. A token with no official paper printing (Kibo,
    // Uktabi Prince's Banana) resolves to a null src with no in-flight lookup;
    // collapsing that into the loading branch is what produced the reported
    // featureless dark square.
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    const token = {
      ...transformedPermanent(),
      name: "Banana",
      display_source: "Token",
      transformed: false,
      back_face: null,
      power: 2,
      toughness: 3,
      base_power: 2,
      base_toughness: 3,
      counters: { p1p1: 1 },
      card_types: { supertypes: [], core_types: ["Artifact", "Creature"], subtypes: ["Food"] },
    };
    useGameStore.setState({
      gameState: {
        objects: { [token.id]: token },
      } as never,
    });

    render(<ArtCropCard objectId={101} />);

    // The art slot carries the fallback tile instead of a blank/black box...
    expect(screen.getByRole("img", { name: "Banana" })).toBeInTheDocument();
    // ...and no <img> is emitted, so nothing can paint as a broken square.
    expect(document.querySelector("img")).toBeNull();

    // Critically, the CARD FRAME survives: swapping only the art (rather than
    // returning a bare tile before the frame) is what keeps game state on
    // screen. An artless creature token must still show its P/T and counters —
    // hiding those would trade one information loss for another, and the same
    // path is taken by EVERY permanent when an art fetch is rejected.
    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();
    expect(screen.getByText("/")).toBeInTheDocument();
    // The +1/+1 counter badge lives in the art area, as a sibling of the very
    // element the fix swaps — so it is the piece most at risk from a future
    // refactor of that slot.
    expect(screen.getByText("1")).toBeInTheDocument();
    // Name appears in both the frame header and the fallback tile's label.
    expect(screen.getAllByText("Banana").length).toBeGreaterThan(0);
  });

  it("falls back to the tile when resolved art fails to load", () => {
    // A URL that resolves but 404s (future-dated set, stale token image ref).
    // Without an onError handler the default renderer would show the browser's
    // broken-image glyph — the same defect issue #6156 reports.
    mockUseCardImage.mockReturnValue({
      src: "https://example.invalid/kuruk.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<ArtCropCard objectId={101} />);

    const img = document.querySelector("img");
    expect(img).not.toBeNull();
    fireEvent.error(img!);

    expect(screen.getByRole("img", { name: "Kuruk, the Mastodon" })).toBeInTheDocument();
    expect(document.querySelector("img")).toBeNull();
    // Frame survives here too.
    expect(screen.getByText("/")).toBeInTheDocument();
  });

  it("re-tries the art when the source changes after a load failure", () => {
    // Mirrors CardImage's equivalent. Without the reset effect a permanent
    // whose front-face art 404s would stay latched on the text tile across a
    // DFC transform forever, even once a loadable face arrives.
    mockUseCardImage.mockReturnValue({
      src: "https://example.invalid/front.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<ArtCropCard objectId={101} />);
    fireEvent.error(document.querySelector("img")!);
    expect(document.querySelector("img")).toBeNull();

    // ArtCropCard is memo()'d on `objectId`, so re-rendering with identical
    // props bails out before the hook is re-read. Push a fresh object identity
    // through the store instead — the same thing a real transform does.
    mockUseCardImage.mockReturnValue({
      src: "https://example.invalid/back.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    act(() => {
      useGameStore.setState({
        gameState: {
          objects: { 101: { ...transformedPermanent(), timestamp: 2 } },
        } as never,
      });
    });

    const img = document.querySelector("img");
    expect(img).not.toBeNull();
    expect(img!.getAttribute("src")).toBe("https://example.invalid/back.png");
  });

  it("still pulses while art is genuinely resolving", () => {
    // The companion to the case above: an in-flight lookup must NOT jump
    // straight to the name tile, or every card flashes text before its art.
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: true,
      isRotated: false,
      isFlip: false,
    });

    const { container } = render(<ArtCropCard objectId={101} />);

    expect(container.querySelector(".animate-pulse")).not.toBeNull();
    expect(screen.queryByRole("img")).toBeNull();
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

  // CR 732.2a / CR 701.34a: art-crop is a SECOND battlefield display mode; the ∞-counter
  // render must live here too or an accepted counter-growth loop silently shows its finite
  // count in this mode (the exact missed-render-site bug this pair guards). Matched pair:
  // marked ⇒ ∞ (not the count); unmarked ⇒ the count (not ∞) — reverting the render flip fails (1).
  function pentadWithCharge(): GameObject {
    return {
      ...transformedPermanent(),
      id: 101,
      name: "Pentad Prism",
      transformed: false,
      back_face: null,
      counters: { charge: 2 },
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] },
    };
  }

  it("renders ∞ for a counter the engine marks unbounded (CR 732.2a art-crop mode)", () => {
    mockUseCardImage.mockReturnValue({ src: "card.png", isLoading: false, isRotated: false, isFlip: false });
    const permanent = pentadWithCharge();
    useGameStore.setState({
      gameState: {
        objects: { [permanent.id]: permanent },
        derived: { unbounded_counters: { [permanent.id]: ["charge"] } },
      } as never,
    });

    render(<ArtCropCard objectId={101} />);

    expect(screen.getByText("∞")).toBeInTheDocument();
    // display-only: the real finite count is NOT shown when the loop is accepted.
    expect(screen.queryByText("2")).not.toBeInTheDocument();
  });

  it("renders the finite count when the counter is NOT marked unbounded (discriminator)", () => {
    mockUseCardImage.mockReturnValue({ src: "card.png", isLoading: false, isRotated: false, isFlip: false });
    const permanent = pentadWithCharge();
    useGameStore.setState({
      gameState: {
        objects: { [permanent.id]: permanent },
        derived: { unbounded_counters: {} },
      } as never,
    });

    render(<ArtCropCard objectId={101} />);

    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.queryByText("∞")).not.toBeInTheDocument();
  });

  // LOW-4 (CR 732.2a / CR 701.34a): the ∞ pill and its TOOLTIP must agree. Before the fix the
  // badge showed ∞ while the tooltip interpolated the raw finite count ("… : 2"), contradicting
  // the pill. The tooltip summary must say ∞, not leak the count.
  it("tooltip renders ∞ and hides the finite count when the counter is unbounded", () => {
    mockUseCardImage.mockReturnValue({ src: "card.png", isLoading: false, isRotated: false, isFlip: false });
    const permanent = pentadWithCharge();
    useGameStore.setState({
      gameState: {
        objects: { [permanent.id]: permanent },
        derived: { unbounded_counters: { [permanent.id]: ["charge"] } },
      } as never,
    });

    render(<ArtCropCard objectId={101} />);

    // The tooltip summary line reads "∞ <label> counters", never the finite "2 <label> counters".
    expect(screen.getByText(/∞ \S+ counters/i)).toBeInTheDocument();
    expect(screen.queryByText(/2 \S+ counters/i)).not.toBeInTheDocument();
  });

  it("tooltip shows the finite count when the counter is NOT unbounded (matched pair)", () => {
    mockUseCardImage.mockReturnValue({ src: "card.png", isLoading: false, isRotated: false, isFlip: false });
    const permanent = pentadWithCharge();
    useGameStore.setState({
      gameState: {
        objects: { [permanent.id]: permanent },
        derived: { unbounded_counters: {} },
      } as never,
    });

    render(<ArtCropCard objectId={101} />);

    expect(screen.getByText(/2 \S+ counters/i)).toBeInTheDocument();
    expect(screen.queryByText(/∞ \S+ counters/i)).not.toBeInTheDocument();
  });
});
