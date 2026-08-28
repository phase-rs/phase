import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef, useState } from "react";

import type { DraftPlayerView } from "../../../adapter/draft-adapter";
import type { PackDisplayController, PackDropSource } from "../PackDisplay";
import type { DraftDropDispatch, DraftDropRequest, DraftPickInteractionSnapshot } from "../workspace/useDraftWorkspaceDrag";
import { useDraftWorkspaceDrag } from "../workspace/useDraftWorkspaceDrag";
import { DRAFT_WORKSPACE_PACK_SCALE_DEFAULT } from "../workspace/workspacePreferences";

const imageState = vi.hoisted(() => ({
  src: null as string | null,
  isLoading: false,
  isFlip: false,
  sources: {} as Record<string, string | null>,
  faceSources: {} as Record<string, string | null>,
}));
const alternateFaceState = vi.hoisted(() => ({
  values: {} as Record<string, { name: string; faceIndex: number; side: "front" | "back" } | null>,
}));
vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: (cardName: string, options?: { faceIndex?: number }) => ({
    src: Object.prototype.hasOwnProperty.call(
      imageState.faceSources,
      `${cardName}:${options?.faceIndex ?? 0}`,
    )
      ? imageState.faceSources[`${cardName}:${options?.faceIndex ?? 0}`]
      : Object.prototype.hasOwnProperty.call(imageState.sources, cardName)
        ? imageState.sources[cardName]
        : imageState.src,
    isLoading: imageState.isLoading,
    isFlip: imageState.isFlip,
    isRotated: false,
  }),
}));
vi.mock("../../../services/scryfall", () => ({
  resolveAlternateCardFaceSync: (cardName: string) => (
    Object.prototype.hasOwnProperty.call(alternateFaceState.values, cardName)
      ? alternateFaceState.values[cardName]
      : undefined
  ),
}));

import { PackDisplay } from "../PackDisplay";
import { menuButtonClass } from "../../menu/buttonStyles";

const cards = [
  { instance_id: "unknown", name: "Same", set_code: "TST", collector_number: "1", rarity: "special", colors: [], cmc: 1, type_line: "Card" },
  { instance_id: "common", name: "Same", set_code: "TST", collector_number: "2", rarity: "common", colors: [], cmc: 1, type_line: "Card" },
];
const effectCard = { instance_id: "effect", name: "Effect", set_code: "TST", collector_number: "3", rarity: "rare", colors: [], cmc: 1, type_line: "Card" };
const view = {
  status: "Drafting", kind: "Premier", current_pack_number: 0, pick_number: 0, pass_direction: "Left",
  current_pack: cards, pool: [], draft_effects: [], pool_groups: {
    color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [], type_filter_options: [], color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: ["common", "rarity_other"] },
    workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
  }, seats: [], cards_per_pack: 14, required_pick_count: 1, pick_steps_per_pack: 14, pack_count: 3, min_deck_size: 40, addable_cards: [], timer_remaining_ms: null,
  standings: [], current_round: 0, next_pairing_round: 1, tournament_format: "Swiss", pod_policy: "Competitive", pairings: [], match_config: { match_type: "Bo1" },
} satisfies DraftPlayerView;
const dragController = {
  handlePointerDown: vi.fn(), handlePointerMove: vi.fn(), handlePointerUp: vi.fn(), handlePointerCancel: vi.fn(),
  handleLostPointerCapture: vi.fn(), consumeCompatibilityActivation: () => false,
};

function controller(overrides: Partial<Extract<PackDisplayController, { kind: "local-workspace" }>> = {}): Extract<PackDisplayController, { kind: "local-workspace" }> {
  return {
    kind: "local-workspace", view, selectedCard: null, pendingIntent: null, interactionGeneration: 4,
    interactionLocked: false, doubleClickPick: false, dragController, selectCard: vi.fn(),
    pickCard: vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" }),
    pickCardStep: vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" }),
    confirmPick: vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" }),
    pickCardWithDraftEffect: vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" }),
    autoPickCard: vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" }), ...overrides,
  };
}

function RealDragPackHarness({
  onDrop,
  responsiveLayout = "desktop",
}: {
  onDrop(request: DraftDropRequest): DraftDropDispatch;
  responsiveLayout?: "desktop" | "tablet-portrait";
}) {
  const [interaction, setInteraction] = useState<DraftPickInteractionSnapshot>({
    interactionGeneration: 4,
    pickInteractionLocked: false,
    pendingPickIntent: null,
  });
  const interactionRef = useRef(interaction);
  const listenersRef = useRef(new Set<() => void>());
  const publish = (next: DraftPickInteractionSnapshot) => {
    interactionRef.current = next;
    setInteraction(next);
    for (const listener of listenersRef.current) listener();
  };
  const drag = useDraftWorkspaceDrag({
    enabled: true,
    readPickInteraction: () => interactionRef.current,
    subscribePickInteraction: (listener) => {
      listenersRef.current.add(listener);
      return () => listenersRef.current.delete(listener);
    },
    onDrop: (request) => {
      const dispatch = onDrop(request);
      publish({
        interactionGeneration: 4,
        pickInteractionLocked: true,
        pendingPickIntent: { kind: "pick", instanceIds: ["unknown"], destination: request.destination, placementHint: request.placementHint },
      });
      return dispatch;
    },
    resolveCollapsedSideboardColumn: () => 0,
  });
  return (
    <>
      <PackDisplay
        controller={controller({
          dragController: drag,
          pendingIntent: interaction.pendingPickIntent,
          interactionLocked: interaction.pickInteractionLocked,
        })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
        responsiveLayout={responsiveLayout}
      />
      <div data-testid="real-drag-target" ref={drag.registerCollapsedSideboard} />
      <button type="button" onClick={() => publish({ interactionGeneration: 4, pickInteractionLocked: false, pendingPickIntent: null })}>unlock</button>
    </>
  );
}

describe("PackDisplay local workspace controller", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    imageState.src = null;
    imageState.isLoading = false;
    imageState.isFlip = false;
    imageState.sources = {};
    imageState.faceSources = {};
    alternateFaceState.values = {};
  });

  it("renders_authoritative_sequence_once_and_preserves_duplicate_names_and_unknown_rarity", () => {
    const { container } = render(<PackDisplay controller={controller()} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    expect([...container.querySelectorAll("[data-instance-id]")].map((node) => node.getAttribute("data-instance-id"))).toEqual(["unknown", "common"]);
    const currentPick = screen.getByText("Pack 1 Pick 1");
    expect(currentPick).toHaveClass("text-sm", "text-fg");
    expect(currentPick.closest("[data-pack-status-controls]")).toBeInTheDocument();
    expect(currentPick.closest("[data-pack-toolbar]")).toHaveClass(
      "flex-nowrap",
      "overflow-x-auto",
      "[scrollbar-width:none]",
      "[&::-webkit-scrollbar]:hidden",
    );
    expect(screen.queryByText(/cards? in pack/i)).not.toBeInTheDocument();
    expect(screen.queryByText("Mythic Rare")).not.toBeInTheDocument();
    const scaleControls = [
      screen.getByRole("button", { name: "Decrease pack scale" }),
      screen.getByRole("button", { name: "Reset pack scale" }),
      screen.getByRole("button", { name: "Increase pack scale" }),
    ];
    for (const control of scaleControls) {
      for (const className of menuButtonClass({ tone: "neutral", size: "icon" }).split(" ")) {
        expect(control).toHaveClass(className);
      }
    }
    expect(scaleControls[1]).not.toHaveTextContent(`${DRAFT_WORKSPACE_PACK_SCALE_DEFAULT}×`);
    expect(scaleControls[1].querySelector("svg")).toHaveAttribute("viewBox", "0 0 20 20");
    const firstCard = container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    expect(firstCard.querySelector(".absolute.bottom-1 > span")).toHaveTextContent("Same");
    expect(within(firstCard).getByRole("button", { name: "Pick Same to Deck" })).toHaveClass("sr-only");
    expect(within(firstCard).getByRole("button", { name: "Pick Same to Sideboard" })).toHaveClass("sr-only");
  });

  it("pins_the_phone_toolbar_and_reserves_a_pack_glow_gutter", () => {
    render(
      <PackDisplay
        controller={controller()}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
        responsiveLayout="phone-portrait"
        phoneToolbarPinned
      />,
    );

    expect(screen.getByText("Pack 1 Pick 1").closest("[data-pack-toolbar]"))
      .toHaveClass("sticky", "top-0", "z-20", "shrink-0", "bg-slate-950");
    expect(document.querySelector('[data-responsive-pack-layout="phone-portrait"]')).toHaveClass("pt-0");
    expect(screen.getByTestId("pack-sequence")).toHaveClass("p-1");
  });

  it.each(["tablet-portrait", "tablet-landscape"] as const)(
    "reserves_the_%s_pack_glow_gutter",
    (responsiveLayout) => {
      render(
        <PackDisplay
          controller={controller()}
          presentation={{ packScale: 1, setPackScale: vi.fn() }}
          onCardHover={vi.fn()}
          responsiveLayout={responsiveLayout}
        />,
      );

      expect(screen.getByTestId("pack-sequence")).toHaveClass("pt-2");
    },
  );

  it("uses compact minus-slider-plus controls only for tablet landscape drafting", () => {
    const { rerender } = render(
      <PackDisplay
        controller={controller()}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
        responsiveLayout="tablet-landscape"
      />,
    );

    const landscapeControls = document.querySelector<HTMLElement>("[data-pack-scale-controls]")!;
    expect(within(landscapeControls).getByText("Pack scale")).toHaveClass("sr-only");
    expect(within(landscapeControls).queryByRole("button", { name: "Reset pack scale" })).not.toBeInTheDocument();
    expect(Array.from(landscapeControls.children).map((child) => child.tagName)).toEqual(["BUTTON", "LABEL", "BUTTON"]);

    rerender(
      <PackDisplay
        controller={controller()}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
        responsiveLayout="tablet-portrait"
      />,
    );
    const portraitControls = document.querySelector<HTMLElement>("[data-pack-scale-controls]")!;
    expect(within(portraitControls).getByText("Pack scale")).toBeInTheDocument();
    expect(within(portraitControls).getByRole("button", { name: "Reset pack scale" })).toBeInTheDocument();
  });

  it.each(["phone-portrait", "phone-landscape"] as const)(
    "disables_mobile_pick_destinations_while_the_%s_workspace_is_open",
    (responsiveLayout) => {
      render(
        <PackDisplay
          controller={controller({ selectedCard: "unknown" })}
          presentation={{ packScale: 1, setPackScale: vi.fn() }}
          onCardHover={vi.fn()}
          responsiveLayout={responsiveLayout}
          mobileWorkspaceOpen
        />,
      );

      expect(screen.getByRole("button", { name: "Deck" })).toBeDisabled();
      expect(screen.getByRole("button", { name: "Sideboard" })).toBeDisabled();
    },
  );

  it("forwards_tablet_touch_pack_gestures_to_the_drag_controller", () => {
    const localDrag = {
      ...dragController,
      handlePointerDown: vi.fn(),
      handlePointerMove: vi.fn(),
      handlePointerUp: vi.fn(),
    };
    const { container } = render(
      <PackDisplay
        controller={controller({ dragController: localDrag })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
        responsiveLayout="tablet-portrait"
      />,
    );

    const packCard = container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    fireEvent.pointerDown(packCard, {
      button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 71, pointerType: "touch",
    });
    fireEvent.pointerMove(packCard, {
      clientX: 30, clientY: 10, isPrimary: true, pointerId: 71, pointerType: "touch",
    });
    fireEvent.pointerUp(packCard, {
      clientX: 30, clientY: 10, isPrimary: true, pointerId: 71, pointerType: "touch",
    });

    expect(localDrag.handlePointerDown).toHaveBeenCalledWith(expect.anything(), expect.anything(), true);
    expect(localDrag.handlePointerMove).toHaveBeenCalled();
    expect(localDrag.handlePointerUp).toHaveBeenCalled();
  });

  it("hides_each_name_footer_after_that_card_image_loads", () => {
    imageState.src = "/card.png";
    const { container } = render(<PackDisplay controller={controller()} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const firstCard = container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const footer = () => firstCard.querySelector(".absolute.bottom-1");

    expect(footer()).toHaveTextContent("Same");
    fireEvent.load(within(firstCard).getByRole("img", { name: "Same" }));

    expect(footer()).not.toBeInTheDocument();
    expect(within(firstCard).getByRole("button", { name: "Pick Same to Deck" })).toHaveClass("sr-only");
    expect(within(firstCard).getByRole("button", { name: "Pick Same to Sideboard" })).toHaveClass("sr-only");
  });

  it("shows_a_face_toggle_and_swaps_a_pack_card_without_selecting_it", () => {
    const doubleFaced = { ...cards[0], name: "Front // Back" };
    const doubleFacedView = { ...view, current_pack: [doubleFaced] };
    const selectCard = vi.fn();
    alternateFaceState.values = {
      "Front // Back": { name: "Back", faceIndex: 1, side: "back" },
    };
    imageState.sources = {
      "Front // Back": "/front.png",
      "": null,
    };
    imageState.faceSources = { "Front // Back:1": "/back.png" };
    const rendered = render(
      <PackDisplay
        controller={controller({ view: doubleFacedView, selectCard })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.queryByText("Hold Ctrl for back face")).not.toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Control" });
    expect(screen.getByRole("img", { name: "Front // Back" })).toHaveAttribute("src", "/front.png");
    fireEvent.keyUp(window, { key: "Control" });
    selectCard.mockClear();

    fireEvent.click(screen.getByRole("button", { name: "Show other face of Front // Back" }));
    expect(screen.getByRole("img", { name: "Back" })).toHaveAttribute("src", "/back.png");
    expect(selectCard).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Show other face of Front // Back" }));
    expect(screen.getByRole("img", { name: "Front // Back" })).toHaveAttribute("src", "/front.png");

    rendered.unmount();
    imageState.sources = { "Front // Back": null, Back: null, "": null };
    imageState.faceSources = { "Front // Back:1": null };
    render(
      <PackDisplay
        controller={controller({ view: doubleFacedView })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "Show other face of Front // Back" })).toBeInTheDocument();
  });

  it("swaps_only_the_double_faced_pack_card_whose_toggle_is_clicked", () => {
    const first = { ...cards[0], name: "First Front // First Back" };
    const second = { ...cards[1], name: "Second Front // Second Back" };
    alternateFaceState.values = {
      [first.name]: { name: "First Back", faceIndex: 1, side: "back" },
      [second.name]: { name: "Second Back", faceIndex: 1, side: "back" },
    };
    imageState.sources = {
      "First Front // First Back": "/first-front.png",
      "Second Front // Second Back": "/second-front.png",
      "": null,
    };
    imageState.faceSources = {
      "First Front // First Back:1": "/first-back.png",
      "Second Front // Second Back:1": "/second-back.png",
    };
    render(
      <PackDisplay
        controller={controller({ view: { ...view, current_pack: [first, second] } })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: `Show other face of ${first.name}` }));
    expect(screen.getByRole("img", { name: "First Back" })).toHaveAttribute("src", "/first-back.png");
    expect(screen.getByRole("img", { name: second.name })).toHaveAttribute("src", "/second-front.png");
  });

  it("discovers_and_swaps_a_cube_cards_back_face_from_its_front_only_name", () => {
    const norman = { ...cards[0], name: "Norman Osborn", set_code: "CUBE" };
    alternateFaceState.values = {
      "Norman Osborn": { name: "Green Goblin", faceIndex: 1, side: "back" },
    };
    imageState.sources = {
      "Norman Osborn": "/norman.png",
      "": null,
    };
    imageState.faceSources = { "Norman Osborn:1": "/goblin.png" };
    render(
      <PackDisplay
        controller={controller({ view: { ...view, current_pack: [norman] } })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Show other face of Norman Osborn" }));
    expect(screen.getByRole("img", { name: "Green Goblin" }))
      .toHaveAttribute("src", "/goblin.png");
  });

  it("does_not_show_a_face_toggle_for_single_image_multi_component_cards", () => {
    const adventureCard = { ...cards[0], name: "Bonecrusher Giant // Stomp" };
    imageState.src = "/adventure.png";
    imageState.isFlip = true;
    alternateFaceState.values = { [adventureCard.name]: null };
    render(
      <PackDisplay
        controller={controller({ view: { ...view, current_pack: [adventureCard] } })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: adventureCard.name })).toHaveAttribute("src", "/adventure.png");
    expect(screen.queryByRole("button", { name: `Show other face of ${adventureCard.name}` })).not.toBeInTheDocument();
  });

  it("dispatches_exact_destination_and_confirms_the_selected_card_on_double_click", async () => {
    const user = userEvent.setup();
    const pickCard = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const selectCard = vi.fn();
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const setPackScale = vi.fn();
    const { container } = render(<PackDisplay controller={controller({ pickCard, selectCard, confirmPick, doubleClickPick: true })} presentation={{ packScale: 0.75, setPackScale }} onCardHover={vi.fn()} />);
    fireEvent.change(screen.getByRole("slider"), { target: { value: "0.8" } });
    expect(screen.getByRole("slider")).toHaveAttribute("min", "0.4");
    expect(screen.getByRole("slider")).toHaveAttribute("max", "2.9");
    expect(screen.getByRole("slider")).toHaveAttribute("step", "0.01");
    fireEvent.click(screen.getByRole("button", { name: "Decrease pack scale" }));
    fireEvent.click(screen.getByRole("button", { name: "Reset pack scale" }));
    fireEvent.click(screen.getByRole("button", { name: "Increase pack scale" }));
    const firstCard = container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    fireEvent.click(within(firstCard).getByRole("button", { name: "Pick Same to Sideboard" }));
    await user.dblClick(screen.getAllByRole("button", { name: "Same" })[0]);
    expect(setPackScale.mock.calls.map(([value]) => value)).toEqual([0.8, 0.65, 1.65, 0.85]);
    expect(pickCard).toHaveBeenNthCalledWith(1, "unknown", "sideboard");
    expect(pickCard).toHaveBeenCalledTimes(1);
    expect(selectCard).toHaveBeenCalledWith("unknown");
    expect(selectCard.mock.invocationCallOrder[0]).toBeLessThan(confirmPick.mock.invocationCallOrder[0]);
    expect(confirmPick).toHaveBeenCalledWith("deck");
  });

  it("selects_on_desktop_pointer_down_so_the_glow_and_double_click_confirmation_are_available", async () => {
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });

    function DesktopPack() {
      const [selectedCard, setSelectedCard] = useState<string | null>(null);
      return (
        <PackDisplay
          controller={controller({ selectedCard, selectCard: setSelectedCard, confirmPick, doubleClickPick: true })}
          presentation={{ packScale: 1, setPackScale: vi.fn() }}
          onCardHover={vi.fn()}
        />
      );
    }

    const rendered = render(<DesktopPack />);
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    fireEvent.pointerDown(cardElement, { button: 0, isPrimary: true, pointerId: 91, pointerType: "mouse" });
    expect(cardElement).toHaveAttribute("data-visual-state", "selected");
    expect(cardElement).toHaveClass("ring-2", "ring-[rgb(3,139,6)]", "shadow-[0_0_4px_2px_rgb(3,139,6)]");

    fireEvent.doubleClick(cardElement);
    await vi.waitFor(() => expect(confirmPick).toHaveBeenCalledWith("deck"));
  });

  it("confirms_the_selected_card_on_a_touch_double_tap", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_000));
    const selectCard = vi.fn();
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const rendered = render(
      <PackDisplay
        controller={controller({ selectCard, confirmPick, doubleClickPick: true })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
      />,
    );
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const button = within(cardElement).getByRole("button", { name: "Same" });

    fireEvent.pointerDown(cardElement, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 21, pointerType: "touch" });
    fireEvent.pointerUp(cardElement, { clientX: 20, clientY: 20, isPrimary: true, pointerId: 21, pointerType: "touch" });
    fireEvent.click(button);
    vi.advanceTimersByTime(150);
    fireEvent.pointerDown(cardElement, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 22, pointerType: "touch" });
    fireEvent.pointerUp(cardElement, { clientX: 20, clientY: 20, isPrimary: true, pointerId: 22, pointerType: "touch" });
    fireEvent.click(button);

    expect(selectCard).toHaveBeenCalledTimes(1);
    expect(selectCard).toHaveBeenCalledWith("unknown");
    await vi.waitFor(() => expect(confirmPick).toHaveBeenCalledWith("deck"));
  });

  it("does_not_pick_from_a_touch_long_press_or_swipe", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2_000));
    const selectCard = vi.fn();
    const confirmPick = vi.fn();
    const onCardHover = vi.fn();
    const rendered = render(
      <PackDisplay
        controller={controller({ selectCard, confirmPick, doubleClickPick: true })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={onCardHover}
      />,
    );
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const button = within(cardElement).getByRole("button", { name: "Same" });

    fireEvent.pointerDown(cardElement, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 31, pointerType: "touch" });
    vi.advanceTimersByTime(500);
    fireEvent.pointerUp(cardElement, { clientX: 20, clientY: 20, isPrimary: true, pointerId: 31, pointerType: "touch" });
    fireEvent.click(button);
    expect(onCardHover).toHaveBeenCalledWith(expect.objectContaining({ name: "Same" }));

    vi.advanceTimersByTime(600);
    fireEvent.pointerDown(cardElement, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 32, pointerType: "touch" });
    fireEvent.pointerMove(cardElement, { clientX: 50, clientY: 20, isPrimary: true, pointerId: 32, pointerType: "touch" });
    fireEvent.pointerUp(cardElement, { clientX: 50, clientY: 20, isPrimary: true, pointerId: 32, pointerType: "touch" });
    fireEvent.click(button);

    expect(selectCard).not.toHaveBeenCalled();
    expect(confirmPick).not.toHaveBeenCalled();
  });

  it("selects_on_pointer_down_but_consumes_drag_compatibility_clicks_and_double_click_before_a_new_deliberate_gesture", async () => {
    let suppression: "none" | "awaiting-click" | "awaiting-double-click" = "none";
    const selectCard = vi.fn();
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const localDrag = {
      ...dragController,
      handlePointerDown: vi.fn(() => { suppression = "none"; }),
      handlePointerMove: vi.fn(() => { suppression = "awaiting-click"; }),
      consumeCompatibilityActivation: vi.fn((event: { kind: "click" | "double-click" }) => {
        if (suppression === "none") return false;
        suppression = event.kind === "double-click" ? "none" : "awaiting-double-click";
        return true;
      }),
    };
    const rendered = render(<PackDisplay controller={controller({ selectCard, confirmPick, doubleClickPick: true, dragController: localDrag })} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const button = within(cardElement).getByRole("button", { name: "Same" });

    fireEvent.pointerDown(cardElement, { button: 0, isPrimary: true, pointerId: 10, pointerType: "mouse" });
    fireEvent.pointerMove(cardElement, { pointerId: 10, pointerType: "mouse" });
    fireEvent.pointerUp(cardElement, { pointerId: 10, pointerType: "mouse" });
    fireEvent.click(button);
    fireEvent.doubleClick(button);
    expect(selectCard).toHaveBeenCalledTimes(1);
    expect(selectCard).toHaveBeenCalledWith("unknown");
    expect(localDrag.handlePointerDown).toHaveBeenCalled();
    expect(confirmPick).not.toHaveBeenCalled();

    fireEvent.pointerDown(cardElement, { button: 0, isPrimary: true, pointerId: 11, pointerType: "mouse" });
    fireEvent.pointerUp(cardElement, { pointerId: 11, pointerType: "mouse" });
    fireEvent.click(button);
    fireEvent.doubleClick(button);
    await vi.waitFor(() => expect(confirmPick).toHaveBeenCalledWith("deck"));
    expect(selectCard).toHaveBeenCalledWith("unknown");
  });

  it("allows_keyboard_selection_after_one_compatibility_click_without_an_in_card_confirmation", () => {
    let suppressCompatibility = true;
    const selectCard = vi.fn();
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const localDrag = {
      ...dragController,
      consumeCompatibilityActivation: vi.fn((activation: { detail: number }) => {
        if (activation.detail === 0) {
          suppressCompatibility = false;
          return false;
        }
        return suppressCompatibility;
      }),
    };
    const initial = controller({ selectCard, confirmPick, doubleClickPick: true, dragController: localDrag });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();
    const firstCard = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const cardButton = within(firstCard).getByRole("button", { name: "Same" });
    expect(firstCard).toHaveClass("select-none", "caret-transparent", "transition-all", "duration-150", "cursor-pointer", "hover:scale-[1.02]", "hover:ring-white/20");

    fireEvent.click(cardButton, { detail: 1, pointerType: "mouse" });
    expect(selectCard).not.toHaveBeenCalled();
    fireEvent.click(cardButton, { detail: 0 });
    expect(selectCard).toHaveBeenCalledWith("unknown");

    rendered.rerender(<PackDisplay controller={{ ...initial, selectedCard: "unknown" }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    expect(firstCard).toHaveClass("ring-2", "ring-[rgb(3,139,6)]", "shadow-[0_0_4px_2px_rgb(3,139,6)]");
    expect(firstCard).not.toHaveClass("scale-105");
    expect(within(firstCard).queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();
    const confirmButton = screen.getByRole("button", { name: "Confirm Pick" });
    for (const className of menuButtonClass({ tone: "emerald", size: "sm" }).split(" ")) {
      expect(confirmButton).toHaveClass(className);
    }
    expect(confirmButton).toHaveClass("!min-h-9", "select-none", "!py-0", "caret-transparent");
    expect(confirmButton.parentElement).toHaveAttribute("data-pack-status-controls");
    expect(confirmButton.closest("[data-pack-toolbar]")).toHaveClass("flex-nowrap", "overflow-x-auto");
    fireEvent.click(confirmButton);
    expect(confirmPick).toHaveBeenCalledWith("deck");
    expect(within(firstCard).getByText("Same", { selector: "div > span" })).toBeInTheDocument();
  });

  it("blocks_incomplete_duplicate_and_stale_effect_sources_but_dispatches_a_complete_live_pair", async () => {
    const effectView = { ...view, draft_effects: [effectCard] };
    const pickCard = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const pickEffect = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const localDrag = { ...dragController, handlePointerDown: vi.fn() };
    const presentation = { packScale: 1, setPackScale: vi.fn() };

    const incomplete = render(<PackDisplay controller={controller({ view: effectView, pickCard, pickCardWithDraftEffect: pickEffect, doubleClickPick: true, dragController: localDrag })} presentation={presentation} onCardHover={vi.fn()} enableDraftEffects />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Effect" }));
    const incompleteCard = incomplete.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    fireEvent.click(within(incompleteCard).getByRole("button", { name: "Pick Same to Deck" }));
    fireEvent.doubleClick(within(incompleteCard).getByRole("button", { name: "Same" }));
    fireEvent.pointerDown(incompleteCard, { button: 0, isPrimary: true, pointerId: 1, pointerType: "mouse" });
    expect(pickCard).not.toHaveBeenCalled();
    expect(pickEffect).not.toHaveBeenCalled();
    expect(localDrag.handlePointerDown).not.toHaveBeenCalled();
    incomplete.unmount();

    const duplicate = render(<PackDisplay controller={controller({ view: effectView, selectedCard: "unknown", pickCard, pickCardWithDraftEffect: pickEffect })} presentation={presentation} onCardHover={vi.fn()} enableDraftEffects />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Effect" }));
    const duplicateCard = duplicate.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    fireEvent.click(within(duplicateCard).getByRole("button", { name: "Same" }));
    fireEvent.click(within(duplicateCard).getByRole("button", { name: "Pick Same to Sideboard" }));
    expect(pickEffect).not.toHaveBeenCalled();
    duplicate.unmount();

    const stale = render(<PackDisplay controller={controller({ view: effectView, selectedCard: "missing", pickCard, pickCardWithDraftEffect: pickEffect })} presentation={presentation} onCardHover={vi.fn()} enableDraftEffects />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Effect" }));
    const staleSecond = stale.container.querySelector<HTMLElement>('[data-instance-id="common"]')!;
    fireEvent.click(within(staleSecond).getByRole("button", { name: "Same" }));
    fireEvent.click(within(staleSecond).getByRole("button", { name: "Pick Same to Deck" }));
    expect(pickEffect).not.toHaveBeenCalled();
    stale.unmount();

    const complete = render(<PackDisplay controller={controller({ view: effectView, selectedCard: "unknown", pickCard, pickCardWithDraftEffect: pickEffect, doubleClickPick: true, dragController: localDrag })} presentation={presentation} onCardHover={vi.fn()} enableDraftEffects />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Effect" }));
    const first = complete.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const second = complete.container.querySelector<HTMLElement>('[data-instance-id="common"]')!;
    fireEvent.click(within(second).getByRole("button", { name: "Same" }));
    fireEvent.click(within(first).getByRole("button", { name: "Pick Same to Sideboard" }));
    fireEvent.doubleClick(within(second).getByRole("button", { name: "Same" }));
    fireEvent.pointerDown(first, { button: 0, isPrimary: true, pointerId: 2, pointerType: "mouse" });
    await vi.waitFor(() => expect(pickEffect).toHaveBeenCalledTimes(2));
    expect(pickEffect).toHaveBeenNthCalledWith(1, "effect", ["unknown", "common"], "sideboard");
    expect(pickEffect).toHaveBeenNthCalledWith(2, "effect", ["unknown", "common"], "deck");
    expect(localDrag.handlePointerDown).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({
      kind: "draft-effect",
      authorityId: "effect",
      instanceIds: ["unknown", "common"],
    }));
    expect(pickCard).not.toHaveBeenCalled();
  });

  it("renders_all_six_visual_states_with_drag_admission_waiting_and_token_precedence", () => {
    vi.useFakeTimers();
    let source!: PackDropSource;
    const localDrag = {
      ...dragController,
      handlePointerDown: vi.fn((_event, nextSource: PackDropSource) => { source = nextSource; }),
    };
    const initial = controller({ dragController: localDrag });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const currentCard = () => rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    expect(currentCard()).toHaveAttribute("data-visual-state", "default");
    rendered.rerender(<PackDisplay controller={{ ...initial, selectedCard: "unknown" }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    expect(currentCard()).toHaveAttribute("data-visual-state", "selected");

    fireEvent.pointerDown(currentCard(), { button: 0, isPrimary: true, pointerId: 90, pointerType: "mouse" });
    const olderSource = source;
    act(() => olderSource.onAdmission({ kind: "dispatch", requestToken: "older", interactionGeneration: 4 }));
    expect(currentCard()).toHaveAttribute("data-visual-state", "submitting");

    fireEvent.pointerDown(currentCard(), { button: 0, isPrimary: true, pointerId: 91, pointerType: "mouse" });
    const newerSource = source;
    act(() => newerSource.onAdmission({ kind: "dispatch", requestToken: "newer", interactionGeneration: 4 }));
    act(() => olderSource.onSettled({ kind: "outcome", outcome: { status: "rejected", reason: "invalid-request" } }));
    expect(currentCard()).toHaveAttribute("data-visual-state", "submitting");

    const pendingIntent = { kind: "pick" as const, instanceIds: ["unknown"] as const, destination: "sideboard" as const, placementHint: { column: 0 } };
    rendered.rerender(<PackDisplay controller={{ ...initial, selectedCard: "unknown", pendingIntent, interactionLocked: true }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    expect(currentCard()).toHaveAttribute("data-visual-state", "waiting");
    act(() => newerSource.onSettled({ kind: "outcome", outcome: { status: "rejected", reason: "invalid-request" } }));
    expect(currentCard()).toHaveAttribute("data-visual-state", "waiting");

    rendered.rerender(<PackDisplay controller={{ ...initial, selectedCard: "unknown" }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    expect(currentCard()).toHaveAttribute("data-visual-state", "failure-restored");

    fireEvent.pointerDown(currentCard(), { button: 0, isPrimary: true, pointerId: 92, pointerType: "mouse" });
    const acknowledgedSource = source;
    act(() => acknowledgedSource.onAdmission({ kind: "dispatch", requestToken: "acknowledged", interactionGeneration: 4 }));
    rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...view, current_pack: [cards[1]] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    act(() => acknowledgedSource.onSettled({ kind: "outcome", outcome: { status: "acknowledged" } }));
    expect(rendered.container.querySelector('[data-instance-id="unknown"][data-visual-state="leaving"]')).toBeInTheDocument();
  });

  it("commits_submitting_before_real_drag_dispatch_then_renders_waiting_after_lock_publication", async () => {
    let resolveOutcome!: (outcome: { status: "ignored"; reason: "busy" }) => void;
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => {
      expect(document.querySelector('[data-instance-id="unknown"]')).toHaveAttribute("data-visual-state", "submitting");
      return {
        requestToken: request.requestToken,
        interactionGeneration: request.interactionGeneration,
        outcome: new Promise((resolve) => { resolveOutcome = resolve; }),
      };
    });
    const rendered = render(<RealDragPackHarness onDrop={onDrop} />);
    const source = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const target = screen.getByTestId("real-drag-target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => ({ left: 0, top: 0, right: 200, bottom: 200, width: 200, height: 200, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;

    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 94, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 94, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 94, pointerType: "mouse" });

    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(source).toHaveAttribute("data-visual-state", "waiting");
    await act(async () => resolveOutcome({ status: "ignored", reason: "busy" }));
    fireEvent.click(screen.getByRole("button", { name: "unlock" }));
    expect(source).toHaveAttribute("data-visual-state", "default");
  });

  it("dispatches_a_tablet_touch_pack_drag_to_the_collapsed_sideboard", async () => {
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: request.interactionGeneration,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    const rendered = render(<RealDragPackHarness responsiveLayout="tablet-portrait" onDrop={onDrop} />);
    const source = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const target = screen.getByTestId("real-drag-target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => ({ left: 100, top: 0, right: 300, bottom: 200, width: 200, height: 200, x: 100, y: 0, toJSON: () => ({}) }) as DOMRect;

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 95, pointerType: "touch" });
    fireEvent.pointerMove(source, { clientX: 120, clientY: 20, pointerId: 95, pointerType: "touch" });
    fireEvent.pointerUp(source, { clientX: 120, clientY: 20, pointerId: 95, pointerType: "touch" });

    expect(onDrop).toHaveBeenCalledWith(expect.objectContaining({
      destination: "sideboard",
      placementHint: { column: 0 },
    }));
  });

  it.each([
    { name: "dispatch error", settlement: { kind: "error" } as const },
    { name: "clean unowned busy", settlement: { kind: "outcome", outcome: { status: "ignored", reason: "busy" } } as const },
  ])("clears_submitting_after_$name", ({ settlement }) => {
    let source!: PackDropSource;
    const localDrag = {
      ...dragController,
      handlePointerDown: vi.fn((_event, nextSource: PackDropSource) => { source = nextSource; }),
    };
    const rendered = render(<PackDisplay controller={controller({ dragController: localDrag })} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const first = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    fireEvent.pointerDown(first, { button: 0, isPrimary: true, pointerId: 93, pointerType: "mouse" });
    act(() => source.onAdmission({ kind: "dispatch", requestToken: "transient", interactionGeneration: 4 }));
    expect(first).toHaveAttribute("data-visual-state", "submitting");
    act(() => source.onSettled(settlement));
    expect(first).toHaveAttribute("data-visual-state", "default");
  });

  it("keeps_newer_failure_state_when_older_results_and_timers_arrive", async () => {
    vi.useFakeTimers();
    let resolveFirst!: (value: { status: "rejected"; reason: "invalid-request" }) => void;
    let resolveSecond!: (value: { status: "rejected"; reason: "invalid-request" }) => void;
    const pickCard = vi.fn()
      .mockReturnValueOnce(new Promise((resolve) => { resolveFirst = resolve; }))
      .mockReturnValueOnce(new Promise((resolve) => { resolveSecond = resolve; }));
    const rendered = render(<PackDisplay controller={controller({ pickCard })} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const first = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const destination = within(first).getByRole("button", { name: "Pick Same to Deck" });

    fireEvent.click(destination);
    fireEvent.click(destination);
    await act(async () => resolveSecond({ status: "rejected", reason: "invalid-request" }));
    expect(first).toHaveAttribute("data-visual-state", "failure-restored");
    await act(async () => resolveFirst({ status: "rejected", reason: "invalid-request" }));
    act(() => vi.advanceTimersByTime(1499));
    expect(first).toHaveAttribute("data-visual-state", "failure-restored");
    act(() => vi.advanceTimersByTime(1));
    expect(first).toHaveAttribute("data-visual-state", "default");
  });

  it("cancels_visual_work_on_generation_replacement_and_reselection", async () => {
    vi.useFakeTimers();
    let resolvePick!: (value: { status: "rejected"; reason: "invalid-request" }) => void;
    const pickCard = vi.fn().mockReturnValue(new Promise((resolve) => { resolvePick = resolve; }));
    const initial = controller({ pickCard });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const first = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    fireEvent.click(within(first).getByRole("button", { name: "Pick Same to Deck" }));
    rendered.rerender(<PackDisplay controller={{ ...initial, interactionGeneration: 5 }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    await act(async () => resolvePick({ status: "rejected", reason: "invalid-request" }));
    expect(first).toHaveAttribute("data-visual-state", "default");

    const immediate = controller({ pickCard: vi.fn().mockResolvedValue({ status: "rejected", reason: "invalid-request" }), interactionGeneration: 5 });
    rendered.rerender(<PackDisplay controller={immediate} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const current = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    fireEvent.click(within(current).getByRole("button", { name: "Pick Same to Deck" }));
    await act(async () => Promise.resolve());
    expect(current).toHaveAttribute("data-visual-state", "failure-restored");
    fireEvent.click(within(current).getByRole("button", { name: "Same" }));
    expect(current).toHaveAttribute("data-visual-state", "default");
    act(() => vi.runAllTimers());
    expect(current).toHaveAttribute("data-visual-state", "default");
  });

  it("retires_acknowledged_departures_on_reappearance_and_preserves_effect_source_order", async () => {
    vi.useFakeTimers();
    const effectView = { ...view, draft_effects: [effectCard] };
    const pickEffect = vi.fn().mockResolvedValue({ status: "acknowledged" });
    const initial = controller({ view: effectView, selectedCard: "unknown", pickCardWithDraftEffect: pickEffect });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Effect" }));
    const second = rendered.container.querySelector<HTMLElement>('[data-instance-id="common"]')!;
    fireEvent.click(within(second).getByRole("button", { name: "Same" }));
    const first = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    fireEvent.click(within(first).getByRole("button", { name: "Pick Same to Deck" }));
    rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...effectView, current_pack: [] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
    await act(async () => Promise.resolve());
    expect([...rendered.container.querySelectorAll('[data-visual-state="leaving"]')].map((node) => node.getAttribute("data-instance-id"))).toEqual(["unknown", "common"]);

    rendered.rerender(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
    expect(rendered.container.querySelectorAll('[data-visual-state="leaving"]')).toHaveLength(0);
    act(() => vi.runAllTimers());
    expect([...rendered.container.querySelectorAll("[data-instance-id]")].map((node) => node.getAttribute("data-instance-id"))).toEqual(["unknown", "common"]);
  });

  it.each([
    { reappeared: [cards[0]], retained: "common" },
    { reappeared: [cards[1]], retained: "unknown" },
  ])("retires_only_the_surviving_departure_after_partial_reappearance_$retained", async ({ reappeared, retained }) => {
    vi.useFakeTimers();
    const effectView = { ...view, draft_effects: [effectCard] };
    const initial = controller({
      view: effectView,
      selectedCard: "unknown",
      pickCardWithDraftEffect: vi.fn().mockResolvedValue({ status: "acknowledged" }),
    });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Effect" }));
    fireEvent.click(within(rendered.container.querySelector<HTMLElement>('[data-instance-id="common"]')!).getByRole("button", { name: "Same" }));
    fireEvent.click(within(rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!).getByRole("button", { name: "Pick Same to Deck" }));
    rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...effectView, current_pack: [] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
    await act(async () => Promise.resolve());
    expect(rendered.container.querySelectorAll('[data-visual-state="leaving"]')).toHaveLength(2);

    rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...effectView, current_pack: reappeared } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
    expect(rendered.container.querySelector('[data-visual-state="leaving"]')).toHaveAttribute("data-instance-id", retained);
    act(() => vi.advanceTimersByTime(180));
    expect(rendered.container.querySelector('[data-visual-state="leaving"]')).not.toBeInTheDocument();
    expect(rendered.container.querySelectorAll(`[data-instance-id="${reappeared[0].instance_id}"]`)).toHaveLength(1);
  });

  it("retires_all_departures_without_reappearance_and_clears_them_on_generation_or_unmount", async () => {
    vi.useFakeTimers();
    const effectView = { ...view, draft_effects: [effectCard] };
    const initial = controller({
      view: effectView,
      selectedCard: "unknown",
      pickCardWithDraftEffect: vi.fn().mockResolvedValue({ status: "acknowledged" }),
    });
    const createDeparture = async () => {
      const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
      fireEvent.click(screen.getByRole("checkbox", { name: "Effect" }));
      fireEvent.click(within(rendered.container.querySelector<HTMLElement>('[data-instance-id="common"]')!).getByRole("button", { name: "Same" }));
      fireEvent.click(within(rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!).getByRole("button", { name: "Pick Same to Deck" }));
      rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...effectView, current_pack: [] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
      await act(async () => Promise.resolve());
      expect(rendered.container.querySelectorAll('[data-visual-state="leaving"]')).toHaveLength(2);
      expect(vi.getTimerCount()).toBe(2);
      return rendered;
    };

    const elapsed = await createDeparture();
    act(() => vi.advanceTimersByTime(180));
    expect(elapsed.container.querySelector('[data-visual-state="leaving"]')).not.toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(0);
    elapsed.unmount();

    const replaced = await createDeparture();
    replaced.rerender(<PackDisplay controller={{ ...initial, interactionGeneration: 5, view: { ...effectView, current_pack: [] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} enableDraftEffects />);
    expect(replaced.container.querySelector('[data-visual-state="leaving"]')).not.toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(0);
    replaced.unmount();

    const unmounted = await createDeparture();
    unmounted.unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("commits_one_leaving_render_before_zero_delay_reduced_motion_retirement", async () => {
    vi.useFakeTimers();
    const originalMatchMedia = window.matchMedia;
    window.matchMedia = vi.fn().mockImplementation((query: string) => ({
      matches: query.includes("prefers-reduced-motion"), media: query, onchange: null,
      addEventListener: vi.fn(), removeEventListener: vi.fn(), addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
    }));
    const pickCard = vi.fn().mockResolvedValue({ status: "acknowledged" });
    const initial = controller({ pickCard });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const first = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    fireEvent.click(within(first).getByRole("button", { name: "Pick Same to Deck" }));
    rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...view, current_pack: [cards[1]] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    await act(async () => Promise.resolve());
    expect(rendered.container.querySelector('[data-visual-state="leaving"]')).toHaveAttribute("data-instance-id", "unknown");
    act(() => vi.runOnlyPendingTimers());
    expect(rendered.container.querySelector('[data-visual-state="leaving"]')).not.toBeInTheDocument();
    window.matchMedia = originalMatchMedia;
  });
});
