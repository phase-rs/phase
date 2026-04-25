import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { AttachmentChipRow } from "../AttachmentChipRow.tsx";

const firedRef = { current: false };

vi.mock("../../../hooks/useCardHover.ts", () => ({
  useCardHover: vi.fn(() => ({
    handlers: { "data-card-hover": true },
    firedRef,
  })),
}));

function makeAttachment(id: number, name: string, subtype = "Equipment"): GameObject {
  return {
    id,
    card_id: 200,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: 1,
    attachments: [],
    counters: {},
    name,
    power: null,
    toughness: null,
    loyalty: null,
    card_types: {
      supertypes: [],
      core_types: [subtype === "Aura" ? "Enchantment" : "Artifact"],
      subtypes: [subtype],
    },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: [],
    timestamp: 1,
    entered_battlefield_turn: 1,
  };
}

function inject(...attachments: GameObject[]) {
  const objects: Record<number, GameObject> = {};
  for (const a of attachments) objects[a.id] = a;
  useGameStore.setState({ gameState: { objects } as never });
}

describe("AttachmentChipRow", () => {
  beforeEach(() => {
    firedRef.current = false;
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when objectIds is empty", () => {
    inject();
    const { container } = render(<AttachmentChipRow objectIds={[]} />);

    expect(container).toBeEmptyDOMElement();
  });

  it("renders one chip per attachment up to the visible limit", () => {
    inject(
      makeAttachment(10, "Bonesplitter"),
      makeAttachment(11, "Pacifism", "Aura"),
      makeAttachment(12, "Strider Harness"),
    );

    render(<AttachmentChipRow objectIds={[10, 11, 12]} />);

    expect(screen.getAllByRole("button")).toHaveLength(3);
  });

  it("collapses to glyph-only chips when more than 3 attachments are present", () => {
    inject(
      makeAttachment(10, "Bonesplitter"),
      makeAttachment(11, "Pacifism", "Aura"),
      makeAttachment(12, "Strider Harness"),
      makeAttachment(13, "Curiosity", "Aura"),
    );

    render(<AttachmentChipRow objectIds={[10, 11, 12, 13]} />);

    // 3 chips rendered, plus the +N overflow indicator (which is a span, not a button)
    expect(screen.getAllByRole("button")).toHaveLength(3);
    // None of the visible chips should show their cost/name label — glyph-only.
    // Bonesplitter has NoCost → would fall back to its name "Bones…" if labels were shown.
    expect(screen.queryByText(/Bones/)).not.toBeInTheDocument();
  });

  it("shows a +N overflow indicator listing the remaining attachment names via title", () => {
    inject(
      makeAttachment(10, "Bonesplitter"),
      makeAttachment(11, "Pacifism", "Aura"),
      makeAttachment(12, "Strider Harness"),
      makeAttachment(13, "Curiosity", "Aura"),
      makeAttachment(14, "Shackles"),
    );

    render(<AttachmentChipRow objectIds={[10, 11, 12, 13, 14]} />);

    const overflow = screen.getByText("+2");
    expect(overflow).toBeInTheDocument();
    expect(overflow).toHaveAttribute("title", "Curiosity · Shackles");
  });

  it("does not collapse when exactly 3 attachments are present (full label mode)", () => {
    inject(
      makeAttachment(10, "Bonesplitter"),
      makeAttachment(11, "Pacifism", "Aura"),
      makeAttachment(12, "Strider Harness"),
    );

    render(<AttachmentChipRow objectIds={[10, 11, 12]} />);

    expect(screen.queryByText(/^\+\d+$/)).not.toBeInTheDocument();
  });

  it("uses a bullet separator in the overflow tooltip so commas in card names stay unambiguous", () => {
    // Many MTG card names contain commas (e.g., "Sram, Senior Edificer"). A
    // comma-joined tooltip would render four ambiguous tokens; a bullet keeps
    // the two card names parseable.
    inject(
      makeAttachment(10, "Bonesplitter"),
      makeAttachment(11, "Pacifism", "Aura"),
      makeAttachment(12, "Strider Harness"),
      makeAttachment(13, "Sram, Senior Edificer"),
      makeAttachment(14, "Jared Carthalion, True Heir"),
    );

    render(<AttachmentChipRow objectIds={[10, 11, 12, 13, 14]} />);

    const overflow = screen.getByText("+2");
    expect(overflow).toHaveAttribute(
      "title",
      "Sram, Senior Edificer · Jared Carthalion, True Heir",
    );
  });

  it("filters out missing attachment names from the overflow tooltip", () => {
    // Transient state: ID 99 isn't in objects yet. The +N still counts the slot
    // so the visible chip count stays correct, but the tooltip omits the missing name.
    inject(
      makeAttachment(10, "Bonesplitter"),
      makeAttachment(11, "Pacifism", "Aura"),
      makeAttachment(12, "Strider Harness"),
      makeAttachment(13, "Curiosity", "Aura"),
    );

    render(<AttachmentChipRow objectIds={[10, 11, 12, 13, 99]} />);

    const overflow = screen.getByText("+2");
    expect(overflow).toHaveAttribute("title", "Curiosity");
  });
});
