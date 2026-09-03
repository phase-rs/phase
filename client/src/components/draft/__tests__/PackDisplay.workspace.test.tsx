import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef, useState } from "react";

import type { DraftPlayerView } from "../../../adapter/draft-adapter";
import type { PackDisplayController, PackDropSource } from "../PackDisplay";
import type { DraftDropDispatch, DraftDropRequest, DraftPickInteractionSnapshot, WorkspaceDragSource } from "../workspace/useDraftWorkspaceDrag";
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
  launch_capability: "None",
  current_pack: cards, pool: [], draft_effects: [], pool_groups: {
    color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [], type_filter_options: [], color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: ["common", "rarity_other"] },
    workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
  }, seats: [], cards_per_pack: 14, required_pick_count: 1, pick_selection_mode: "Direct", pick_steps_per_pack: 14, pack_count: 3, min_deck_size: 40, addable_cards: [], timer_remaining_ms: null,
  standings: [], current_round: 0, next_pairing_round: 1, tournament_format: "Swiss", pod_policy: "Competitive", pairings: [], match_config: { match_type: "Bo1" },
} satisfies DraftPlayerView;
const dragController = {
  handlePointerDown: vi.fn(), handlePointerMove: vi.fn(), handlePointerUp: vi.fn(), handlePointerCancel: vi.fn(),
  handleLostPointerCapture: vi.fn(), consumeCompatibilityActivation: () => false,
};

type WorkspaceController = Extract<PackDisplayController, { kind: "local-workspace" }>;
type PickCard = WorkspaceController["pickCard"];
type PickCardStep = WorkspaceController["pickCardStep"];
type ConfirmPick = WorkspaceController["confirmPick"];

function controller(overrides: Partial<WorkspaceController> = {}): WorkspaceController {
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

const commanderPickTwoCards = [
  ...cards,
  { instance_id: "third", name: "Third", set_code: "TST", collector_number: "4", rarity: "uncommon", colors: [], cmc: 1, type_line: "Card" },
];
const commanderPickTwoView = {
  ...view,
  kind: "CommanderDraft" as const,
  required_pick_count: 2,
  pick_selection_mode: "Ordered" as const,
  current_pack: commanderPickTwoCards,
};

function CommanderPickTwoPack({
  pickCard = async () => ({ status: "ignored" as const, reason: "busy" as const }),
  pickCardStep = async () => ({ status: "ignored" as const, reason: "busy" as const }),
  confirmPick = async () => ({ status: "ignored" as const, reason: "busy" as const }),
  doubleClickPick = true,
  requiredPickCount = 2,
  dragController: localDrag = dragController,
  responsiveLayout = "desktop",
}: {
  pickCard?: PickCard;
  pickCardStep?: PickCardStep;
  confirmPick?: ConfirmPick;
  doubleClickPick?: boolean;
  requiredPickCount?: number;
  dragController?: typeof dragController;
  responsiveLayout?: "desktop" | "phone-portrait" | "tablet-portrait";
}) {
  const [selectedCard, setSelectedCard] = useState<string | null>(null);
  return (
    <PackDisplay
      controller={controller({
        view: { ...commanderPickTwoView, required_pick_count: requiredPickCount },
        selectedCard,
        selectCard: setSelectedCard,
        pickCard,
        pickCardStep,
        confirmPick,
        doubleClickPick,
        dragController: localDrag,
      })}
      presentation={{ packScale: 1, setPackScale: vi.fn() }}
      onCardHover={vi.fn()}
      responsiveLayout={responsiveLayout}
    />
  );
}

function WorkspaceDragCommanderPickTwoHarness({
  pickCardStep,
  onCompatibilityDoubleClick = vi.fn(),
}: {
  pickCardStep: PickCardStep;
  onCompatibilityDoubleClick?: () => void;
}) {
  const [selectedCard, setSelectedCard] = useState<string | null>(null);
  const drag = useDraftWorkspaceDrag({
    enabled: true,
    readPickInteraction: () => ({ interactionGeneration: 4, pickInteractionLocked: false, pendingPickIntent: null }),
    subscribePickInteraction: () => () => undefined,
    onDrop: () => { throw new Error("Workspace drags do not dispatch picks."); },
    resolveCollapsedSideboardColumn: () => 0,
  });
  const workspaceSource: WorkspaceDragSource = {
    kind: "workspace",
    instanceIds: ["workspace-a"],
    cards: [cards[0]],
    previewWidth: 146,
    previewHeight: 204,
    onDrop: () => true,
  };
  return (
    <>
      <div
        data-testid="workspace-drag-source"
        onPointerDown={(event) => drag.handleWorkspacePointerDown(event, workspaceSource)}
        onPointerMove={drag.handlePointerMove}
        onPointerUp={drag.handlePointerUp}
        onPointerCancel={drag.handlePointerCancel}
        onLostPointerCapture={drag.handleLostPointerCapture}
        onClick={(event) => {
          const pointerEvent = event.nativeEvent as MouseEvent & { readonly pointerId?: number; readonly pointerType?: string };
          drag.consumeCompatibilityActivation({
            kind: "click",
            detail: event.detail,
            pointerId: pointerEvent.pointerId ?? null,
            ...(pointerEvent.pointerType === undefined ? {} : { pointerType: pointerEvent.pointerType }),
            surface: "workspace",
            sourceInstanceId: workspaceSource.instanceIds[0],
          });
        }}
        onDoubleClick={(event) => {
          const pointerEvent = event.nativeEvent as MouseEvent & { readonly pointerId?: number; readonly pointerType?: string };
          if (!drag.consumeCompatibilityActivation({
            kind: "double-click",
            detail: event.detail,
            pointerId: pointerEvent.pointerId ?? null,
            ...(pointerEvent.pointerType === undefined ? {} : { pointerType: pointerEvent.pointerType }),
            surface: "workspace",
            sourceInstanceId: workspaceSource.instanceIds[0],
          })) onCompatibilityDoubleClick();
        }}
      />
      <PackDisplay
        controller={controller({
          view: commanderPickTwoView,
          selectedCard,
          selectCard: setSelectedCard,
          pickCardStep,
          dragController: drag,
        })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
        responsiveLayout="desktop"
      />
      <div data-testid="workspace-drag-target" ref={drag.registerCollapsedSideboard} />
    </>
  );
}

function firePointerActivation(
  element: Element,
  type: "click" | "dblclick",
  { detail, pointerId, pointerType }: { detail: number; pointerId: number; pointerType: string },
) {
  fireEvent(element, new PointerEvent(type, { bubbles: true, detail, pointerId, pointerType }));
}

function RealDragPackHarness({
  onDrop,
  confirmPick,
  onSelectionChange,
  viewOverride,
  advancedView,
  responsiveLayout = "desktop",
}: {
  onDrop(request: DraftDropRequest): DraftDropDispatch;
  confirmPick?: ConfirmPick;
  onSelectionChange?(instanceId: string | null): void;
  viewOverride?: DraftPlayerView;
  advancedView?: DraftPlayerView;
  responsiveLayout?: "desktop" | "tablet-portrait";
}) {
  const [selectedCard, setSelectedCard] = useState<string | null>(null);
  const [renderedView, setRenderedView] = useState(viewOverride ?? view);
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
  const selectCard = (instanceId: string | null) => {
    onSelectionChange?.(instanceId);
    setSelectedCard(instanceId);
  };
  return (
    <>
      <PackDisplay
        controller={controller({
          view: renderedView,
          dragController: drag,
          pendingIntent: interaction.pendingPickIntent,
          interactionLocked: interaction.pickInteractionLocked,
          selectedCard,
          selectCard,
          ...(confirmPick === undefined ? {} : { confirmPick }),
        })}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
        responsiveLayout={responsiveLayout}
      />
      <div data-testid="real-drag-target" ref={drag.registerCollapsedSideboard} />
      {advancedView !== undefined && <button type="button" onClick={() => setRenderedView(advancedView)}>advance pack</button>}
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

  it("uses_static_selected_pack_feedback_and_a_static_card_area_drop_glow", () => {
    const css = readFileSync(resolve(process.cwd(), "src/index.css"), "utf8");
    const dropGlow = css.match(/@utility draft-card-area-drop-active \{[\s\S]*?\n\}/)?.[0];

    expect(css).toContain("--color-arcane: #38bdf8");
    expect(css).not.toContain("@keyframes draft-pack-selected-glow");
    expect(dropGlow).toContain("background-color: rgb(255 255 255 / 0.08)");
    expect(dropGlow).toContain("inset 0 0 0 1px rgb(255 255 255 / 0.92)");
    expect(dropGlow).toContain("0 0 18px 2px rgb(255 255 255 / 0.42)");
    expect(dropGlow).not.toMatch(/\b(border|transform|animation)\b/);
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
      "min-h-9",
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
    expect(within(firstCard).getByText("Same")).toHaveClass("text-white/50");
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

  it("keeps_resolved_pack_images_free_of_name_footers_before_native_load", () => {
    imageState.src = "/card.png";
    const { container } = render(<PackDisplay controller={controller()} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const firstCard = container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    expect(within(firstCard).getByRole("img", { name: "Same" })).toHaveAttribute("src", "/card.png");
    expect(firstCard.querySelector(".absolute.bottom-1")).not.toBeInTheDocument();
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

  it("selects_on_desktop_click_after_pointer_down_sets_up_drag", async () => {
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const localDrag = { ...dragController, handlePointerDown: vi.fn() };

    function DesktopPack() {
      const [selectedCard, setSelectedCard] = useState<string | null>(null);
      return (
        <PackDisplay
          controller={controller({ selectedCard, selectCard: setSelectedCard, confirmPick, doubleClickPick: true, dragController: localDrag })}
          presentation={{ packScale: 1, setPackScale: vi.fn() }}
          onCardHover={vi.fn()}
        />
      );
    }

    const rendered = render(<DesktopPack />);
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    fireEvent.pointerDown(cardElement, { button: 0, isPrimary: true, pointerId: 91, pointerType: "mouse" });
    expect(cardElement).toHaveAttribute("data-visual-state", "default");
    expect(localDrag.handlePointerDown).toHaveBeenCalledWith(expect.anything(), expect.anything());
    fireEvent.click(within(cardElement).getByRole("button", { name: "Same" }));
    expect(cardElement).toHaveAttribute("data-visual-state", "selected");
    expect(within(cardElement).getByRole("button", { name: "Same" })).toHaveAttribute("aria-pressed", "true");
    expect(cardElement).toHaveClass(
      "transition-transform",
      "duration-150",
      "ring-2",
      "ring-arcane",
      "shadow-[0_0_7px_3px_#38bdf8]",
    );
    expect(cardElement).not.toHaveClass("motion-safe:animate-[draft-pack-selected-glow_4.8s_ease-in-out_infinite]");
    expect(cardElement).not.toHaveClass("transition-all");
    expect(cardElement).not.toHaveClass("!duration-0");

    fireEvent.doubleClick(cardElement);
    await vi.waitFor(() => expect(confirmPick).toHaveBeenCalledWith("deck"));
  });

  it("selects_a_desktop_pack_card_when_pointer_capture_retargets_its_click_to_the_card_shell", async () => {
    const onDrop = vi.fn();
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const onSelectionChange = vi.fn();
    const rendered = render(
      <RealDragPackHarness
        onDrop={onDrop as never}
        confirmPick={confirmPick}
        onSelectionChange={onSelectionChange}
      />,
    );
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const activation = within(cardElement).getByRole("button", { name: "Same" });
    cardElement.setPointerCapture = vi.fn();
    cardElement.releasePointerCapture = vi.fn();

    fireEvent.pointerDown(activation, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 97, pointerType: "mouse" });
    fireEvent.pointerUp(cardElement, { clientX: 10, clientY: 10, isPrimary: true, pointerId: 97, pointerType: "mouse" });
    firePointerActivation(cardElement, "click", { detail: 1, pointerId: 97, pointerType: "mouse" });

    expect(onDrop).not.toHaveBeenCalled();
    expect(onSelectionChange).toHaveBeenCalledTimes(1);
    expect(onSelectionChange).toHaveBeenCalledWith("unknown");
    expect(cardElement).toHaveAttribute("data-visual-state", "selected");
    expect(cardElement).toHaveClass("ring-arcane", "shadow-[0_0_7px_3px_#38bdf8]");
    expect(screen.getByRole("button", { name: "Confirm Pick" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));
    await vi.waitFor(() => expect(confirmPick).toHaveBeenCalledWith("deck"));
    expect(onSelectionChange).toHaveBeenCalledTimes(1);
  });

  it("selects_once_when_an_ordinary_desktop_click_stays_on_the_nested_pack_activation", () => {
    const onDrop = vi.fn();
    const onSelectionChange = vi.fn();
    const rendered = render(
      <RealDragPackHarness onDrop={onDrop as never} onSelectionChange={onSelectionChange} />,
    );
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const activation = within(cardElement).getByRole("button", { name: "Same" });
    cardElement.setPointerCapture = vi.fn();
    cardElement.releasePointerCapture = vi.fn();

    fireEvent.pointerDown(activation, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 98, pointerType: "mouse" });
    fireEvent.pointerUp(cardElement, { clientX: 10, clientY: 10, isPrimary: true, pointerId: 98, pointerType: "mouse" });
    firePointerActivation(activation, "click", { detail: 1, pointerId: 98, pointerType: "mouse" });

    expect(onDrop).not.toHaveBeenCalled();
    expect(onSelectionChange).toHaveBeenCalledTimes(1);
    expect(onSelectionChange).toHaveBeenCalledWith("unknown");
    expect(cardElement).toHaveAttribute("data-visual-state", "selected");
    expect(cardElement).toHaveClass("ring-arcane", "shadow-[0_0_7px_3px_#38bdf8]");
    expect(screen.getByRole("button", { name: "Confirm Pick" })).toBeInTheDocument();
  });

  it("does_not_select_when_the_desktop_alternate_face_control_is_activated", () => {
    const doubleFaced = { ...cards[0], name: "Front // Back" };
    const onDrop = vi.fn();
    const onSelectionChange = vi.fn();
    alternateFaceState.values = {
      "Front // Back": { name: "Back", faceIndex: 1, side: "back" },
    };
    imageState.sources = {
      "Front // Back": "/front.png",
      "": null,
    };
    imageState.faceSources = { "Front // Back:1": "/back.png" };
    render(
      <RealDragPackHarness
        onDrop={onDrop as never}
        onSelectionChange={onSelectionChange}
        viewOverride={{ ...view, current_pack: [doubleFaced, cards[1]] }}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Show other face of Front // Back" }));

    expect(screen.getByRole("img", { name: "Back" })).toHaveAttribute("src", "/back.png");
    expect(onDrop).not.toHaveBeenCalled();
    expect(onSelectionChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();
  });

  it("does_not_select_from_a_retargeted_mouse_click_on_tablet_portrait", () => {
    const onDrop = vi.fn();
    const onSelectionChange = vi.fn();
    const rendered = render(
      <RealDragPackHarness
        onDrop={onDrop as never}
        onSelectionChange={onSelectionChange}
        responsiveLayout="tablet-portrait"
      />,
    );
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const activation = within(cardElement).getByRole("button", { name: "Same" });
    cardElement.setPointerCapture = vi.fn();
    cardElement.releasePointerCapture = vi.fn();

    fireEvent.pointerDown(activation, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 99, pointerType: "mouse" });
    fireEvent.pointerUp(cardElement, { clientX: 10, clientY: 10, isPrimary: true, pointerId: 99, pointerType: "mouse" });
    firePointerActivation(cardElement, "click", { detail: 1, pointerId: 99, pointerType: "mouse" });

    expect(onDrop).not.toHaveBeenCalled();
    expect(onSelectionChange).not.toHaveBeenCalled();
    expect(cardElement).toHaveAttribute("data-visual-state", "default");
    expect(screen.queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();
  });

  it.each(["mouse", "pen"] as const)("selects_a_desktop_%s_pack_card_after_a_no_target_drag_release", async (pointerType) => {
    const onDrop = vi.fn();
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const rendered = render(<RealDragPackHarness onDrop={onDrop as never} confirmPick={confirmPick} />);
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const target = screen.getByTestId("real-drag-target");
    cardElement.setPointerCapture = vi.fn();
    cardElement.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => ({ left: 100, top: 0, right: 300, bottom: 200, width: 200, height: 200, x: 100, y: 0, toJSON: () => ({}) }) as DOMRect;

    fireEvent.pointerDown(cardElement, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 96, pointerType });
    fireEvent.pointerMove(cardElement, { clientX: 30, clientY: 30, pointerId: 96, pointerType });
    fireEvent.pointerUp(cardElement, { clientX: 30, clientY: 30, pointerId: 96, pointerType });
    expect(onDrop).not.toHaveBeenCalled();

    firePointerActivation(within(cardElement).getByRole("button", { name: "Same" }), "click", {
      detail: 1, pointerId: 96, pointerType,
    });
    expect(cardElement).toHaveAttribute("data-visual-state", "selected");
    expect(cardElement).toHaveClass("ring-2", "ring-arcane", "shadow-[0_0_7px_3px_#38bdf8]");

    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));
    await vi.waitFor(() => expect(confirmPick).toHaveBeenCalledWith("deck"));
  });

  it.each([
    ["desktop", true],
    ["phone-portrait", false],
    ["tablet-landscape", false],
  ] as const)("uses_the_desktop_hover_scale_only_for_%s", (responsiveLayout, hasDesktopHover) => {
    const rendered = render(
      <PackDisplay
        controller={controller()}
        presentation={{ packScale: 1, setPackScale: vi.fn() }}
        onCardHover={vi.fn()}
        responsiveLayout={responsiveLayout}
      />,
    );
    const packCard = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    expect(packCard).toHaveClass("cursor-pointer", "hover:ring-white/20");
    expect(packCard.classList.contains("hover:scale-[1.05]")).toBe(hasDesktopHover);
    expect(packCard.classList.contains("hover:scale-[1.08]")).toBe(false);
  });

  it("keeps_commander_pick_two_selection_in_click_order_and_submits_that_order_manually", () => {
    const pickCardStep = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const rendered = render(<CommanderPickTwoPack pickCardStep={pickCardStep} />);
    const card = (instanceId: string) => rendered.container.querySelector<HTMLElement>(`[data-instance-id="${instanceId}"]`)!;
    const activate = (instanceId: string, name: string) => fireEvent.click(within(card(instanceId)).getByRole("button", { name }));

    activate("unknown", "Same"); // [] + A -> [A]
    expect(card("unknown")).toHaveAttribute("data-visual-state", "selected");

    activate("unknown", "Same"); // [A] + A -> []
    expect(card("unknown")).toHaveAttribute("data-visual-state", "default");

    activate("unknown", "Same"); // [] + A -> [A]
    activate("common", "Same"); // [A] + B -> [A, B]
    expect(card("unknown")).toHaveAttribute("data-visual-state", "selected");
    expect(card("common")).toHaveAttribute("data-visual-state", "selected");

    activate("common", "Same"); // [A, B] + B -> [A]
    expect(card("unknown")).toHaveAttribute("data-visual-state", "selected");
    expect(card("common")).toHaveAttribute("data-visual-state", "default");

    activate("common", "Same"); // [A] + B -> [A, B]
    activate("unknown", "Same"); // [A, B] + A -> [B]
    expect(card("unknown")).toHaveAttribute("data-visual-state", "default");
    expect(card("common")).toHaveAttribute("data-visual-state", "selected");

    activate("third", "Third"); // [B] + C -> [B, C]
    expect(card("common")).toHaveAttribute("data-visual-state", "selected");
    expect(card("third")).toHaveAttribute("data-visual-state", "selected");

    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));
    expect(pickCardStep).toHaveBeenCalledWith(["common", "third"], "deck");
  });

  it("consumes_a_native_workspace_drag_double_click_before_commander_pick_two_pack_selection", () => {
    const pickCardStep = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const onCompatibilityDoubleClick = vi.fn();
    const rendered = render(<WorkspaceDragCommanderPickTwoHarness pickCardStep={pickCardStep} onCompatibilityDoubleClick={onCompatibilityDoubleClick} />);
    const workspaceSource = screen.getByTestId("workspace-drag-source");
    const workspaceTarget = screen.getByTestId("workspace-drag-target");
    workspaceSource.setPointerCapture = vi.fn();
    workspaceSource.releasePointerCapture = vi.fn();
    workspaceTarget.getBoundingClientRect = () => ({ left: 0, top: 0, right: 200, bottom: 200, width: 200, height: 200, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;

    fireEvent.pointerDown(workspaceSource, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 70, pointerType: "mouse" });
    fireEvent.pointerMove(workspaceSource, { clientX: 30, clientY: 30, pointerId: 70, pointerType: "mouse" });
    fireEvent.pointerUp(workspaceSource, { clientX: 30, clientY: 30, pointerId: 70, pointerType: "mouse" });

    firePointerActivation(workspaceSource, "click", { detail: 1, pointerId: 70, pointerType: "mouse" });
    fireEvent(workspaceSource, new MouseEvent("dblclick", { bubbles: true, detail: 2 }));

    expect(onCompatibilityDoubleClick).not.toHaveBeenCalled();
    expect(pickCardStep).not.toHaveBeenCalled();

    const card = (instanceId: string) => rendered.container.querySelector<HTMLElement>(`[data-instance-id="${instanceId}"]`)!;
    fireEvent.click(within(card("unknown")).getByRole("button", { name: "Same" }));
    fireEvent.click(within(card("common")).getByRole("button", { name: "Same" }));

    expect(card("unknown")).toHaveAttribute("data-visual-state", "selected");
    expect(card("common")).toHaveAttribute("data-visual-state", "selected");
    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));
    expect(pickCardStep).toHaveBeenCalledWith(["unknown", "common"], "deck");
  });

  it("replaces_the_oldest_completed_commander_pick_two_selection_on_desktop_and_submits_the_new_pair_manually", () => {
    const pickCardStep = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const rendered = render(<CommanderPickTwoPack pickCardStep={pickCardStep} />);
    const card = (instanceId: string) => rendered.container.querySelector<HTMLElement>(`[data-instance-id="${instanceId}"]`)!;
    const activate = (instanceId: string, name: string) => fireEvent.click(within(card(instanceId)).getByRole("button", { name }));

    activate("unknown", "Same"); // [] + A -> [A]
    activate("common", "Same"); // [A] + B -> [A, B]
    expect(card("unknown")).toHaveAttribute("data-visual-state", "selected");
    expect(card("common")).toHaveAttribute("data-visual-state", "selected");

    activate("third", "Third"); // [A, B] + C -> [B, C]
    expect(card("unknown")).toHaveAttribute("data-visual-state", "default");
    expect(card("common")).toHaveAttribute("data-visual-state", "selected");
    expect(card("third")).toHaveAttribute("data-visual-state", "selected");

    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));
    expect(pickCardStep).toHaveBeenCalledWith(["common", "third"], "deck");
  });

  it.each([
    { responsiveLayout: "phone-portrait" as const },
    { responsiveLayout: "tablet-portrait" as const },
  ])("selects_commander_pick_two_cards_from_a_$responsiveLayout_touch_tap", ({ responsiveLayout }) => {
    const localDrag = { ...dragController, handlePointerDown: vi.fn() };
    const rendered = render(<CommanderPickTwoPack responsiveLayout={responsiveLayout} dragController={localDrag} />);
    const first = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    fireEvent.pointerDown(first, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 81, pointerType: "touch" });
    fireEvent.pointerUp(first, { clientX: 20, clientY: 20, isPrimary: true, pointerId: 81, pointerType: "touch" });

    expect(first).toHaveAttribute("data-visual-state", "selected");
  });

  it.each(["phone-portrait" as const, "tablet-portrait" as const])("replaces_the_oldest_completed_commander_pick_two_selection_from_a_%s_touch_tap", (responsiveLayout) => {
    const pickCardStep = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const localDrag = { ...dragController, handlePointerDown: vi.fn() };
    const rendered = render(<CommanderPickTwoPack responsiveLayout={responsiveLayout} dragController={localDrag} pickCardStep={pickCardStep} />);
    const card = (instanceId: string) => rendered.container.querySelector<HTMLElement>(`[data-instance-id="${instanceId}"]`)!;
    const tap = (instanceId: string, pointerId: number) => {
      const element = card(instanceId);
      fireEvent.pointerDown(element, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId, pointerType: "touch" });
      fireEvent.pointerUp(element, { clientX: 20, clientY: 20, isPrimary: true, pointerId, pointerType: "touch" });
    };

    tap("unknown", 81); // [] + A -> [A]
    tap("common", 82); // [A] + B -> [A, B]
    expect(card("unknown")).toHaveAttribute("data-visual-state", "selected");
    expect(card("common")).toHaveAttribute("data-visual-state", "selected");

    tap("third", 83); // [A, B] + C -> [B, C]
    expect(card("unknown")).toHaveAttribute("data-visual-state", "default");
    expect(card("common")).toHaveAttribute("data-visual-state", "selected");
    expect(card("third")).toHaveAttribute("data-visual-state", "selected");

    fireEvent.click(screen.getByRole("button", { name: responsiveLayout === "phone-portrait" ? "Deck" : "Confirm Pick" }));
    expect(pickCardStep).toHaveBeenCalledWith(["common", "third"], "deck");
  });

  it("disables_commander_pick_two_double_click_and_double_tap_confirmation", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_000));
    const pickCard = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const pickCardStep = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const rendered = render(<CommanderPickTwoPack pickCard={pickCard} pickCardStep={pickCardStep} confirmPick={confirmPick} />);
    const first = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const second = rendered.container.querySelector<HTMLElement>('[data-instance-id="common"]')!;
    const activation = within(first).getByRole("button", { name: "Same" });

    fireEvent.click(activation);
    fireEvent.click(within(second).getByRole("button", { name: "Same" }));
    expect(first).toHaveAttribute("data-visual-state", "selected");
    expect(second).toHaveAttribute("data-visual-state", "selected");

    fireEvent.doubleClick(activation);
    fireEvent.pointerDown(first, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 82, pointerType: "touch" });
    fireEvent.pointerUp(first, { clientX: 20, clientY: 20, isPrimary: true, pointerId: 82, pointerType: "touch" });
    vi.advanceTimersByTime(150);
    fireEvent.pointerDown(first, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 83, pointerType: "touch" });
    fireEvent.pointerUp(first, { clientX: 20, clientY: 20, isPrimary: true, pointerId: 83, pointerType: "touch" });

    expect(pickCardStep).not.toHaveBeenCalled();
    expect(pickCard).not.toHaveBeenCalled();
    expect(confirmPick).not.toHaveBeenCalled();
  });

  it("keeps_the_engine_ordered_selection_policy_on_a_one_card_commander_remainder", () => {
    const pickCard = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const confirmPick = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const rendered = render(<CommanderPickTwoPack requiredPickCount={1} pickCard={pickCard} confirmPick={confirmPick} />);
    const activation = within(rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!).getByRole("button", { name: "Same" });

    fireEvent.doubleClick(activation);

    expect(pickCard).not.toHaveBeenCalled();
    expect(confirmPick).not.toHaveBeenCalled();
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
    expect(selectCard).not.toHaveBeenCalled();
    expect(localDrag.handlePointerDown).toHaveBeenCalled();
    expect(confirmPick).not.toHaveBeenCalled();

    fireEvent.pointerDown(cardElement, { button: 0, isPrimary: true, pointerId: 11, pointerType: "mouse" });
    fireEvent.pointerUp(cardElement, { pointerId: 11, pointerType: "mouse" });
    fireEvent.click(button);
    fireEvent.doubleClick(button);
    expect(selectCard).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(confirmPick).toHaveBeenCalledWith("deck"));
    expect(selectCard).toHaveBeenCalledWith("unknown");
  });

  it("ignores_exact_shell_desktop_clicks_while_locked_before_consuming_compatibility_activation", () => {
    const selectCard = vi.fn();
    const localDrag = {
      ...dragController,
      consumeCompatibilityActivation: vi.fn(() => false),
    };
    const initial = controller({
      interactionLocked: true,
      selectCard,
      dragController: localDrag,
    });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const cardElement = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    firePointerActivation(cardElement, "click", { detail: 1, pointerId: 12, pointerType: "mouse" });

    expect(localDrag.consumeCompatibilityActivation).not.toHaveBeenCalled();
    expect(selectCard).not.toHaveBeenCalled();

    rendered.rerender(<PackDisplay controller={{ ...initial, interactionLocked: false }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    firePointerActivation(cardElement, "click", { detail: 1, pointerId: 12, pointerType: "mouse" });

    expect(localDrag.consumeCompatibilityActivation).toHaveBeenCalledTimes(1);
    expect(selectCard).toHaveBeenCalledTimes(1);
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
    expect(firstCard).toHaveClass("select-none", "caret-transparent", "transition-all", "duration-150", "cursor-pointer", "hover:scale-[1.05]", "hover:ring-white/20");

    fireEvent.click(cardButton, { detail: 1, pointerType: "mouse" });
    expect(selectCard).not.toHaveBeenCalled();
    fireEvent.click(cardButton, { detail: 0 });
    expect(selectCard).toHaveBeenCalledWith("unknown");

    rendered.rerender(<PackDisplay controller={{ ...initial, selectedCard: "unknown" }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    expect(firstCard).toHaveClass(
      "transition-transform",
      "duration-150",
      "ring-2",
      "ring-arcane",
      "shadow-[0_0_7px_3px_#38bdf8]",
    );
    expect(firstCard).not.toHaveClass("motion-safe:animate-[draft-pack-selected-glow_4.8s_ease-in-out_infinite]");
    expect(firstCard).not.toHaveClass("transition-all");
    expect(firstCard).not.toHaveClass("!duration-0");
    expect(firstCard).not.toHaveClass("scale-105");
    expect(within(firstCard).queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();
    const confirmButton = screen.getByRole("button", { name: "Confirm Pick" });
    for (const className of menuButtonClass({ tone: "emerald", size: "sm" }).split(" ")) {
      expect(confirmButton).toHaveClass(className);
    }
    expect(confirmButton).toHaveClass("!min-h-9", "select-none", "!py-0", "caret-transparent");
    expect(confirmButton.parentElement).toHaveAttribute("data-pack-status-controls");
    expect(confirmButton.closest("[data-pack-toolbar]")).toHaveClass("flex-nowrap", "overflow-x-auto", "min-h-9");
    fireEvent.click(confirmButton);
    expect(confirmPick).toHaveBeenCalledWith("deck");
    expect(within(firstCard).getByRole("button", { name: "Same" })).toHaveTextContent("Same");
  });

  it("blocks_incomplete_duplicate_and_stale_effect_sources_but_dispatches_a_complete_live_pair", async () => {
    const effectView = { ...view, draft_effects: [effectCard] };
    const pickCard = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const pickEffect = vi.fn().mockResolvedValue({ status: "ignored", reason: "busy" });
    const localDrag = {
      ...dragController,
      handlePointerDown: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
    };
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
    expect(localDrag.consumeCompatibilityActivation).toHaveBeenLastCalledWith(expect.objectContaining({
      kind: "click",
      pointerId: null,
      surface: "pack",
      sourceInstanceId: "common",
    }));
    fireEvent.click(within(first).getByRole("button", { name: "Pick Same to Sideboard" }));
    fireEvent.doubleClick(within(second).getByRole("button", { name: "Same" }));
    fireEvent.pointerDown(first, { button: 0, isPrimary: true, pointerId: 2, pointerType: "mouse" });
    await vi.waitFor(() => expect(pickEffect).toHaveBeenCalledTimes(2));
    expect(pickEffect).toHaveBeenNthCalledWith(1, "effect", ["unknown", "common"], "sideboard");
    expect(pickEffect).toHaveBeenNthCalledWith(2, "effect", ["unknown", "common"], "deck");
    expect(localDrag.handlePointerDown).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({
      kind: "draft-effect",
      authorityId: "effect",
      sourceInstanceId: "unknown",
      instanceIds: ["unknown", "common"],
    }));
    expect(pickCard).not.toHaveBeenCalled();
  });

  it("renders_all_six_visual_states_with_drag_admission_waiting_and_token_precedence", () => {
    vi.useFakeTimers();
    imageState.src = "https://images.example.test/same.jpg";
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
    const departure = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"][data-visual-state="leaving"]')!;
    expect(departure).toBeInTheDocument();
    expect(within(departure).getByRole("img", { name: "Same" })).toHaveAttribute("src", imageState.src);
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

  it("does_not_retain_a_direct_pick_after_the_engine_advances_to_the_next_pick", async () => {
    vi.useFakeTimers();
    let resolvePick!: (outcome: { status: "acknowledged" }) => void;
    const pickCard = vi.fn().mockReturnValue(new Promise((resolve) => { resolvePick = resolve; }));
    const initial = controller({ pickCard });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const picked = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    fireEvent.click(within(picked).getByRole("button", { name: "Pick Same to Deck" }));
    rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...view, pick_number: 1, current_pack: [cards[1]] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    await act(async () => resolvePick({ status: "acknowledged" }));

    expect(rendered.container.querySelector('[data-instance-id="unknown"]')).not.toBeInTheDocument();
    expect(rendered.container.querySelector('[data-visual-state="leaving"]')).not.toBeInTheDocument();
    expect(rendered.container.querySelector('[data-instance-id="common"]')).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps_same_step_departures_but_clears_them_and_their_timers_on_a_new_pack", async () => {
    vi.useFakeTimers();
    let resolvePick!: (outcome: { status: "acknowledged" }) => void;
    const pickCard = vi.fn().mockReturnValue(new Promise((resolve) => { resolvePick = resolve; }));
    const initial = controller({ pickCard });
    const rendered = render(<PackDisplay controller={initial} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    const picked = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;

    fireEvent.click(within(picked).getByRole("button", { name: "Pick Same to Deck" }));
    rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...view, current_pack: [cards[1]] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);
    await act(async () => resolvePick({ status: "acknowledged" }));
    expect(rendered.container.querySelector('[data-instance-id="unknown"][data-visual-state="leaving"]')).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(1);

    rendered.rerender(<PackDisplay controller={{ ...initial, view: { ...view, current_pack_number: 1, pick_number: 0, current_pack: [] } }} presentation={{ packScale: 1, setPackScale: vi.fn() }} onCardHover={vi.fn()} />);

    expect(screen.getByText("Waiting for next pack...")).toBeInTheDocument();
    expect(rendered.container.querySelector('[data-instance-id="unknown"]')).not.toBeInTheDocument();
    expect(rendered.container.querySelector('[data-visual-state="leaving"]')).not.toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does_not_retain_a_real_drag_pick_after_the_engine_advances_to_an_empty_pack", async () => {
    vi.useFakeTimers();
    let resolveOutcome!: (outcome: { status: "acknowledged" }) => void;
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: request.interactionGeneration,
      outcome: new Promise((resolve) => { resolveOutcome = resolve; }),
    }));
    const rendered = render(<RealDragPackHarness onDrop={onDrop} advancedView={{ ...view, current_pack_number: 1, pick_number: 0, current_pack: [] }} />);
    const source = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const target = screen.getByTestId("real-drag-target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => ({ left: 0, top: 0, right: 200, bottom: 200, width: 200, height: 200, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;

    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 96, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 96, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 96, pointerType: "mouse" });
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(source).toHaveAttribute("data-visual-state", "waiting");

    fireEvent.click(screen.getByRole("button", { name: "advance pack" }));
    expect(screen.getByText("Waiting for next pack...")).toBeInTheDocument();
    await act(async () => resolveOutcome({ status: "acknowledged" }));
    fireEvent.click(screen.getByRole("button", { name: "unlock" }));

    expect(rendered.container.querySelector('[data-instance-id="unknown"]')).not.toBeInTheDocument();
    expect(rendered.container.querySelector('[data-visual-state="leaving"]')).not.toBeInTheDocument();
    expect(screen.getByText("Waiting for next pack...")).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("suppresses_the_trailing_desktop_shell_click_after_a_collapsed_sideboard_drop", async () => {
    let resolveOutcome!: (outcome: Awaited<DraftDropDispatch["outcome"]>) => void;
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: request.interactionGeneration,
      outcome: new Promise((resolve) => { resolveOutcome = resolve; }),
    }));
    const onSelectionChange = vi.fn();
    const rendered = render(
      <RealDragPackHarness
        onDrop={onDrop}
        onSelectionChange={onSelectionChange}
      />,
    );
    const source = rendered.container.querySelector<HTMLElement>('[data-instance-id="unknown"]')!;
    const activation = within(source).getByRole("button", { name: "Same" });
    const target = screen.getByTestId("real-drag-target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => ({ left: 100, top: 0, right: 300, bottom: 200, width: 200, height: 200, x: 100, y: 0, toJSON: () => ({}) }) as DOMRect;

    fireEvent.pointerDown(activation, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 100, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 120, clientY: 20, pointerId: 100, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 120, clientY: 20, isPrimary: true, pointerId: 100, pointerType: "mouse" });
    firePointerActivation(source, "click", { detail: 1, pointerId: 100, pointerType: "mouse" });

    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(onDrop).toHaveBeenCalledWith(expect.objectContaining({
      destination: "sideboard",
      placementHint: { column: 0 },
    }));
    expect(onSelectionChange).not.toHaveBeenCalled();
    expect(source).toHaveAttribute("data-visual-state", "waiting");
    expect(screen.queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();

    await act(async () => resolveOutcome({ status: "acknowledged" }));
    fireEvent.click(screen.getByRole("button", { name: "unlock" }));
    expect(source).toHaveAttribute("data-visual-state", "default");
    expect(screen.queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();
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
    expect(first).toHaveAttribute("data-visual-state", "default");
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
