import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useCardHover } from "../../../hooks/useCardHover.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { AttachmentChip } from "../AttachmentChip.tsx";

const firedRef = { current: false };

vi.mock("../../../hooks/useCardHover.ts", () => ({
  useCardHover: vi.fn(() => ({
    handlers: { "data-card-hover": true, onMouseEnter: vi.fn(), onMouseLeave: vi.fn() },
    firedRef,
  })),
}));

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(() => Promise.resolve()),
}));

const mockedUseCardHover = vi.mocked(useCardHover);
const mockedDispatchAction = vi.mocked(dispatchAction);

function targetingState(legalIds: number[], player = 0): WaitingFor {
  return {
    type: "TargetSelection",
    data: {
      player,
      pending_cast: {} as never,
      target_slots: [],
      selection: {
        current_legal_targets: legalIds.map((id) => ({ Object: id })),
      } as never,
    },
  } as WaitingFor;
}

function makeAttachment(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 50,
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
    name: "Bonesplitter",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
    mana_cost: { type: "Cost", shards: [], generic: 1 },
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
    ...overrides,
  };
}

function injectObject(obj: GameObject) {
  useGameStore.setState({
    gameState: { objects: { [obj.id]: obj } } as never,
  });
}

describe("AttachmentChip", () => {
  beforeEach(() => {
    firedRef.current = false;
    mockedUseCardHover.mockClear();
    mockedDispatchAction.mockClear();
    useUiStore.setState({ selectedObjectId: null });
    useGameStore.setState({ waitingFor: null });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders the Equipment glyph for Equipment subtype", () => {
    injectObject(makeAttachment());
    render(<AttachmentChip id={50} />);

    expect(screen.getByRole("button")).toHaveTextContent("⚒");
  });

  it("renders the Aura glyph for Aura subtype", () => {
    injectObject(makeAttachment({
      name: "Pacifism",
      card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
    }));
    render(<AttachmentChip id={50} />);

    expect(screen.getByRole("button")).toHaveTextContent("✧");
  });

  it("renders the Fortification glyph for Fortification subtype", () => {
    injectObject(makeAttachment({
      name: "Darksteel Garrison",
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Fortification"] },
    }));
    render(<AttachmentChip id={50} />);

    expect(screen.getByRole("button")).toHaveTextContent("▣");
  });

  it("falls back to the Other glyph for an attachment without a known subtype", () => {
    injectObject(makeAttachment({
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] },
    }));
    render(<AttachmentChip id={50} />);

    expect(screen.getByRole("button")).toHaveTextContent("◇");
  });

  it("renders the face-down placeholder regardless of subtype", () => {
    injectObject(makeAttachment({ face_down: true }));
    render(<AttachmentChip id={50} />);

    expect(screen.getByRole("button")).toHaveTextContent("?");
  });

  it("returns null when the attachment object is not in the store (transient state)", () => {
    useGameStore.setState({ gameState: { objects: {} } as never });
    const { container } = render(<AttachmentChip id={50} />);

    expect(container).toBeEmptyDOMElement();
  });

  it("calls selectObject with the attachment id on click", () => {
    injectObject(makeAttachment());
    render(<AttachmentChip id={50} />);

    fireEvent.click(screen.getByRole("button"));

    expect(useUiStore.getState().selectedObjectId).toBe(50);
  });

  it("suppresses click when firedRef indicates a long-press just fired, and resets the flag", () => {
    injectObject(makeAttachment());
    firedRef.current = true;
    render(<AttachmentChip id={50} />);

    fireEvent.click(screen.getByRole("button"));

    expect(useUiStore.getState().selectedObjectId).toBeNull();
    // Reset is critical — without it, the next click is also suppressed.
    expect(firedRef.current).toBe(false);
  });

  it("spreads useCardHover handlers (data-card-hover invariant) onto the button", () => {
    injectObject(makeAttachment());
    render(<AttachmentChip id={50} />);

    expect(screen.getByRole("button")).toHaveAttribute("data-card-hover");
  });

  it("renders a tap indicator when the attachment is tapped", () => {
    injectObject(makeAttachment({ tapped: true }));
    render(<AttachmentChip id={50} />);

    expect(screen.getByLabelText("tapped")).toBeInTheDocument();
  });

  it("renders the predominant counter count with a type-aware aria-label", () => {
    injectObject(makeAttachment({ counters: { Plus1Plus1: 3 } }));
    render(<AttachmentChip id={50} />);

    // formatCounterType passes through unknown variants — the type name appears
    // verbatim. The +N visible text is what the player reads at a glance.
    const label = screen.getByLabelText(/Plus1Plus1 counters: 3/);
    expect(label).toHaveTextContent("+3");
  });

  it("picks the largest-count entry when multiple counter types are present (Cranial Plating + Charge)", () => {
    // Cranial Plating-like case: 1 P1P1 counter from a buff, 4 Charge counters
    // from accumulated activations. The chip surfaces the predominant (Charge).
    injectObject(makeAttachment({
      counters: { Plus1Plus1: 1, Charge: 4 },
    }));
    render(<AttachmentChip id={50} />);

    const label = screen.getByLabelText(/Charge counters: 4/);
    expect(label).toHaveTextContent("+4");
  });

  it("includes the counter summary in the chip's tooltip", () => {
    injectObject(makeAttachment({ counters: { Plus1Plus1: 2 } }));
    render(<AttachmentChip id={50} />);

    expect(screen.getByRole("button")).toHaveAttribute(
      "title",
      expect.stringContaining("(Plus1Plus1 ×2)"),
    );
  });

  it("hides label and state indicators in glyph-only mode", () => {
    injectObject(makeAttachment({ tapped: true, counters: { Plus1Plus1: 2 } }));
    render(<AttachmentChip id={50} glyphOnly />);

    expect(screen.queryByLabelText("tapped")).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/Plus1Plus1 counters: 2/)).not.toBeInTheDocument();
  });

  it("stops click propagation so the host card's onClick does not also fire", () => {
    injectObject(makeAttachment());
    const hostClick = vi.fn();
    render(
      <div onClick={hostClick}>
        <AttachmentChip id={50} />
      </div>,
    );

    fireEvent.click(screen.getByRole("button"));

    expect(hostClick).not.toHaveBeenCalled();
    expect(useUiStore.getState().selectedObjectId).toBe(50);
  });

  it("dispatches ChooseTarget when the chip is a legal target during TargetSelection", () => {
    injectObject(makeAttachment());
    useGameStore.setState({ waitingFor: targetingState([50]) });

    render(<AttachmentChip id={50} />);
    fireEvent.click(screen.getByRole("button"));

    expect(mockedDispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 50 } },
    });
    // Selection must NOT happen — targeting takes precedence.
    expect(useUiStore.getState().selectedObjectId).toBeNull();
  });

  it("renders the targeting glow ring when the chip is a legal target", () => {
    injectObject(makeAttachment());
    useGameStore.setState({ waitingFor: targetingState([50]) });

    render(<AttachmentChip id={50} />);

    expect(screen.getByRole("button").className).toContain("ring-amber-400/60");
  });

  it("falls through to selectObject when no targeting prompt matches the chip", () => {
    injectObject(makeAttachment());
    // Targeting is active but a different object is the legal target.
    useGameStore.setState({ waitingFor: targetingState([99]) });

    render(<AttachmentChip id={50} />);
    fireEvent.click(screen.getByRole("button"));

    expect(mockedDispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().selectedObjectId).toBe(50);
  });

  it("does not glow or dispatch when the opponent is the prompted player (multiplayer gate)", () => {
    injectObject(makeAttachment());
    // The opponent (player 1) is choosing targets. Even though id=50 is in
    // their legal-targets set, the local player (PLAYER_ID=0) must not be
    // able to fire a ChooseTarget on their behalf.
    useGameStore.setState({ waitingFor: targetingState([50], 1) });

    render(<AttachmentChip id={50} />);
    const button = screen.getByRole("button");

    expect(button.className).not.toContain("ring-amber-400/60");

    fireEvent.click(button);

    expect(mockedDispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().selectedObjectId).toBe(50);
  });
});
