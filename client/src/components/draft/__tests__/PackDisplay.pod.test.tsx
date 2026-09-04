import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router";

import type { DraftPlayerView } from "../../../adapter/draft-adapter";
import { useMultiplayerDraftStore } from "../../../stores/multiplayerDraftStore";
import { DRAFT_WORKSPACE_PREFERENCES_KEY } from "../../../constants/storage";
import { DraftPodPage } from "../../../pages/DraftPodPage";
import type { PackDisplayController } from "../PackDisplay";
import { PackDisplay } from "../PackDisplay";
import { createDefaultDraftWorkspacePreferences } from "../workspace/workspacePreferences";

vi.mock("../../../hooks/useCardImage", () => ({ useCardImage: () => ({ src: null, isLoading: false }) }));
vi.mock("../../chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../menu/MenuShell", () => ({ MenuShell: ({ children }: { children: React.ReactNode }) => <>{children}</> }));
vi.mock("../DraftIntro", () => ({ DraftIntro: ({ onContinue }: { onContinue(): void }) => <button type="button" onClick={onContinue}>Continue</button> }));
vi.mock("../DraftPodLobby", () => ({ DraftPodLobby: () => null }));
vi.mock("../DraftProgress", () => ({ DraftProgress: () => null }));
vi.mock("../HostControls", () => {
  const emptyTopActions: readonly [] = [];
  return {
    HostControls: () => null,
    useHostDraftTopActions: (_options: { enabled: boolean }) => emptyTopActions,
  };
});
vi.mock("../HoverCardPreview", () => ({ HoverCardPreview: () => null }));
vi.mock("../PickTimer", () => ({ PickTimer: () => null }));
vi.mock("../PoolPanel", () => ({ PoolPanel: () => null }));
vi.mock("../SeatStatusRing", () => ({ SeatStatusRing: () => null }));

const view: DraftPlayerView = {
  status: "Drafting",
  kind: "Premier",
  launch_capability: "None",
  current_pack_number: 0,
  pick_number: 0,
  pass_direction: "Left",
  // Premier (CR 905.1a): one card per pick step.
  required_pick_count: 1,
  pick_selection_mode: "Direct",
  current_pack: [
    {
      instance_id: "card-1",
      name: "Lightning Bolt",
      set_code: "tst",
      collector_number: "1",
      rarity: "common",
      colors: ["R"],
      cmc: 1,
      type_line: "Instant",
    },
  ],
  pool: [],
  draft_effects: [],
  pool_groups: {
    color_groups: [],
    type_groups: [],
    cmc_groups: [],
    rarity_groups: [],
    type_filter_options: [],
    color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: ["common"] },
    workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
  },
  seats: [],
  cards_per_pack: 14,
  pack_sizes: [14, 14, 14],
  pack_set_codes: ["TST", "TST", "TST"],
  pack_pick_steps: [14, 14, 14],
  pick_steps_per_pack: 14,
  pack_count: 3,
  min_deck_size: 40,
  addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
  timer_remaining_ms: null,
  standings: [],
  current_round: 0,
  next_pairing_round: 1,
  tournament_format: "Swiss",
  pod_policy: "Competitive",
  pairings: [],
  match_config: { match_type: "Bo1" },
};
const card = view.current_pack![0];
const presentation = { packScale: 1, setPackScale: vi.fn() };

function podController(overrides: Partial<Extract<PackDisplayController, { kind: "pod-single-confirm" }>> = {}): Extract<PackDisplayController, { kind: "pod-single-confirm" }> {
  return {
    kind: "pod-single-confirm", view, selectedCard: null, interactionLocked: false,
    selectCard: vi.fn(), confirmPick: vi.fn(), pickCardWithDraftEffect: vi.fn(), autoPickCard: vi.fn(),
    ...overrides,
  };
}

function workspaceController(overrides: Partial<Extract<PackDisplayController, { kind: "local-workspace" }>> = {}): Extract<PackDisplayController, { kind: "local-workspace" }> {
  return {
    kind: "local-workspace", view, selectedCard: null, pendingIntent: null,
    interactionGeneration: 0, interactionLocked: false, doubleClickPick: false,
    dragController: {
      handlePointerDown: vi.fn(), handlePointerMove: vi.fn(), handlePointerUp: vi.fn(),
      handlePointerCancel: vi.fn(), handleLostPointerCapture: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
    },
    selectCard: vi.fn(), pickCard: vi.fn(), pickCardStep: vi.fn(), confirmPick: vi.fn(),
    pickCardWithDraftEffect: vi.fn(), autoPickCard: vi.fn(),
    ...overrides,
  };
}

describe("PackDisplay pod controller", () => {
  afterEach(() => {
    cleanup();
    useMultiplayerDraftStore.getState().reset();
    localStorage.clear();
    vi.restoreAllMocks();
  });

  it("is_destination_free_and_keeps_confirmation_off_the_card", () => {
    const selectCard = vi.fn();
    const confirmPick = vi.fn();
    const { rerender } = render(<PackDisplay controller={podController({ selectCard })} presentation={presentation} onCardHover={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Lightning Bolt" }));
    expect(selectCard).toHaveBeenCalledWith("card-1");
    expect(screen.queryByRole("button", { name: /Deck/ })).not.toBeInTheDocument();

    rerender(<PackDisplay controller={podController({ selectedCard: "card-1", confirmPick })} presentation={presentation} onCardHover={vi.fn()} />);
    const selected = screen.getByRole("button", { name: "Lightning Bolt" }).closest('[data-instance-id="card-1"]')!;
    expect(selected).toHaveClass(
      "transition-transform",
      "duration-150",
      "ring-2",
      "ring-arcane",
      "shadow-[0_0_7px_3px_#38bdf8]",
    );
    expect(selected).not.toHaveClass("motion-safe:animate-[draft-pack-selected-glow_4.8s_ease-in-out_infinite]");
    expect(selected).not.toHaveClass("!duration-0", "transition-all", "scale-105");
    expect(screen.queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Auto-pick" })).not.toBeInTheDocument();
    expect(confirmPick).not.toHaveBeenCalled();
  });

  it("tracks_a_complete_effect_pair_without_rendering_an_in_card_confirmation", () => {
    const effectCard = { ...card, instance_id: "effect", name: "Effect Card", draft_effect: "additional_pick" as const };
    const second = { ...card, instance_id: "card-2", name: "Island" };
    const effectView = { ...view, current_pack: [card, second], draft_effects: [effectCard] };
    const pickCardWithDraftEffect = vi.fn();
    const { rerender } = render(<PackDisplay controller={podController({ view: effectView })} presentation={presentation} onCardHover={vi.fn()} enableDraftEffects />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Effect Card" }));
    rerender(<PackDisplay controller={podController({ view: effectView, selectedCard: "card-1", pickCardWithDraftEffect })} presentation={presentation} onCardHover={vi.fn()} enableDraftEffects />);
    fireEvent.click(screen.getByRole("button", { name: "Island" }));
    expect(screen.queryByRole("button", { name: "Confirm Pick" })).not.toBeInTheDocument();
    expect(pickCardWithDraftEffect).not.toHaveBeenCalled();
  });

  it("submits_the_engine_defined_two_card_commander_pick_step", () => {
    const second = { ...card, instance_id: "card-2", name: "Island" };
    const commanderView = { ...view, kind: "CommanderDraft" as const, required_pick_count: 2, pick_selection_mode: "Ordered" as const, current_pack: [card, second] };
    const pickCardStep = vi.fn(async () => ({ status: "acknowledged" as const }));
    render(
      <PackDisplay controller={workspaceController({ view: commanderView, selectedCard: "card-1", pickCardStep })} presentation={presentation} onCardHover={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Island" }));
    expect(screen.getByRole("button", { name: "Confirm Pick" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));
    expect(pickCardStep).toHaveBeenCalledWith(["card-1", "card-2"], "deck");
  });
});

describe("DraftPodPage pack presentation preferences", () => {
  afterEach(() => {
    cleanup();
    useMultiplayerDraftStore.getState().reset();
    localStorage.clear();
    vi.restoreAllMocks();
  });

  function renderDraftingPage() {
    useMultiplayerDraftStore.setState({ phase: "drafting", view, selectedCard: null, paused: false, pauseReason: null });
    const rendered = render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    return rendered;
  }

  it("loads_repairs_persists_and_retains_pack_scale_across_remount", () => {
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, JSON.stringify({
      ...createDefaultDraftWorkspacePreferences(),
      packScale: 0.83,
    }));
    const first = renderDraftingPage();
    expect(screen.getByRole("slider", { name: "Pack scale" })).toHaveValue("0.83");

    fireEvent.change(screen.getByRole("slider", { name: "Pack scale" }), { target: { value: "1.2" } });
    expect(JSON.parse(localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY) ?? "null")).toMatchObject({ schemaVersion: 3, packScale: 1.2 });
    first.unmount();

    renderDraftingPage();
    expect(screen.getByRole("slider", { name: "Pack scale" })).toHaveValue("1.2");
  });

  it("keeps_pack_scale_interactive_when_storage_throws", () => {
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, JSON.stringify({
      ...createDefaultDraftWorkspacePreferences(),
      packScale: 0.75,
    }));
    const setItem = vi.spyOn(localStorage, "setItem").mockImplementation(() => { throw new Error("blocked"); });
    renderDraftingPage();

    fireEvent.change(screen.getByRole("slider", { name: "Pack scale" }), { target: { value: "1.1" } });
    expect(screen.getByRole("slider", { name: "Pack scale" })).toHaveValue("1.1");
    expect(setItem).toHaveBeenCalled();
  });
});
