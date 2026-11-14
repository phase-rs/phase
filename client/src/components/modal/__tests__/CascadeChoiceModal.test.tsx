import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  CastOfferKind,
  GameObject,
  ManaCost,
  ResolutionCastCleanup,
  WaitingFor,
} from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CascadeChoiceModal } from "../CascadeChoiceModal.tsx";

const dispatchMock = vi.fn();

type GraveyardPaidCastOffer = Extract<CastOfferKind, { type: "GraveyardPaidCast" }>;

const paidCastCleanup = {
  source_id: 17,
  face_policy: {
    filter: { type: "Any" },
    source_id: 17,
    controller: 0,
    constraint: null,
  },
  exiled_misses: [],
  reject_action: { type: "RemainExiled" },
  success_action: { type: "BottomMisses" },
} satisfies ResolutionCastCleanup;

function graveyardPaidCast(
  fields: Omit<GraveyardPaidCastOffer, "type" | "hit_card" | "cleanup"> = {},
): GraveyardPaidCastOffer {
  return { type: "GraveyardPaidCast", hit_card: 52, cleanup: paidCastCleanup, ...fields };
}

function makeObject(id: number, name: string): GameObject {
  return buildGameObjectWithCoreTypes(["Instant"], {
    id,
    card_id: id,
    zone: "Exile",
    name,
    mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
    color: ["Red"],
    base_color: ["Red"],
    timestamp: 1,
    entered_battlefield_turn: null,
  });
}

function setWaitingFor(waitingFor: WaitingFor) {
  const gameState = buildGameState({
    objects: buildObjectMap(makeObject(52, "Lightning Bolt")),
    priority_player: 0,
    waiting_for: waitingFor,
  });

  useGameStore.setState({
    gameState,
    waitingFor,
    dispatch: dispatchMock,
  });
}

describe("CascadeChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  it("renders DiscoverChoice and dispatches DiscoverChoice actions", () => {
    setWaitingFor({
      type: "CastOffer",
      data: {
        player: 0,
        kind: { type: "Discover", hit_card: 52, exiled_misses: [1, 2, 3], discover_value: 3 },
      },
    });

    render(<CascadeChoiceModal />);

    expect(screen.getByText("Discover")).toBeInTheDocument();
    expect(screen.getByText("Cast Lightning Bolt?")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Cast Lightning Bolt/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DiscoverChoice",
      data: { choice: { type: "Cast" } },
    });

    fireEvent.click(screen.getByRole("button", { name: /Put into hand/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DiscoverChoice",
      data: { choice: { type: "Decline" } },
    });
  });

  it("keeps CascadeChoice routing intact", () => {
    setWaitingFor({
      type: "CastOffer",
      data: {
        player: 0,
        kind: { type: "Cascade", hit_card: 52, exiled_misses: [1], source_mv: 3 },
      },
    });

    render(<CascadeChoiceModal />);

    expect(screen.getByText("Cascade")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Decline/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "CascadeChoice",
      data: { choice: { type: "Decline" } },
    });
  });

  it("routes RippleChoice actions from remaining-hit offers", () => {
    setWaitingFor({
      type: "CastOffer",
      data: {
        player: 0,
        kind: { type: "Ripple", hit_card: 52, remaining_hits: [53], revealed_misses: [54] },
      },
    });

    render(<CascadeChoiceModal />);

    expect(screen.getByText("Ripple")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Cast Lightning Bolt/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "RippleChoice",
      data: { choice: { type: "Cast" } },
    });
  });

  // CR 608.2g + CR 609.4b: the paid graveyard cast (Quistis Trepe, Tinybones the
  // Pickpocket) must render PAID-cast copy (NOT the free "without paying" strings)
  // and dispatch GraveyardPaidCastChoice on accept/decline.
  it("renders PAID-cast copy and dispatches GraveyardPaidCastChoice actions", () => {
    setWaitingFor({
      type: "CastOffer",
      data: {
        player: 0,
        kind: graveyardPaidCast({
          mana_spend_permission: "AnyTypeOrColor",
        }),
      },
    });

    render(<CascadeChoiceModal />);

    // (a) PAID-cast copy: the graveyard eyebrow + the pay-its-cost suffix, and
    // NOT the free "without paying its mana cost" string.
    expect(screen.getByText("Cast from Graveyard")).toBeInTheDocument();
    expect(
      screen.getByText("(pay its mana cost — any type of mana may be spent)"),
    ).toBeInTheDocument();
    expect(screen.queryByText("(without paying its mana cost)")).not.toBeInTheDocument();

    // (b) accept → Cast, decline → Decline, both as GraveyardPaidCastChoice.
    fireEvent.click(screen.getByRole("button", { name: /Cast Lightning Bolt/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "GraveyardPaidCastChoice",
      data: { choice: { type: "Cast" } },
    });

    fireEvent.click(screen.getByRole("button", { name: /Decline/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "GraveyardPaidCastChoice",
      data: { choice: { type: "Decline" } },
    });
  });

  // CR 608.2g + CR 601.2b + CR 609.4b (issue #8775): the paid offer names
  // exactly what it carries — nothing for the plain offer (Helmut Zemo,
  // Toshiro Umezawa), the additional cost for Ogre Battlecaster, the
  // concession by its variant (`AnyColor` relaxes colors only), and both
  // together when both are present.
  it("names the plain paid cost, the additional cost, the concession variant, and both", () => {
    const renderOffer = (kind: Omit<GraveyardPaidCastOffer, "type" | "hit_card" | "cleanup">) => {
      setWaitingFor({
        type: "CastOffer",
        data: { player: 0, kind: graveyardPaidCast(kind) },
      });
      return render(<CascadeChoiceModal />);
    };
    const redRed = { type: "Cost", shards: ["Red", "Red"], generic: 0 } satisfies ManaCost;

    let view = renderOffer({});
    expect(screen.getByText("(pay its mana cost)")).toBeInTheDocument();
    expect(screen.queryByText(/any type of mana|any color of mana/)).not.toBeInTheDocument();
    view.unmount();

    view = renderOffer({ additional_cost: redRed });
    expect(screen.getByText("(pay its mana cost plus {R}{R})")).toBeInTheDocument();
    expect(screen.getByText(/by paying its mana cost plus \{R\}\{R\}\. Or decline/)).toBeInTheDocument();
    view.unmount();

    view = renderOffer({ mana_spend_permission: "AnyColor" });
    expect(screen.getByText("(pay its mana cost — any color of mana may be spent)")).toBeInTheDocument();
    expect(screen.queryByText(/any type of mana/)).not.toBeInTheDocument();
    view.unmount();

    renderOffer({ mana_spend_permission: "AnyTypeOrColor", additional_cost: redRed });
    expect(
      screen.getByText("(pay its mana cost plus {R}{R} — any type of mana may be spent)"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/by paying its mana cost plus \{R\}\{R\} — mana of any type may be spent\./),
    ).toBeInTheDocument();
  });
});
