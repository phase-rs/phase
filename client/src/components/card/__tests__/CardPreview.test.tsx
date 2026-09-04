import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject, ObjectCounterDisplay } from "../../../adapter/types.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardPreview } from "../CardPreview.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn((cardName: string, options?: { oracleId?: string }) => ({
    src: `${options?.oracleId ?? cardName}.png`,
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

function battlefieldObject(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObject({
    id: 101,
    card_id: 1,
    zone: "Battlefield",
    name: "Pithing Needle",
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    ...overrides,
  });
}

function gameStateWithObject(object: GameObject) {
  return buildGameState({
    objects: buildObjectMap(object),
    next_object_id: 102,
    battlefield: [object.id],
    next_timestamp: 2,
  });
}

afterEach(() => {
  cleanup();
  document.querySelectorAll("[data-hand-card]").forEach((node) => node.remove());
  vi.clearAllMocks();
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1280 });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 768 });
  useGameStore.setState({ gameState: null, spellCosts: {}, legalActionsByObject: {} });
  usePreferencesStore.setState({ animationSpeedMultiplier: 1, showCardPreviewFooter: true });
  useUiStore.getState().dismissPreview();
});

describe("CardPreview chosen attributes", () => {
  it("clamps an explicit preview position into the viewport", () => {
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 768 });
    const { container } = render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    const preview = container.querySelector<HTMLElement>("[data-card-preview]");
    expect(preview).not.toBeNull();
    expect(preview?.style.left).toBe("40px");
    expect(preview?.style.top).toBe("16px");
    expect(screen.getAllByAltText("Pithing Needle").length).toBeGreaterThan(0);
  });

  it("keeps the desktop preview mounted while its exit easing completes", async () => {
    const { container, rerender } = render(
      <CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />,
    );

    rerender(<CardPreview cardName={null} position={{ x: 20, y: 20 }} />);

    expect(container.querySelector("[data-card-preview]")).not.toBeNull();
    await waitFor(() => {
      expect(container.querySelector("[data-card-preview]")).toBeNull();
    });
  });

  it("anchors a hand preview to the viewport bottom and grows from its source card", () => {
    const object = battlefieldObject({ zone: "Hand" });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    const source = document.createElement("div");
    source.dataset.handCard = "";
    source.dataset.handRotation = "-4";
    source.dataset.objectId = "101";
    Object.defineProperty(source, "offsetWidth", { configurable: true, value: 120 });
    source.matches = vi.fn((selector) => selector === ":hover");
    source.getBoundingClientRect = () => ({
      bottom: 748,
      height: 168,
      left: 220,
      right: 340,
      top: 580,
      width: 120,
      x: 220,
      y: 580,
      toJSON: () => ({}),
    });
    document.body.appendChild(source);

    const { container } = render(
      <CardPreview cardName="Pithing Needle" objectId={101} handSourceObjectId={101} />,
    );

    const preview = container.querySelector<HTMLElement>("[data-card-preview]");
    expect(preview).not.toBeNull();
    expect(preview?.style.bottom).toBe("0px");
    expect(preview?.style.transformOrigin).toBe("50% 100%");
    expect(screen.getByAltText("Pithing Needle").parentElement!).toHaveClass(
      "w-[clamp(190px,18vw,300px)]",
    );
    source.remove();
  });

  it("uses the bottom-anchored hand animation for an active mobile scrub", () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 500 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 440 });
    const source = document.createElement("div");
    source.dataset.handCard = "";
    source.dataset.handTouchActive = "true";
    source.dataset.handRotation = "5";
    source.dataset.objectId = "101";
    Object.defineProperty(source, "offsetWidth", { configurable: true, value: 90 });
    source.matches = vi.fn(() => false);
    source.getBoundingClientRect = () => ({
      bottom: 432,
      height: 126,
      left: 180,
      right: 270,
      top: 306,
      width: 90,
      x: 180,
      y: 306,
      toJSON: () => ({}),
    });
    document.body.appendChild(source);

    const { container } = render(
      <CardPreview cardName="Pithing Needle" handSourceObjectId={101} />,
    );

    const preview = container.querySelector<HTMLElement>("[data-card-preview]");
    expect(preview).not.toBeNull();
    expect(preview?.style.bottom).toBe("0px");
    expect(preview).toHaveClass("pointer-events-none");
    expect(screen.getByAltText("Pithing Needle").parentElement!).toHaveClass(
      "w-[clamp(190px,18vw,300px)]",
    );
    source.remove();
  });

  it("hands off a stationary blue preview before despawning for direct card drag", async () => {
    const object = battlefieldObject({ zone: "Hand" });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    const source = document.createElement("div");
    source.dataset.handCard = "";
    source.dataset.handTouchActive = "true";
    source.dataset.objectId = String(object.id);
    Object.defineProperty(source, "offsetWidth", { configurable: true, value: 120 });
    source.matches = vi.fn(() => false);
    source.getBoundingClientRect = () => ({
      bottom: 748,
      height: 168,
      left: 220,
      right: 340,
      top: 580,
      width: 120,
      x: 220,
      y: 580,
      toJSON: () => ({}),
    });
    document.body.appendChild(source);
    useUiStore.setState({
      mobileHandGesture: {
        objectId: object.id,
        phase: "preview",
        sourceOrigin: {
          bottom: 748,
          centerX: 280,
          height: 168,
          rotation: 0,
          top: 580,
          width: 120,
        },
        offsetX: 12,
        offsetY: -30,
        playable: true,
        castReady: false,
      },
    });

    const { container } = render(
      <CardPreview
        cardName={object.name}
        objectId={object.id}
        handSourceObjectId={object.id}
      />,
    );

    expect(
      container.querySelector('[data-mobile-hand-preview-state="playable"]'),
    ).toHaveClass("ring-cyan-400");
    expect(
      container.querySelector('[data-mobile-hand-preview-wobble="true"]'),
    ).not.toBeNull();

    act(() => {
      useUiStore.getState().setMobileHandGesture({
        objectId: object.id,
        phase: "drag",
        sourceOrigin: {
          bottom: 748,
          centerX: 280,
          height: 168,
          rotation: 0,
          top: 580,
          width: 120,
        },
        offsetX: 16,
        offsetY: -90,
        playable: true,
        castReady: true,
      });
    });

    expect(container.querySelector("[data-card-preview]")).not.toBeNull();
    expect(
      container.querySelector("[data-mobile-hand-preview-wobble]"),
    ).toBeNull();
    await waitFor(() => {
      expect(container.querySelector("[data-card-preview]")).toBeNull();
    });
    source.remove();
  });

  it("uses the normal preview when the matching board hand card is not hovered", () => {
    const object = battlefieldObject({ zone: "Hand" });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    const source = document.createElement("div");
    source.dataset.handCard = "";
    source.dataset.objectId = "101";
    source.matches = vi.fn(() => false);
    document.body.appendChild(source);

    const { container } = render(
      <CardPreview
        cardName="Pithing Needle"
        objectId={object.id}
        handSourceObjectId={101}
      />,
    );

    const preview = container.querySelector<HTMLElement>("[data-card-preview]");
    expect(preview).not.toBeNull();
    expect(preview?.style.bottom).toBe("");
    expect(screen.getByAltText("Pithing Needle")).not.toHaveClass(
      "w-[clamp(190px,18vw,300px)]",
    );
    source.remove();
  });

  it("reuses one preview layer during rapid hand scrubbing", async () => {
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      writable: true,
      value: 500,
    });
    const first = battlefieldObject({
      id: 101,
      zone: "Hand",
      name: "First Card",
      printed_ref: { oracle_id: "oracle-first", face_name: "First Card" },
    });
    const second = battlefieldObject({
      id: 102,
      zone: "Hand",
      name: "Second Card",
      printed_ref: { oracle_id: "oracle-second", face_name: "Second Card" },
    });
    useGameStore.setState({
      gameState: buildGameState({
        objects: buildObjectMap(first, second),
        next_object_id: 103,
      }),
      spellCosts: {},
    });

    const firstSource = document.createElement("div");
    firstSource.dataset.handCard = "";
    firstSource.dataset.handTouchActive = "true";
    firstSource.dataset.objectId = String(first.id);
    firstSource.getBoundingClientRect = () => ({
      bottom: 748,
      height: 168,
      left: 220,
      right: 340,
      top: 580,
      width: 120,
      x: 220,
      y: 580,
      toJSON: () => ({}),
    });
    const secondSource = firstSource.cloneNode() as HTMLElement;
    secondSource.dataset.objectId = String(second.id);
    secondSource.getBoundingClientRect = () => ({
      ...firstSource.getBoundingClientRect(),
      left: 320,
      right: 440,
      x: 320,
    });
    document.body.append(firstSource, secondSource);

    useUiStore.setState({ inspectedObjectId: first.id });
    const { rerender } = render(
      <CardPreview
        cardName={first.name}
        objectId={first.id}
        handSourceObjectId={first.id}
      />,
    );

    firstSource.removeAttribute("data-hand-touch-active");
    secondSource.dataset.handTouchActive = "true";
    useUiStore.setState({ inspectedObjectId: second.id });
    rerender(
      <CardPreview
        cardName={second.name}
        objectId={second.id}
        handSourceObjectId={second.id}
      />,
    );

    expect(screen.getByAltText("Second Card")).toHaveAttribute(
      "src",
      "oracle-second.png",
    );
    expect(document.querySelectorAll("[data-card-preview]")).toHaveLength(1);
    await waitFor(() => {
      expect(screen.queryByAltText("First Card")).toBeNull();
    });
  });

  it("hides the informational footer without hiding the card art", () => {
    const object = battlefieldObject({
      chosen_attributes: [{ type: "CardName", value: "Lightning Bolt" }],
    });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    usePreferencesStore.setState({ showCardPreviewFooter: false });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    expect(screen.getByAltText("Pithing Needle")).toBeInTheDocument();
    expect(screen.queryByText("Chosen")).not.toBeInTheDocument();
    expect(screen.queryByText("Card name: Lightning Bolt")).not.toBeInTheDocument();
  });

  it("shows a persisted chosen card name for a battlefield permanent", () => {
    const object = battlefieldObject({
      chosen_attributes: [{ type: "CardName", value: "Lightning Bolt" }],
    });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    expect(screen.getByText("Chosen")).toBeInTheDocument();
    expect(screen.getByText("Card name: Lightning Bolt")).toBeInTheDocument();
  });

  it("renders keyword reminder tooltips for battlefield permanents", () => {
    const object = battlefieldObject({
      keywords: ["Flying", { Ward: { type: "Mana", data: { Cost: { shards: [], generic: 2 } } } }],
      base_keywords: ["Flying", { Ward: { type: "Mana", data: { Cost: { shards: [], generic: 2 } } } }],
    });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    expect(screen.getByText("Flying")).toBeInTheDocument();
    expect(screen.getByText("Ward").closest("[aria-describedby]")).not.toBeNull();
    expect(screen.getAllByAltText("2").length).toBeGreaterThan(0);
    expect(screen.getByText(/creatures with flying or reach/)).toBeInTheDocument();
    expect(screen.getByText(/ward cost/)).toBeInTheDocument();
  });

  it("renders mana symbols in battlefield preview ability text", () => {
    const object = battlefieldObject({
      abilities: [
        {
          description: "{G}, {T}: Add {G}.",
          effects: [],
          targets: [],
          cost: { type: "Tap" },
          timing: "AnyTime",
          kind: "Activated",
        },
      ],
    });
    useGameStore.setState({
      gameState: gameStateWithObject(object),
      legalActionsByObject: {
        [String(object.id)]: [
          {
            type: "ActivateAbility",
            data: { source_id: object.id, ability_index: 0 },
          },
        ],
      },
      spellCosts: {},
    });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Pithing Needle" position={{ x: 20, y: 20 }} />);

    expect(screen.getByText(/Activate/)).toBeInTheDocument();
    expect(screen.getAllByAltText("T").length).toBeGreaterThan(0);
    expect(screen.getAllByAltText("G").length).toBeGreaterThan(0);
  });

  it("passes token lookup metadata to the mobile preview image hook", () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 500 });
    const object = battlefieldObject({
      display_source: "Token",
      name: "Elf Warrior",
      power: 2,
      toughness: 2,
      color: ["Green"],
      card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elf", "Warrior"] },
      token_image_ref: {
        scryfall_id: "token-printing-id",
        scryfall_oracle_id: "token-oracle-id",
        face_name: "Elf Warrior",
        preset_id: "elf-warrior-token",
      },
    });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

    render(<CardPreview cardName="Elf Warrior" />);

    expect(useCardImage).toHaveBeenCalledWith("Elf Warrior", expect.objectContaining({
      isToken: true,
      tokenFilters: expect.objectContaining({
        colors: ["Green"],
        power: 2,
        subtypes: ["Elf", "Warrior"],
        toughness: 2,
      }),
      tokenImageRef: object.token_image_ref,
    }));
  });
});

// MAJOR-1 (CR 602.5): CardPreview is the SECOND `blocked_abilities` consumer and
// had no coverage before this change. It renders every prohibiting source name via
// preview.fromSource, joined, dropping ids absent from `objects`.
describe("CardPreview blocked abilities", () => {
  function inspectWith(object: GameObject, sources: GameObject[] = []) {
    const gameState = buildGameState({
      objects: buildObjectMap(object, ...sources),
      next_object_id: 999,
      battlefield: [object.id],
      next_timestamp: 2,
    });
    useGameStore.setState({ gameState, spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });
    return render(<CardPreview cardName={object.name} position={{ x: 20, y: 20 }} />);
  }

  it("renders both prohibiting source names when two sources block one ability", () => {
    const object = battlefieldObject({
      id: 101,
      name: "Grim Monolith",
      abilities: [
        {
          description: "{T}: draw",
          effects: [],
          targets: [],
          cost: { type: "Tap" },
          timing: "AnyTime",
          kind: "Activated",
        },
      ],
      blocked_abilities: [
        { ability_index: 0, sources: [201, 202], type: "CantBeActivated" },
      ],
    });
    inspectWith(object, [
      buildGameObject({ id: 201, name: "Needle A" }),
      buildGameObject({ id: 202, name: "Needle B" }),
    ]);

    expect(screen.getByText(/\(from Needle A, Needle B\)/)).toBeInTheDocument();
  });

  it("renders a single prohibiting source name", () => {
    const object = battlefieldObject({
      id: 101,
      name: "Grim Monolith",
      abilities: [],
      blocked_abilities: [
        { ability_index: 0, sources: [201], type: "CantBeActivated" },
      ],
    });
    inspectWith(object, [buildGameObject({ id: 201, name: "Needle A" })]);

    expect(screen.getByText(/\(from Needle A\)/)).toBeInTheDocument();
  });

  it("drops a departed source id and renders no fromSource span", () => {
    const object = battlefieldObject({
      id: 101,
      name: "Grim Monolith",
      abilities: [],
      // source 999 is absent from `objects` — the departed-source guard drops it.
      blocked_abilities: [
        { ability_index: 0, sources: [999], type: "Prohibited" },
      ],
    });
    inspectWith(object);

    expect(screen.queryByText(/\(from/)).not.toBeInTheDocument();
  });

  // CR 201.5: the blocked row labels the ability with the engine's own description, which
  // ships `~` as the self-reference token — the same leak the activate read-out above was
  // fixed for. Description is abridged from the reported Kilo board dump (object 110): the
  // engine text continues "Its controller may search their library for a basic land card, put
  // it onto the battlefield, then shuffle." — elided because that tail carries no `~` and so
  // moves neither assertion below.
  it("substitutes ~ with the source name in the blocked-ability read-out", () => {
    const object = battlefieldObject({
      id: 110,
      name: "Ghost Quarter",
      abilities: [
        {
          description: "{T}, Sacrifice ~: Destroy target land.",
          effects: [],
          targets: [],
          cost: { type: "Tap" },
          timing: "AnyTime",
          kind: "Activated",
        },
      ],
      blocked_abilities: [
        { ability_index: 0, sources: [201], type: "CantBeActivated" },
      ],
    });
    const { container } = inspectWith(object, [
      buildGameObject({ id: 201, name: "Needle A" }),
    ]);

    // Reach-guard: proves the row rendered at all, so the negative below is not vacuous.
    expect(container.textContent).toContain("{T}, Sacrifice Ghost Quarter: Destroy target land.");
    expect(container.textContent).not.toContain("~");
  });
});

// CR 732.2a / CR 701.34a: the hover status box under the full card render is the
// THIRD counter render site (after PermanentCard's pill and ArtCropCard's badge).
// An accepted counter-growth ∞ loop annotates the pumped row in
// `derived.counter_display` and deliberately leaves the object's real count
// finite (engine.rs `materialize_object_growth_shortcut`: "the object's real
// counter count is NOT mutated ... this only marks the pill to render ∞"), so a
// site that reads `obj.counters` alone shows a stale pre-shortcut number.
//
// Values taken from the real 4p playtest dump where the bug was observed
// (`.kilo-dump/game-state-turn-1-2026-07-22T20-04-12-617Z.json`): Pentad Prism is
// object 409 with `counters = {"charge": 2}`; accepting the Kilo/Freed/Relic
// proliferate loop marks (409, charge) unbounded — asserted end-to-end from that
// board by the engine test
// `kilo_accept_marks_pentad_charge_as_unbounded_display_target`.
//
// Matched pair: the ONLY difference between the two cases is the engine mark, so
// it is the discriminator.
describe("CardPreview unbounded counters", () => {
  // `null` means "a frame that arrived with no `derived.counter_display` at all" — the object's
  // own `counters` map stays populated either way, so a site that fell back to it would show a
  // pill in that case. It must not.
  function inspectPentadPrism(display: ObjectCounterDisplay | null) {
    const object = battlefieldObject({
      id: 409,
      name: "Pentad Prism",
      counters: { charge: 2 },
    });
    const gameState = gameStateWithObject(object);
    gameState.derived = display ? { counter_display: { 409: display } } : {};
    useGameStore.setState({ gameState, spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });
    return render(<CardPreview cardName="Pentad Prism" position={{ x: 20, y: 20 }} />);
  }

  it("renders ∞ for a counter the engine marks as unbounded", () => {
    const { container } = inspectPentadPrism({
      pills: [{ counter: "charge", count: 2, magnitude: "Unbounded" }],
    });

    expect(container.textContent).toContain("charge: ∞");
    expect(container.textContent).not.toContain("charge: 2");
  });

  it("renders the finite count when the counter is not marked unbounded", () => {
    // `magnitude` omitted exactly as the engine omits the serde default.
    const { container } = inspectPentadPrism({ pills: [{ counter: "charge", count: 2 }] });

    expect(container.textContent).toContain("charge: 2");
    expect(container.textContent).not.toContain("∞");
  });

  // THE NO-FALLBACK MATCHED PAIR. `counter_display` is the SINGLE authority: an object carrying
  // real counters with no projection entry renders NO row. This is what catches this render site
  // re-introducing `Object.entries(obj.counters)`, and it is worthless without its positive twin
  // — alone it would also pass on a panel that rendered nothing at all.
  it("renders no counter row for an object with counters but no projection entry", () => {
    const { container } = inspectPentadPrism(null);

    expect(container.textContent).not.toContain("charge");
    expect(container.textContent).not.toContain("∞");
  });

  it("renders the row for that SAME object once the projection carries it", () => {
    const { container } = inspectPentadPrism({ pills: [{ counter: "charge", count: 2 }] });

    expect(container.textContent).toContain("charge: 2");
  });

  // LOW: the ∞ row and its TOOLTIP must agree — a badge saying ∞ over a tooltip
  // interpolating the finite count contradicts itself (mirrors ArtCropCard.test.tsx:353).
  it("the ∞ status row's tooltip agrees with the badge", () => {
    const { container } = inspectPentadPrism({
      pills: [{ counter: "charge", count: 2, magnitude: "Unbounded" }],
    });

    // `GameplayTooltip` renders its lines through `createPortal(…, document.body)`, so the
    // summary is NOT inside `container` — query it via `screen`, exactly as the tooltip
    // assertion this mirrors does (ArtCropCard.test.tsx:353-368). `container` still carries
    // the badge, so both halves of the agreement are asserted against their real roots.
    expect(container.textContent).toContain("charge: ∞");
    expect(screen.getByText(/∞ charge counters/i)).toBeInTheDocument();
    expect(screen.queryByText(/2 charge counters/i)).not.toBeInTheDocument();
  });

  // REGRESSION (F2) — this test used to assert the GAP, and now asserts its fix. The ∞ counter
  // targets are derived by `analysis::resource::grown_beneficial_counter_deltas` over the two
  // frames `game::engine::drive_one_period_frames` produces, and its BEFORE frame is a clone of
  // the LIVE state. A pair that grows 0 → 1 across the driven period is therefore registered
  // while the live object carries none of that counter. While the channel published bare counter
  // TYPES, every display mode iterated `obj.counters` and such a mark rendered NOWHERE — a real,
  // accepted, registered ∞ that was invisible.
  //
  // The engine now publishes a self-sufficient ROW carrying the live count (`0` when absent), so
  // the display renders it without synthesizing anything: the frontend still must NOT invent a
  // counter row the engine did not publish, and it does not — the ENGINE decided this row exists.
  // Pinned end-to-end on the engine side by
  // `loop_counter_growth::plus_one_counter_growth_registers_a_target_the_bearer_does_not_yet_carry`.
  //
  // DISCRIMINATOR: `charge: 2` in the same frame is the paired positive — it proves the finite
  // path still renders finitely, so this is not a component that started drawing ∞ for
  // everything, and it makes the `oil: 0` negative non-vacuous.
  it("renders a marked counter the object does not carry, with count 0 (F2 regression)", () => {
    const { container } = inspectPentadPrism({
      pills: [
        { counter: "oil", count: 0, magnitude: "Unbounded" },
        { counter: "charge", count: 2 },
      ],
    });

    expect(container.textContent).toContain("oil: ∞");
    expect(container.textContent).toContain("charge: 2");
    // The ∞ must not be bought by showing a bogus finite count for a counter that is absent.
    expect(container.textContent).not.toContain("oil: 0");
  });
});

describe("CardPreview activate labels", () => {
  function inspect(object: GameObject, abilityIndexes: number[]) {
    useGameStore.setState({
      gameState: gameStateWithObject(object),
      legalActionsByObject: {
        [String(object.id)]: abilityIndexes.map((ability_index) => ({
          type: "ActivateAbility" as const,
          data: { source_id: object.id, ability_index },
        })),
      },
      spellCosts: {},
    });
    useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });
    return render(<CardPreview cardName={object.name} position={{ x: 20, y: 20 }} />);
  }

  // CR 201.5: the engine ships the self-reference as `~`; the hover panel must show the
  // card's own name. Live defect on the reported Kilo board: Pentad Prism read
  // "Activate — Remove a charge counter from ~".
  it("substitutes ~ with the source name in the activate-label list", () => {
    const object = battlefieldObject({
      id: 409,
      name: "Pentad Prism",
      abilities: [
        {
          description: "Remove a charge counter from ~: Add one mana of any color.",
          effects: [], targets: [], cost: { type: "RemoveCounter" },
          timing: "AnyTime", kind: "Activated",
        },
      ],
    });
    const { container } = inspect(object, [0]);

    expect(screen.getByText(/Remove a charge counter from Pentad Prism/)).toBeInTheDocument();
    expect(container.textContent).not.toContain("~");
  });

  // MULTI-AUTHORITY HOSTILE (Identity/Provenance contract 1): two abilities whose cost
  // text differs ONLY by the `~` token. `activateLabels` dedups on `rawLabel`, so
  // substitution must happen BEFORE the dedup or the panel shows two rows that render
  // identically — one of them still carrying a raw `~`.
  it("collapses two rows whose cost text differs only by the ~ token", () => {
    const object = battlefieldObject({
      id: 409,
      name: "Pentad Prism",
      abilities: [
        {
          description: "Remove a charge counter from ~: Add one mana of any color.",
          effects: [], targets: [], cost: { type: "RemoveCounter" },
          timing: "AnyTime", kind: "Activated",
        },
        {
          description: "Remove a charge counter from Pentad Prism: Add {C}.",
          effects: [], targets: [], cost: { type: "RemoveCounter" },
          timing: "AnyTime", kind: "Activated",
        },
      ],
    });
    const { container } = inspect(object, [0, 1]);

    // Pre-fix the two raw labels differ => 2 rows, one showing `~`.
    expect(screen.getAllByText(/Activate/)).toHaveLength(1);
    expect(container.textContent).not.toContain("~");
  });
});

describe("CardPreview nameless permanent", () => {
  // CR 709.5: a permanent with a shared type line doesn't have the name of a
  // half it hasn't unlocked, and CR 709.5d gives a copy of a Room neither
  // unlocked designation — so it has no name at all. The hover must still
  // answer with the card. An empty name is a real permanent; only `null`
  // means "there is nothing to preview".
  function namelessRoomCopy(overrides: Partial<GameObject> = {}): GameObject {
    return battlefieldObject({
      name: "",
      printed_ref: { oracle_id: "greenhouse-oracle-id", face_name: "Greenhouse" },
      ...overrides,
    });
  }

  it("previews a battlefield permanent whose name is empty", () => {
    const object = namelessRoomCopy();
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });

    const { container } = render(
      <CardPreview cardName="" objectId={object.id} position={{ x: 20, y: 20 }} />,
    );

    expect(container.querySelector("[data-card-preview]")).not.toBeNull();
    // The identity the art is resolved FROM is the object's `printed_ref`,
    // never the name — the empty name reaches the hook untouched beside it.
    expect(useCardImage).toHaveBeenCalledWith(
      "",
      expect.objectContaining({
        oracleId: "greenhouse-oracle-id",
        faceName: "Greenhouse",
      }),
    );
    expect(container.querySelector("img")?.getAttribute("src")).toBe(
      "greenhouse-oracle-id.png",
    );
  });

  it("measures a hand origin for a nameless card the same way", () => {
    // The hand-origin gate asks the same question as the render gate. Left
    // asking for truthiness it withholds `measuredHandOrigin`, and the preview
    // never mounts — the same defect on the other branch.
    const object = namelessRoomCopy({ zone: "Hand" });
    useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
    const source = document.createElement("div");
    source.dataset.handCard = "";
    source.dataset.handRotation = "-4";
    source.dataset.objectId = String(object.id);
    Object.defineProperty(source, "offsetWidth", { configurable: true, value: 120 });
    source.matches = vi.fn((selector) => selector === ":hover");
    source.getBoundingClientRect = () => ({
      bottom: 748, height: 168, left: 220, right: 340, top: 580, width: 120,
      x: 220, y: 580, toJSON: () => ({}),
    });
    document.body.appendChild(source);

    const { container } = render(
      <CardPreview cardName="" objectId={object.id} handSourceObjectId={object.id} />,
    );

    const preview = container.querySelector<HTMLElement>("[data-card-preview]");
    expect(preview).not.toBeNull();
    expect(preview?.style.bottom).toBe("0px");
    source.remove();
  });
});
