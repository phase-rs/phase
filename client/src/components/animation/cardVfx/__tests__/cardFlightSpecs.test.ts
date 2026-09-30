import { describe, expect, it } from "vitest";

import type { GameObject, GameState, ManaColor, TargetRef, Zone } from "../../../../adapter/types.ts";
import type { AnimationEvent } from "../../../../animation/types.ts";
import { buildObjectMap, gameObjectFactory } from "../../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildStackEntry } from "../../../../test/factories/gameStateFactory.ts";
import {
  cardFlightSpecFor,
  type CardFlightSpecContext,
  COUNTER_RIPPLE_MS,
  COUNTER_WASH_MS,
  FLIGHT_BATCH_MAX,
} from "../cardFlightSpecs.ts";
import { cardVfxSpecFor } from "../cardVfxSpecs.ts";

const X = 7;

function stateWith(...objects: GameObject[]): GameState {
  return buildGameState({ objects: buildObjectMap(...objects) });
}

function visible(object: GameObject): GameObject {
  return { ...object, display_visible_to_viewer: true };
}

function hidden(object: GameObject): GameObject {
  return { ...object, display_visible_to_viewer: false };
}

const card = gameObjectFactory.withId(X).named("Llanowar Elves").creature(1, 1);

function context(
  pre: GameState | null,
  post: GameState | null,
  pace = 1,
  stepEvents: readonly AnimationEvent[] = [],
): CardFlightSpecContext {
  return { pre, post, pace, owningStepMs: 500, snapshotSeq: 1, stepEvents };
}

const spellCast: AnimationEvent = {
  type: "SpellCast",
  data: { card_id: X, controller: 0, object_id: X },
};

function zoneChanged(from: Zone, to: Zone): AnimationEvent {
  return { type: "ZoneChanged", data: { object_id: X, from, to } };
}

describe("cardFlightSpecFor", () => {
  it("V3-4a: a cast flies to the stack with the pre face at the start and the post face at the end", () => {
    const pre = stateWith(visible(card.inHand().build()));
    const post = stateWith(visible(card.params({ zone: "Stack" }).build()));

    const spec = cardFlightSpecFor(spellCast, context(pre, post));

    expect(spec).toMatchObject({ objectId: X, route: { from: "Hand", to: "Stack", ownerId: 0 }, pace: 1, owningStepMs: 500 });
    expect(spec?.startFace).toMatchObject({ objectId: X, cardName: "Llanowar Elves" });
    expect(spec?.endFace).toMatchObject({ objectId: X, cardName: "Llanowar Elves" });
  });

  it("V3-4b: a Stack→Battlefield move resolves to the battlefield", () => {
    const pre = stateWith(visible(card.params({ zone: "Stack" }).build()));
    const post = stateWith(visible(card.onBattlefield().build()));

    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, post))?.route)
      .toEqual({ from: "Stack", to: "Battlefield", ownerId: 0 });
  });

  it("V3-4c: a Stack→Graveyard move goes to the owner's pile, not the controller's", () => {
    const instant = gameObjectFactory.withId(X).instant().ownedBy(1).controlledBy(0);
    const pre = stateWith(visible(instant.params({ zone: "Stack" }).build()));
    const post = stateWith(visible(instant.inGraveyard().build()));

    expect(cardFlightSpecFor(zoneChanged("Stack", "Graveyard"), context(pre, post))?.route)
      .toEqual({ from: "Stack", to: "Graveyard", ownerId: 1 });
    // No post object means no owner to route by.
    expect(cardFlightSpecFor(zoneChanged("Stack", "Graveyard"), context(pre, stateWith()))).toBeNull();
  });

  it("V3-4d: a hidden face yields no snapshot, and nothing is derived from its name", () => {
    const pre = stateWith(hidden(card.inHand().named("Secret Hand Card").build()));
    const post = stateWith(visible(card.params({ zone: "Stack" }).build()));

    const spec = cardFlightSpecFor(spellCast, context(pre, post));
    expect(spec?.startFace).toBeNull();
    expect(spec?.endFace).toMatchObject({ cardName: "Llanowar Elves" });
    expect(JSON.stringify(spec)).not.toContain("Secret Hand Card");

    const bothHidden = cardFlightSpecFor(
      spellCast,
      context(pre, stateWith(hidden(card.params({ zone: "Stack" }).build()))),
    );
    expect(bothHidden).toMatchObject({ route: { from: "Hand", to: "Stack", ownerId: 0 }, startFace: null, endFace: null });
  });

  it("V4-3: the landing tint takes the engine's colours, and a hidden face gets none", () => {
    const pre = stateWith(visible(card.params({ zone: "Stack" }).build()));
    const green = stateWith(visible(card.params({ color: ["Green", "White"] }).onBattlefield().build()));
    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, green))?.endColors)
      .toEqual(["Green", "White"]);

    const facedown = stateWith(hidden(card.params({ color: ["Green"] }).onBattlefield().build()));
    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, facedown))?.endColors).toBeNull();
  });

  it("V5-1: a draw flies from the owner's library to their hand, face up only when the engine shows it", () => {
    const pre = stateWith(hidden(card.params({ zone: "Library" }).build()));
    const own = cardFlightSpecFor(zoneChanged("Library", "Hand"), context(pre, stateWith(visible(card.inHand().build()))));
    expect(own).toMatchObject({ route: { from: "Library", to: "Hand", ownerId: 0 }, startFace: null, delayMs: 0 });
    expect(own?.endFace).toMatchObject({ cardName: "Llanowar Elves" });

    const opponentCard = gameObjectFactory.withId(X).named("Secret Draw").ownedBy(1);
    const opponent = cardFlightSpecFor(
      zoneChanged("Library", "Hand"),
      context(pre, stateWith(hidden(opponentCard.inHand().build()))),
    );
    expect(opponent).toMatchObject({ route: { from: "Library", to: "Hand", ownerId: 1 }, startFace: null, endFace: null });
    expect(JSON.stringify(opponent)).not.toContain("Secret Draw");

    // A revealed library top starts face up, so the card does not turn over.
    const revealed = stateWith(visible(card.params({ zone: "Library" }).build()));
    const fromTop = cardFlightSpecFor(zoneChanged("Library", "Hand"), context(revealed, stateWith(visible(card.inHand().build()))));
    expect(fromTop?.startFace).toMatchObject({ cardName: "Llanowar Elves" });

    // No post object, no owner to route to.
    expect(cardFlightSpecFor(zoneChanged("Library", "Hand"), context(pre, stateWith()))).toBeNull();
  });

  it.each([3, 7])("V5-2: %i draws in one step leave one after another inside the step's first half", (count) => {
    const draws = Array.from({ length: count }, (_, i): AnimationEvent => ({
      type: "ZoneChanged",
      data: { object_id: 100 + i, from: "Library", to: "Hand" },
    }));
    // A non-draw zone change in the step does not take a stagger slot.
    const stepEvents = [draws[0], zoneChanged("Stack", "Graveyard"), ...draws.slice(1)];
    const post = stateWith(...draws.map((_, i) => visible(gameObjectFactory.withId(100 + i).inHand().build())));
    for (const pace of [1, 2]) {
      const delays = draws.map(
        (draw) => cardFlightSpecFor(draw, context(null, post, pace, stepEvents))?.delayMs ?? Number.NaN,
      );
      expect(delays[0]).toBe(0);
      for (let i = 1; i < count; i += 1) expect(delays[i]).toBeGreaterThan(delays[i - 1]);
      expect(delays[count - 1]).toBeLessThanOrEqual(500 / 2);
    }
  });

  it("V3-4e: zone moves without a flight and other events have no flight", () => {
    const pre = stateWith(visible(card.inHand().build()));
    const post = stateWith(visible(card.onBattlefield().build()));
    const ctx = context(pre, post);

    expect(cardFlightSpecFor(zoneChanged("Battlefield", "Exile"), ctx)).toBeNull();
    expect(cardFlightSpecFor(zoneChanged("Battlefield", "Graveyard"), ctx)).toBeNull();
    expect(cardFlightSpecFor(zoneChanged("Hand", "Command"), ctx)).toBeNull();
    expect(cardFlightSpecFor({ type: "TokenCreated", data: { object_id: X, name: "Elf", source_id: 3 } }, ctx)).toBeNull();
    expect(
      cardFlightSpecFor(
        { type: "DamageDealt", data: { source_id: X, target: { Player: 1 }, amount: 2, is_combat: false } },
        ctx,
      ),
    ).toBeNull();
    // Reach guard: the same context does route a Stack→Battlefield move.
    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), ctx)).not.toBeNull();
  });

  it("V7-1: a land played from the hand lands on the battlefield face up", () => {
    const pre = stateWith(visible(card.inHand().build()));
    const post = stateWith(visible(card.onBattlefield().build()));

    expect(cardFlightSpecFor(zoneChanged("Hand", "Battlefield"), context(pre, post))).toMatchObject({
      route: { from: "Hand", to: "Battlefield", ownerId: 0 },
      startFace: { cardName: "Llanowar Elves" },
      endFace: { cardName: "Llanowar Elves" },
    });
  });

  it("V7-2: a cast flies from the zone it is cast from, announced or cast", () => {
    const post = stateWith(visible(card.params({ zone: "Stack" }).build()));
    const announced: AnimationEvent = { type: "StackPushed", data: { object_id: X } };

    expect(cardFlightSpecFor(announced, context(stateWith(visible(card.inHand().build())), post))?.route)
      .toEqual({ from: "Hand", to: "Stack", ownerId: 0 });
    expect(cardFlightSpecFor(spellCast, context(stateWith(visible(card.params({ zone: "Graveyard" }).build())), post))?.route)
      .toEqual({ from: "Graveyard", to: "Stack", ownerId: 0 });
    // No pre object means no zone to fly from.
    expect(cardFlightSpecFor(announced, context(null, post))).toBeNull();
  });

  it("V7-4: a cast completing after a paused announcement has no second flight", () => {
    const announced = { ...stateWith(visible(card.inHand().build())), stack: [buildStackEntry({ id: X, source_id: X })] };
    const post = stateWith(visible(card.params({ zone: "Stack" }).build()));

    expect(cardFlightSpecFor(spellCast, context(announced, post))).toBeNull();
  });

  it("V3-4f: pace 0 has no flight; any other pace is carried on the spec", () => {
    const pre = stateWith(visible(card.inHand().build()));
    const post = stateWith(visible(card.params({ zone: "Stack" }).build()));

    expect(cardFlightSpecFor(spellCast, context(pre, post, 0))).toBeNull();
    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, post, 0))).toBeNull();
    expect(cardFlightSpecFor(spellCast, context(pre, post, 1.5))?.pace).toBe(1.5);
  });
});

describe("cardFlightSpecFor zone moves", () => {
  const SOURCE = 3;
  const TOKEN = 50;

  it.each<[Zone, Zone]>([
    ["Stack", "Hand"],
    ["Stack", "Library"],
    ["Stack", "Exile"],
    ["Library", "Graveyard"],
    ["Library", "Exile"],
    ["Library", "Battlefield"],
    ["Hand", "Graveyard"],
    ["Hand", "Library"],
    ["Hand", "Exile"],
    ["Graveyard", "Hand"],
    ["Graveyard", "Battlefield"],
    ["Graveyard", "Library"],
    ["Graveyard", "Exile"],
    ["Exile", "Hand"],
    ["Exile", "Battlefield"],
    ["Exile", "Graveyard"],
    ["Exile", "Library"],
    ["Battlefield", "Hand"],
    ["Battlefield", "Library"],
  ])("V11-1: a %s→%s move flies to its owner's zone", (from, to) => {
    const post = stateWith(visible(card.params({ zone: to, owner: 1 }).build()));
    expect(cardFlightSpecFor(zoneChanged(from, to), context(null, post))).toMatchObject({
      objectId: X,
      sourceId: X,
      route: { from, to, ownerId: 1 },
    });
  });

  it("V11-2: a sacrificed permanent flies to its owner's graveyard; one sent elsewhere or a token that ceased to exist does not", () => {
    const sacrificed: AnimationEvent = { type: "PermanentSacrificed", data: { object_id: X, player_id: 0 } };
    const pre = stateWith(visible(card.onBattlefield().build()));
    const inGraveyard = stateWith(visible(card.params({ zone: "Graveyard", owner: 1 }).build()));

    expect(cardFlightSpecFor(sacrificed, context(pre, inGraveyard))).toMatchObject({
      route: { from: "Battlefield", to: "Graveyard", ownerId: 1 },
      startFace: { cardName: "Llanowar Elves" },
    });
    expect(cardFlightSpecFor(sacrificed, context(pre, stateWith(visible(card.params({ zone: "Exile" }).build()))))).toBeNull();
    expect(cardFlightSpecFor(sacrificed, context(pre, stateWith()))).toBeNull();
  });

  it("V11-3: a token comes out of the spell or permanent that created it, showing its face from the start", () => {
    const created: AnimationEvent = { type: "TokenCreated", data: { object_id: TOKEN, name: "Elf Warrior", source_id: SOURCE } };
    const token = visible(gameObjectFactory.withId(TOKEN).named("Elf Warrior").creature(1, 1).onBattlefield().build());
    const source = gameObjectFactory.withId(SOURCE).named("Elvish Promenade");
    const post = stateWith(token);

    for (const zone of ["Stack", "Battlefield"] as const) {
      const pre = stateWith(visible(source.params({ zone }).build()));
      expect(cardFlightSpecFor(created, context(pre, post))).toMatchObject({
        objectId: TOKEN,
        sourceId: SOURCE,
        route: { from: zone, to: "Battlefield", ownerId: 0 },
        startFace: { cardName: "Elf Warrior" },
        endFace: { cardName: "Elf Warrior" },
      });
    }
    // A source in another zone, or none, has no surface the token leaves from.
    expect(cardFlightSpecFor(created, context(stateWith(visible(source.params({ zone: "Graveyard" }).build())), post))).toBeNull();
    expect(cardFlightSpecFor(created, context(stateWith(), post))).toBeNull();
  });

  it("V11-4: a batch past the cap presents Classic; the rest leave inside the step's first half", () => {
    const count = FLIGHT_BATCH_MAX + 2;
    const mills = Array.from({ length: count }, (_, i): AnimationEvent => ({
      type: "ZoneChanged",
      data: { object_id: 100 + i, from: "Library", to: "Graveyard" },
    }));
    const post = stateWith(...mills.map((_, i) => visible(gameObjectFactory.withId(100 + i).inGraveyard().build())));
    const specs = mills.map((mill) => cardFlightSpecFor(mill, context(null, post, 1, mills)));

    expect(specs.slice(0, FLIGHT_BATCH_MAX).every((spec) => spec !== null)).toBe(true);
    expect(specs.slice(FLIGHT_BATCH_MAX)).toEqual([null, null]);
    expect(specs[FLIGHT_BATCH_MAX - 1]?.delayMs).toBeLessThanOrEqual(500 / 2);
  });
});

describe("cardVfxSpecFor", () => {
  const destroyed: AnimationEvent = { type: "CreatureDestroyed", data: { object_id: X } };

  it("V8-9: a destroyed permanent shatters with the face it showed; flights are unchanged", () => {
    const pre = stateWith(visible(card.onBattlefield().build()));
    const post = stateWith(visible(card.params({ zone: "Graveyard" }).build()));

    expect(cardVfxSpecFor(destroyed, context(pre, post))).toEqual({
      kind: "shatter",
      objectId: X,
      face: expect.objectContaining({ cardName: "Llanowar Elves" }),
      pace: 1,
      owningStepMs: 500,
      snapshotSeq: 1,
    });
    expect(cardVfxSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, post))?.kind).toBe("flight");
  });

  it("V11-5: a destruction or sacrifice a replacement sent to exile, a hand or a library is its zone change's to present", () => {
    const pre = stateWith(visible(card.onBattlefield().build()));
    const sacrificed: AnimationEvent = { type: "PermanentSacrificed", data: { object_id: X, player_id: 0 } };
    for (const zone of ["Exile", "Hand", "Library"] as const) {
      const post = stateWith(visible(card.params({ zone }).build()));
      expect(cardVfxSpecFor(destroyed, context(pre, post))).toEqual({ kind: "covered", objectId: X });
      expect(cardVfxSpecFor(sacrificed, context(pre, post))).toEqual({ kind: "covered", objectId: X });
    }
    // A destroyed token that ceased to exist still shatters; a sacrificed one presents Classic.
    expect(cardVfxSpecFor(destroyed, context(pre, stateWith()))?.kind).toBe("shatter");
    expect(cardVfxSpecFor(sacrificed, context(pre, stateWith()))).toBeNull();
    expect(cardVfxSpecFor(destroyed, context(pre, pre, 0))).toBeNull();
  });

  it("V11-5: a token's entry from no zone has no card VFX; its creation flies it", () => {
    const entered: AnimationEvent = { type: "ZoneChanged", data: { object_id: X, from: null, to: "Battlefield" } };
    const token = stateWith(visible(card.onBattlefield().params({ is_token: true }).build()));
    expect(cardVfxSpecFor(entered, context(null, token))).toBeNull();
  });

  it("V8-9: no pre object or pace 0 has no shatter", () => {
    const pre = stateWith(visible(card.onBattlefield().build()));
    expect(cardVfxSpecFor(destroyed, context(null, pre))).toBeNull();
    expect(cardVfxSpecFor(destroyed, context(pre, pre, 0))).toBeNull();
  });
});

describe("cardVfxSpecFor counters", () => {
  it("V13-3: counters put on or removed from a permanent play as a counter change; pace 0 has none", () => {
    const added: AnimationEvent = { type: "CounterAdded", data: { object_id: X, counter_type: "P1P1", count: 2 } };
    const removed: AnimationEvent = { type: "CounterRemoved", data: { object_id: X, counter_type: "loyalty", count: 1 } };

    expect(cardVfxSpecFor(added, context(null, null))).toEqual({
      kind: "counter",
      objectId: X,
      counterType: "P1P1",
      change: "added",
      count: 2,
      pace: 1,
    });
    expect(cardVfxSpecFor(removed, context(null, null))).toMatchObject({ change: "removed", counterType: "loyalty" });
    expect(cardVfxSpecFor(added, context(null, null, 0))).toBeNull();
  });
});

describe("cardVfxSpecFor exile", () => {
  it("V9-5: a permanent exiled from the battlefield dissolves, toward the permanent holding it if any", () => {
    const pre = stateWith(visible(card.onBattlefield().build()));
    const post = stateWith(visible(card.params({ zone: "Exile" }).build()));

    expect(cardVfxSpecFor(zoneChanged("Battlefield", "Exile"), context(pre, post))).toEqual({
      kind: "dissolve",
      objectId: X,
      face: expect.objectContaining({ cardName: "Llanowar Elves" }),
      holderId: null,
      pace: 1,
      owningStepMs: 500,
      snapshotSeq: 1,
    });

    const held = { ...post, derived: { ...post.derived, linked_exile_ids: { "3": [9, X] } } } as GameState;
    expect(cardVfxSpecFor(zoneChanged("Battlefield", "Exile"), context(pre, held))).toMatchObject({ holderId: 3 });
  });

  it("V9-5: exile from another zone, a missing pre object or pace 0 has no dissolve", () => {
    const pre = stateWith(visible(card.inHand().build()));
    expect(cardVfxSpecFor(zoneChanged("Hand", "Exile"), context(pre, pre))?.kind).toBe("flight");
    expect(cardVfxSpecFor(zoneChanged("Battlefield", "Exile"), context(null, pre))).toBeNull();
    expect(cardVfxSpecFor(zoneChanged("Battlefield", "Exile"), context(pre, pre, 0))).toBeNull();
  });
});

describe("cardVfxSpecFor damage", () => {
  const SPELL = 20;
  const shock = gameObjectFactory.withId(SPELL).named("Shock").instant().params({ zone: "Stack", color: ["Red"] });
  const damage = (target: TargetRef, isCombat = false): AnimationEvent => ({
    type: "DamageDealt",
    data: { source_id: SPELL, target, amount: 2, is_combat: isCombat },
  });
  const pre = buildGameState({
    objects: buildObjectMap(shock.build(), visible(card.onBattlefield().ownedBy(1).build())),
    stack: [buildStackEntry({ id: SPELL, source_id: SPELL })],
  });

  it("V10-11: a resolving spell's damage strikes a player, or a permanent with the face it shows", () => {
    expect(cardVfxSpecFor(damage({ Player: 1 }), context(pre, pre))).toEqual({
      kind: "damage",
      cause: "fire",
      origin: { zone: "Stack", objectId: SPELL, ownerId: 0 },
      target: { kind: "player", playerId: 1 },
      amount: 2,
      pace: 1,
      owningStepMs: 500,
    });
    expect(cardVfxSpecFor(damage({ Object: X }), context(pre, pre))).toMatchObject({
      target: { kind: "permanent", objectId: X, face: expect.objectContaining({ cardName: "Llanowar Elves" }) },
    });
  });

  it("V10-11: combat damage, a missing target or pace 0 has no strike", () => {
    expect(cardVfxSpecFor(damage({ Player: 1 }, true), context(pre, pre))).toBeNull();
    expect(cardVfxSpecFor(damage({ Object: 99 }), context(pre, pre))).toBeNull();
    expect(cardVfxSpecFor(damage({ Player: 1 }), context(pre, pre, 0))).toBeNull();
  });
});

describe("a counter", () => {
  const COUNTER = 20;
  const OTHER = 30;
  const counterspell = (color: ManaColor[]) =>
    gameObjectFactory.withId(COUNTER).named("Counterspell").instant().params({ zone: "Stack", color });
  const giant = gameObjectFactory.withId(X).named("Hill Giant").creature(3, 3).ownedBy(1);
  const other = gameObjectFactory.withId(OTHER).named("Opt").instant();
  const countered: AnimationEvent = { type: "SpellCountered", data: { object_id: X, countered_by: COUNTER } };
  const toGraveyard = (objectId: number): AnimationEvent => ({
    type: "ZoneChanged",
    data: { object_id: objectId, from: "Stack", to: "Graveyard" },
  });
  const step = [countered, toGraveyard(X), toGraveyard(OTHER), toGraveyard(COUNTER)];

  function counterContext(color: ManaColor[], pace = 1) {
    const pre = buildGameState({
      objects: buildObjectMap(
        counterspell(color).build(),
        visible(giant.params({ zone: "Stack" }).build()),
        visible(other.params({ zone: "Stack" }).build()),
      ),
      stack: [buildStackEntry({ id: X, source_id: X }), buildStackEntry({ id: COUNTER, source_id: COUNTER })],
    });
    const post = stateWith(
      counterspell(color).inGraveyard().build(),
      visible(giant.inGraveyard().build()),
      visible(other.inGraveyard().build()),
    );
    return context(pre, post, pace, step);
  }

  it("V14-3: the countered spell waits for the ripple, washes out in its counter's look, then leaves", () => {
    const wait = (COUNTER_RIPPLE_MS + COUNTER_WASH_MS) * 2;
    expect(cardFlightSpecFor(toGraveyard(X), counterContext(["Blue"], 2))).toMatchObject({
      route: { from: "Stack", to: "Graveyard", ownerId: 1 },
      delayMs: wait,
      wash: { atMs: COUNTER_RIPPLE_MS * 2, durationMs: COUNTER_WASH_MS * 2, look: "water" },
    });
    expect(cardFlightSpecFor(toGraveyard(X), counterContext(["White"]))?.wash?.look).toBe("pale");
  });

  it("V14-3: the counter leaves with the spell it countered, unwashed; other moves keep their stagger", () => {
    const ctx = counterContext(["Blue"]);
    expect(cardFlightSpecFor(toGraveyard(COUNTER), ctx)).toMatchObject({
      delayMs: COUNTER_RIPPLE_MS + COUNTER_WASH_MS,
      wash: null,
    });
    // Second in the step's Stack→Graveyard batch: one stagger gap.
    expect(cardFlightSpecFor(step[2], ctx)).toMatchObject({ objectId: OTHER, delayMs: 90, wash: null });
  });

  it("V14-4: a counter's ripple leaves from the resolving counter to the countered entry", () => {
    expect(cardVfxSpecFor(countered, counterContext(["Blue"]))).toEqual({
      kind: "ripple",
      origin: { zone: "Stack", objectId: COUNTER, ownerId: 0 },
      targetId: X,
      look: "water",
      pace: 1,
    });
    expect(cardVfxSpecFor(countered, counterContext(["Black"]))).toMatchObject({ look: "pale" });
    expect(cardVfxSpecFor(countered, counterContext(["Blue"], 0))).toBeNull();
    // A counter that is not what resolves (none on the stack) has no ripple.
    expect(cardVfxSpecFor(countered, context(stateWith(), null))).toBeNull();
  });
});
