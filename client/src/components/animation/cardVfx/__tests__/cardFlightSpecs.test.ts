import { describe, expect, it } from "vitest";

import type { GameObject, GameState, Zone } from "../../../../adapter/types.ts";
import type { AnimationEvent } from "../../../../animation/types.ts";
import { buildObjectMap, gameObjectFactory } from "../../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../../test/factories/gameStateFactory.ts";
import { cardFlightSpecFor, type CardFlightSpecContext } from "../cardFlightSpecs.ts";

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

function context(pre: GameState | null, post: GameState | null, pace = 1): CardFlightSpecContext {
  return { pre, post, pace, owningStepMs: 500 };
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

    expect(spec).toMatchObject({ objectId: X, route: { kind: "cast" }, pace: 1, owningStepMs: 500 });
    expect(spec?.startFace).toMatchObject({ objectId: X, cardName: "Llanowar Elves" });
    expect(spec?.endFace).toMatchObject({ objectId: X, cardName: "Llanowar Elves" });
  });

  it("V3-4b: a Stack→Battlefield move resolves to the battlefield", () => {
    const pre = stateWith(visible(card.params({ zone: "Stack" }).build()));
    const post = stateWith(visible(card.onBattlefield().build()));

    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, post))?.route)
      .toEqual({ kind: "resolveToBattlefield" });
  });

  it("V3-4c: a Stack→Graveyard move goes to the owner's pile, not the controller's", () => {
    const instant = gameObjectFactory.withId(X).instant().ownedBy(1).controlledBy(0);
    const pre = stateWith(visible(instant.params({ zone: "Stack" }).build()));
    const post = stateWith(visible(instant.inGraveyard().build()));

    expect(cardFlightSpecFor(zoneChanged("Stack", "Graveyard"), context(pre, post))?.route)
      .toEqual({ kind: "resolveToGraveyard", ownerId: 1 });
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
    expect(bothHidden).toMatchObject({ route: { kind: "cast" }, startFace: null, endFace: null });
  });

  it("V4-3: the landing tint takes the engine's colours, and a hidden face gets none", () => {
    const pre = stateWith(visible(card.params({ zone: "Stack" }).build()));
    const green = stateWith(visible(card.params({ color: ["Green", "White"] }).onBattlefield().build()));
    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, green))?.endColors)
      .toEqual(["Green", "White"]);

    const facedown = stateWith(hidden(card.params({ color: ["Green"] }).onBattlefield().build()));
    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, facedown))?.endColors).toBeNull();
  });

  it("V3-4e: land plays, other zone moves and other events have no flight", () => {
    const pre = stateWith(visible(card.inHand().build()));
    const post = stateWith(visible(card.onBattlefield().build()));
    const ctx = context(pre, post);

    expect(cardFlightSpecFor(zoneChanged("Hand", "Battlefield"), ctx)).toBeNull();
    expect(cardFlightSpecFor(zoneChanged("Stack", "Exile"), ctx)).toBeNull();
    expect(cardFlightSpecFor(zoneChanged("Library", "Hand"), ctx)).toBeNull();
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

  it("V3-4f: pace 0 has no flight; any other pace is carried on the spec", () => {
    const pre = stateWith(visible(card.inHand().build()));
    const post = stateWith(visible(card.params({ zone: "Stack" }).build()));

    expect(cardFlightSpecFor(spellCast, context(pre, post, 0))).toBeNull();
    expect(cardFlightSpecFor(zoneChanged("Stack", "Battlefield"), context(pre, post, 0))).toBeNull();
    expect(cardFlightSpecFor(spellCast, context(pre, post, 1.5))?.pace).toBe(1.5);
  });
});
