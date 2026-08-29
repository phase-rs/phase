import { useState, type ReactNode } from "react";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  DraftCardInstance,
  DraftPoolGroupKind,
  DraftPoolGroups,
  DraftPlayerView,
} from "../../../adapter/draft-adapter";
import { useDraftStore } from "../../../stores/draftStore";
import type { CardHoverInfo } from "../../card/CardPreview";
import { PoolPanel } from "../PoolPanel";
import type { DraftWorkspaceFilter, DraftWorkspaceState } from "../workspace/types";
import type { DraftBoardSort } from "../workspace/workspacePreferences";

afterEach(() => {
  cleanup();
  useDraftStore.setState({ view: null, poolPanelOpen: true, poolSortMode: "color" });
});

function card(
  instanceId: string,
  name: string,
  setCode: string,
  collectorNumber: string,
  cmc: number,
): DraftCardInstance {
  return {
    instance_id: instanceId,
    name,
    set_code: setCode,
    collector_number: collectorNumber,
    rarity: "common",
    colors: ["U"],
    cmc,
    type_line: "Creature",
  };
}

const pool = [
  card("shared-a", "Shared Name", "AAA", "1", 2),
  card("shared-b", "Shared Name", "BBB", "2", 2),
  card("other", "Other Card", "CCC", "3", 1),
];

function group(kind: DraftPoolGroupKind, instanceIds: string[]) {
  return {
    kind,
    total: instanceIds.length,
    cards: instanceIds.map((instanceId) => ({
      card: pool.find((entry) => entry.instance_id === instanceId)!,
      count: 1,
      instance_ids: [instanceId],
    })),
  };
}

function groups(raritySupported = true): DraftPoolGroups {
  return {
    color_groups: [group("blue", ["shared-b", "shared-a", "other"])],
    type_groups: [group("creature", ["other", "shared-a", "shared-b"])],
    cmc_groups: [group("mana_value1", ["other"]), group("mana_value2", ["shared-a", "shared-b"])],
    rarity_groups: [group("common", ["shared-a", "shared-b", "other"])],
    type_filter_options: ["creature"],
    color_filter_options: ["blue"],
    color_counts: { white: 0, blue: 3, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: raritySupported ? ["common"] : null },
    workspace_row_classification: {
      creature_instance_ids: pool.map((entry) => entry.instance_id),
      noncreature_instance_ids: [],
    },
  };
}

const workspace: DraftWorkspaceState = {
  schemaVersion: 1,
  placements: {
    "shared-a": { zone: "deck", row: 0, column: 0, order: 0 },
    "shared-b": { zone: "sideboard", row: 0, column: 0, order: 0 },
    other: { zone: "deck", row: 0, column: 0, order: 1 },
  },
  virtualBasics: [],
};

const preferences = {
  deck: { sort: "cmc", columnCount: 3, rows: "one", showHeaders: true },
  sideboard: { sort: "color", columnCount: 2, rows: "one", showHeaders: true },
} as const;

function ControlledHarness({
  onCardHover,
  raritySupported = true,
  initialSort = "cmc",
  compactPrimaryControls,
  compactCount,
  compactTrailingControls,
  builderCompact = false,
}: {
  onCardHover?: (info: CardHoverInfo | null) => void;
  raritySupported?: boolean;
  initialSort?: DraftBoardSort;
  compactPrimaryControls?: ReactNode;
  compactCount?: ReactNode;
  compactTrailingControls?: ReactNode;
  builderCompact?: boolean;
}) {
  const [state, setState] = useState(workspace);
  const [filter, setFilter] = useState<DraftWorkspaceFilter>("combined");
  const [sort, setSort] = useState<DraftBoardSort>(initialSort);
  return (
    <PoolPanel
      onCardHover={onCardHover}
      controlledWorkspace={{
        pool,
        poolGroups: groups(raritySupported),
        workspace: state,
        preferences,
        filter,
        sort,
        onFilterChange: setFilter,
        onSortChange: setSort,
        onWorkspaceChange: setState,
        compactPrimaryControls,
        compactCount,
        compactTrailingControls,
        builderCompact,
      }}
    />
  );
}

describe("controlled workspace pool panel", () => {
  it("controlled_pool_panel_previews_exact_printing_and_clears_on_leave", () => {
    const onCardHover = vi.fn();
    const { container } = render(<ControlledHarness onCardHover={onCardHover} />);
    const first = container.querySelector<HTMLElement>('[data-instance-id="shared-a"]')!;
    const second = container.querySelector<HTMLElement>('[data-instance-id="shared-b"]')!;

    fireEvent.mouseEnter(within(first).getByRole("button", { name: "Shared Name" }));
    expect(onCardHover).toHaveBeenLastCalledWith({
      name: "Shared Name",
      sourcePrinting: { setCode: "AAA", collectorNumber: "1" },
    });
    fireEvent.mouseLeave(within(first).getByRole("button", { name: "Shared Name" }));
    expect(onCardHover).toHaveBeenLastCalledWith(null);
    fireEvent.focus(within(second).getByRole("button", { name: "Shared Name" }));
    expect(onCardHover).toHaveBeenLastCalledWith({
      name: "Shared Name",
      sourcePrinting: { setCode: "BBB", collectorNumber: "2" },
    });
    fireEvent.blur(within(second).getByRole("button", { name: "Shared Name" }));
    expect(onCardHover).toHaveBeenLastCalledWith(null);
  });

  it("filters_counts_sorts_and_moves_controlled_instances_by_exact_identity", () => {
    const { container } = render(<ControlledHarness />);
    const renderedIds = () => [...container.querySelectorAll("[data-instance-id]")]
      .map((element) => element.getAttribute("data-instance-id"));

    expect(renderedIds()).toEqual(["other", "shared-a", "shared-b"]);
    fireEvent.click(screen.getByRole("button", { name: "Color" }));
    expect(renderedIds()).toEqual(["shared-b", "shared-a", "other"]);

    const first = container.querySelector<HTMLElement>('[data-instance-id="shared-a"]')!;
    fireEvent.click(within(first).getByRole("button", { name: "Shared Name" }));
    expect(screen.getByRole("button", { name: "Deck (1)" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sideboard (2)" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Sideboard (2)" }));
    expect(renderedIds()).toEqual(["shared-b", "shared-a"]);
  });

  it("omits_rarity_for_unsupported_pools_and_falls_back_to_cmc", () => {
    render(<ControlledHarness raritySupported={false} initialSort="rarity" />);

    expect(screen.queryByRole("button", { name: "Rarity" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Mana value" })).toHaveAttribute("aria-pressed", "true");
  });

  it("keeps_mana_value_compact_controls_together_before_wrappable_sorts", () => {
    const { container } = render(
      <ControlledHarness
        compactPrimaryControls={<button type="button">Lands</button>}
        compactTrailingControls={<button type="button">Visual builder</button>}
      />,
    );

    const primary = container.querySelector<HTMLElement>("[data-compact-pool-primary-controls]")!;
    expect(primary).toHaveClass("flex-wrap");
    expect(within(primary).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["Mana value", "Color", "Rarity", "Type", "Lands", "Visual builder"]);
    expect(within(primary).getByRole("button", { name: "Color" })).toHaveClass("min-h-8");
  });

  it("uses_a_capability_aware_group_menu_only_for_builder_phone_compact", () => {
    const { container } = render(
      <ControlledHarness
        raritySupported={false}
        builderCompact
        compactPrimaryControls={<button type="button">Add Lands</button>}
        compactCount={<button type="button">Counts</button>}
        compactTrailingControls={<button type="button">Visual builder</button>}
      />,
    );

    const primary = container.querySelector<HTMLElement>("[data-compact-pool-primary-controls]")!;
    expect(within(primary).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["Group", "Add Lands", "Counts", "Visual builder"]);
    expect(primary).toHaveClass("flex-nowrap", "w-full");
    expect(container.querySelector("[data-compact-pool-trailing-controls]")).toHaveClass("ml-auto");
    expect(within(primary).getByRole("button", { name: "Group" })).toHaveClass("min-h-11", "bg-slate-950/80");
    expect(screen.getByRole("region", { name: "Card pool" })).toHaveClass("min-h-0", "overflow-hidden");
    expect(container.querySelector<HTMLElement>("[data-instance-id]")?.parentElement?.parentElement)
      .toHaveClass("min-h-0", "overflow-y-auto");
    expect(screen.queryByRole("button", { name: "Mana value" })).not.toBeInTheDocument();

    fireEvent.click(within(primary).getByRole("button", { name: "Group" }));
    const menu = screen.getByRole("menu", { name: "Group" });
    for (const sort of ["Mana value", "Color", "Type"]) {
      expect(within(menu).getByRole("menuitemradio", { name: sort })).toHaveClass("min-h-11");
    }
    expect(within(menu).getByRole("menuitemradio", { name: "Mana value" })).toHaveAttribute("aria-pressed", "true");
    expect(within(menu).queryByRole("menuitemradio", { name: "Rarity" })).not.toBeInTheDocument();
    fireEvent.click(within(menu).getByRole("menuitemradio", { name: "Color" }));
    expect(screen.queryByRole("menu", { name: "Group" })).not.toBeInTheDocument();
  });

  it("preserves_legacy_pool_panel_when_controlled_workspace_props_are_absent", () => {
    const onCardHover = vi.fn();
    useDraftStore.setState({
      view: { pool, pool_groups: groups() } as DraftPlayerView,
      poolPanelOpen: true,
      poolSortMode: "color",
    });
    render(<PoolPanel onCardHover={onCardHover} />);

    expect(screen.getByRole("button", { name: /3 cards drafted/i })).toBeInTheDocument();
    expect(screen.getByText("Blue (3)")).toBeInTheDocument();
    fireEvent.mouseEnter(screen.getAllByText("Shared Name")[0].closest("div")!);
    expect(onCardHover).toHaveBeenCalledWith({
      name: "Shared Name",
      sourcePrinting: { setCode: "BBB", collectorNumber: "2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Type" }));
    expect(screen.getByText("Creature (3)")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /3 cards drafted/i }));
    expect(screen.queryByText("Creature (3)")).not.toBeInTheDocument();
  });
});
