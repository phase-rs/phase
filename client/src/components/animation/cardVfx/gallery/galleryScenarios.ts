import type { GameEvent, GameObject, GameState, ManaColor, ObjectId, TargetRef, Zone } from "../../../../adapter/types.ts";
import { gameObjectFactory } from "../../../../test/factories/gameObjectFactory.ts";
import { buildStackEntry, gameStateFactory } from "../../../../test/factories/gameStateFactory.ts";

/**
 * One engine update the gallery replays. It edits a copy of the state before
 * into the state the update leaves, and returns the events the engine reports
 * for it.
 */
export type GalleryBatch = (state: GameState) => GameEvent[];

/** An effect the gallery shows, as the engine updates that produce it. */
export interface GalleryScenario {
  title: string;
  description: string;
  batches: GalleryBatch[];
}

const YOU = 0;
const OPPONENT = 1;

// Your hand, library, graveyard and battlefield.
const BOLT = 1;
const ELVES = 2;
const COUNTERSPELL = 3;
const FOREST_IN_HAND = 4;
const PSIONIC_BLAST = 5;
const EVACUATION = 6;
const LIBRARY = [10, 11, 12, 13, 14];
const OPT = 15;
const BEARS = 20;
const ANGEL = 21;
const LANDS = [22, 23, 24];
// The opponent's.
const OPPONENT_HAND = [30, 31, 32];
const HILL_GIANT_IN_HAND = 31;
const WRATH = 33;
const DAMNATION = 34;
const OPPONENT_LIBRARY = [40, 41, 42, 43, 44];
const DURESS = 45;
const HILL_GIANT = 50;
const AIR_ELEMENTAL = 51;
const OPPONENT_LANDS = [52, 53];
const TOKEN = 60;

interface CardSpec {
  id: ObjectId;
  name: string;
  owner?: number;
  zone: Zone;
  colors?: ManaColor[];
  creature?: [number, number];
  kind?: "instant" | "sorcery" | "land";
  /** Whether the viewer (you) sees its face: false for libraries and the opponent's hand. */
  visible?: boolean;
}

function card({ id, name, owner = YOU, zone, colors = [], creature, kind, visible = true }: CardSpec): GameObject {
  let factory = gameObjectFactory.withId(id).named(name).ownedBy(owner);
  if (creature) factory = factory.creature(...creature);
  if (kind === "instant") factory = factory.instant();
  if (kind === "sorcery") factory = factory.sorcery();
  if (kind === "land") factory = factory.land();
  const object = factory.build({ zone, color: colors, base_color: colors });
  object.display_visible_to_viewer = visible;
  return object;
}

/** The board every scenario starts from: two players mid-game, each with a hand,
 *  a library, a graveyard and permanents in play. */
export function galleryBoard(): GameState {
  const objects = [
    card({ id: BOLT, name: "Lightning Bolt", zone: "Hand", colors: ["Red"], kind: "instant" }),
    card({ id: ELVES, name: "Llanowar Elves", zone: "Hand", colors: ["Green"], creature: [1, 1] }),
    card({ id: COUNTERSPELL, name: "Counterspell", zone: "Hand", colors: ["Blue"], kind: "instant" }),
    card({ id: FOREST_IN_HAND, name: "Forest", zone: "Hand", kind: "land" }),
    card({ id: PSIONIC_BLAST, name: "Psionic Blast", zone: "Hand", colors: ["Blue"], kind: "instant" }),
    card({ id: EVACUATION, name: "Evacuation", zone: "Hand", colors: ["Blue"], kind: "instant" }),
    ...LIBRARY.map((id, i) =>
      card({ id, name: ["Opt", "Island", "Shock", "Plains", "Forest"][i], zone: "Library", visible: false }),
    ),
    card({ id: OPT, name: "Opt", zone: "Graveyard", colors: ["Blue"], kind: "instant" }),
    card({ id: BEARS, name: "Grizzly Bears", zone: "Battlefield", colors: ["Green"], creature: [2, 2] }),
    card({ id: ANGEL, name: "Serra Angel", zone: "Battlefield", colors: ["White"], creature: [4, 4] }),
    ...LANDS.map((id, i) => card({ id, name: ["Forest", "Island", "Mountain"][i], zone: "Battlefield", kind: "land" })),
    ...OPPONENT_HAND.map((id, i) =>
      card({
        id,
        name: ["Doom Blade", "Hill Giant", "Swamp"][i],
        owner: OPPONENT,
        zone: "Hand",
        colors: [["Black"], ["Red"], []][i] as ManaColor[],
        creature: id === HILL_GIANT_IN_HAND ? [3, 3] : undefined,
        visible: false,
      }),
    ),
    card({ id: WRATH, name: "Wrath of God", owner: OPPONENT, zone: "Hand", colors: ["White"], kind: "sorcery", visible: false }),
    card({ id: DAMNATION, name: "Damnation", owner: OPPONENT, zone: "Hand", colors: ["Black"], kind: "sorcery", visible: false }),
    ...OPPONENT_LIBRARY.map((id) => card({ id, name: "Swamp", owner: OPPONENT, zone: "Library", visible: false })),
    card({ id: DURESS, name: "Duress", owner: OPPONENT, zone: "Graveyard", colors: ["Black"], kind: "sorcery" }),
    card({ id: HILL_GIANT, name: "Hill Giant", owner: OPPONENT, zone: "Battlefield", colors: ["Red"], creature: [3, 3] }),
    card({ id: AIR_ELEMENTAL, name: "Air Elemental", owner: OPPONENT, zone: "Battlefield", colors: ["Blue"], creature: [4, 4] }),
    ...OPPONENT_LANDS.map((id, i) =>
      card({ id, name: ["Swamp", "Plains"][i], owner: OPPONENT, zone: "Battlefield", kind: "land" }),
    ),
  ];
  const zone = (owner: number, z: Zone) => objects.filter((o) => o.owner === owner && o.zone === z).map((o) => o.id);
  return gameStateFactory
    .withPlayers(
      { id: YOU, life: 20, hand: zone(YOU, "Hand"), library: zone(YOU, "Library"), graveyard: zone(YOU, "Graveyard") },
      {
        id: OPPONENT,
        life: 20,
        hand: zone(OPPONENT, "Hand"),
        library: zone(OPPONENT, "Library"),
        graveyard: zone(OPPONENT, "Graveyard"),
      },
    )
    .withObjects(...objects)
    .build();
}

/** Moves `id` to `to` as the engine's state would show it afterwards. */
function move(state: GameState, id: ObjectId, to: Zone, { visible = true } = {}) {
  const object = state.objects[id];
  for (const player of state.players) {
    player.hand = player.hand.filter((x) => x !== id);
    player.library = player.library.filter((x) => x !== id);
    player.graveyard = player.graveyard.filter((x) => x !== id);
  }
  state.battlefield = state.battlefield.filter((x) => x !== id);
  state.exile = state.exile.filter((x) => x !== id);
  state.stack = state.stack.filter((entry) => entry.id !== id);
  const owner = state.players[object.owner];
  switch (to) {
    case "Hand":
      owner.hand.push(id);
      break;
    case "Library":
      owner.library.unshift(id);
      break;
    case "Graveyard":
      owner.graveyard.push(id);
      break;
    case "Battlefield":
      state.battlefield.push(id);
      break;
    case "Exile":
      state.exile.push(id);
      break;
    case "Stack":
      state.stack.push(
        buildStackEntry({
          id,
          source_id: id,
          controller: object.controller,
          kind: { type: "Spell", data: { card_id: object.card_id, ability: { targets: [] } } },
        }),
      );
      break;
  }
  state.objects[id] = { ...object, zone: to, display_visible_to_viewer: visible };
}

const zoneChanged = (object_id: ObjectId, from: Zone | null, to: Zone): GameEvent => ({
  type: "ZoneChanged",
  data: { object_id, from, to },
});

/** Casting `id` from its owner's hand: it goes to the stack, face up. */
function cast(id: ObjectId, controller: number): GalleryBatch {
  return (state) => {
    move(state, id, "Stack");
    return [{ type: "SpellCast", data: { card_id: id, controller, object_id: id } }, zoneChanged(id, "Hand", "Stack")];
  };
}

/** Each of `ids` moving from `from` to `to`, with `event` reported after each
 *  move, as the engine reports a destruction after the move it makes. */
function moveAll(ids: ObjectId[], from: Zone, to: Zone, event?: (id: ObjectId) => GameEvent): GalleryBatch {
  return (state) =>
    ids.flatMap((id) => {
      move(state, id, to);
      return event ? [zoneChanged(id, from, to), event(id)] : [zoneChanged(id, from, to)];
    });
}

function lifeChanged(player_id: number, amount: number, state: GameState): GameEvent {
  const player = state.players[player_id];
  player.life += amount;
  return { type: "LifeChanged", data: { player_id, amount, new_total: player.life } };
}

const CREATURES = [BEARS, ANGEL, HILL_GIANT, AIR_ELEMENTAL];

/** A destruction by `source_id`'s destroy instruction; `null` for a death from
 *  lethal damage, as the engine reports it. */
const destroyedBy =
  (source_id: ObjectId | null) =>
  (object_id: ObjectId): GameEvent => ({ type: "CreatureDestroyed", data: { object_id, source_id } });
const diedOfDamage = destroyedBy(null);

const damage = (source_id: ObjectId, target: TargetRef, amount: number, is_combat = false): GameEvent => ({
  type: "DamageDealt",
  data: { source_id, target, amount, is_combat },
});

export const GALLERY_SCENARIOS: Record<string, GalleryScenario> = {
  castAndResolve: {
    title: "Cast and resolve a creature",
    description: "Llanowar Elves flies from hand to the stack, then resolves onto the battlefield.",
    batches: [
      cast(ELVES, YOU),
      moveAll([ELVES], "Stack", "Battlefield", (object_id) => ({ type: "StackResolved", data: { object_id } })),
    ],
  },
  playLand: {
    title: "Play a land",
    description: "A Forest goes straight from hand to the battlefield.",
    batches: [
      moveAll([FOREST_IN_HAND], "Hand", "Battlefield", (object_id) => ({
        type: "LandPlayed",
        data: { object_id, player_id: YOU, from_zone: "Hand" },
      })),
    ],
  },
  draw: {
    title: "Draw two cards",
    description: "Your top two cards fly from the library and turn face up into your hand.",
    batches: [
      moveAll(LIBRARY.slice(0, 2), "Library", "Hand", (object_id) => ({
        type: "CardDrawn",
        data: { player_id: YOU, object_id, nth_in_turn: 1, nth_in_step: 1 },
      })),
    ],
  },
  opponentDraw: {
    title: "Opponent draws",
    description: "The opponent's card stays face down on its way to their hand.",
    batches: [
      (state) => {
        move(state, OPPONENT_LIBRARY[0], "Hand", { visible: false });
        return [
          { type: "CardDrawn", data: { player_id: OPPONENT, object_id: OPPONENT_LIBRARY[0], nth_in_turn: 1, nth_in_step: 1 } },
          zoneChanged(OPPONENT_LIBRARY[0], "Library", "Hand"),
        ];
      },
    ],
  },
  discard: {
    title: "Discard",
    description: "Lightning Bolt goes from your hand to your graveyard.",
    batches: [
      moveAll([BOLT], "Hand", "Graveyard", (object_id) => ({ type: "Discarded", data: { player_id: YOU, object_id } })),
    ],
  },
  mill: {
    title: "Mill three",
    description: "Your top three cards turn over into your graveyard, one after another.",
    batches: [
      moveAll(LIBRARY.slice(0, 3), "Library", "Graveyard", (object_id) => ({
        type: "Milled",
        data: { player_id: YOU, object_id, to: "Graveyard" },
      })),
    ],
  },
  destroy: {
    title: "Destroy a creature",
    description: "The opponent's Hill Giant cracks and shatters where it stands.",
    batches: [moveAll([HILL_GIANT], "Battlefield", "Graveyard", diedOfDamage)],
  },
  boardWipe: {
    title: "Board wipe (Wrath of God)",
    description: "The opponent casts Wrath of God, and a wall of light breaks every creature it passes.",
    batches: [
      cast(WRATH, OPPONENT),
      (state) => [
        ...moveAll(CREATURES, "Battlefield", "Graveyard", destroyedBy(WRATH))(state),
        { type: "EffectResolved", data: { kind: "Destroy", source_id: WRATH } },
        ...moveAll([WRATH], "Stack", "Graveyard")(state),
      ],
    ],
  },
  blackWipe: {
    title: "Board wipe (Damnation)",
    description: "The opponent casts Damnation, and smoke rolls over every creature.",
    batches: [
      cast(DAMNATION, OPPONENT),
      (state) => [
        ...moveAll(CREATURES, "Battlefield", "Graveyard", destroyedBy(DAMNATION))(state),
        { type: "EffectResolved", data: { kind: "Destroy", source_id: DAMNATION } },
        ...moveAll([DAMNATION], "Stack", "Graveyard")(state),
      ],
    ],
  },
  exile: {
    title: "Exile a creature",
    description: "Serra Angel dissolves into flakes as it leaves the battlefield.",
    batches: [moveAll([ANGEL], "Battlefield", "Exile")],
  },
  bounce: {
    title: "Return a creature to hand",
    description: "Grizzly Bears flies from the battlefield back to your hand.",
    batches: [moveAll([BEARS], "Battlefield", "Hand")],
  },
  massBounce: {
    title: "Mass bounce (Evacuation)",
    description: "Evacuation sends a wave over the board, washing every creature back to its owner's hand.",
    batches: [
      cast(EVACUATION, YOU),
      (state) => {
        const events = moveAll(CREATURES, "Battlefield", "Hand")(state);
        // The opponent's creatures go back into a hand you cannot see.
        for (const id of [HILL_GIANT, AIR_ELEMENTAL]) {
          state.objects[id] = { ...state.objects[id], display_visible_to_viewer: false };
        }
        return [...events, { type: "EffectResolved", data: { kind: "ChangeZone", source_id: EVACUATION } },
        ...moveAll([EVACUATION], "Stack", "Graveyard")(state)];
      },
    ],
  },
  sacrifice: {
    title: "Sacrifice",
    description: "You sacrifice Grizzly Bears; it goes to your graveyard.",
    batches: [
      moveAll([BEARS], "Battlefield", "Graveyard", (object_id) => ({
        type: "PermanentSacrificed",
        data: { object_id, player_id: YOU },
      })),
    ],
  },
  token: {
    title: "Create a token",
    description: "A Spirit token comes out of Serra Angel onto the battlefield.",
    batches: [
      (state) => {
        const token = card({ id: TOKEN, name: "Spirit", zone: "Battlefield", colors: ["White"], creature: [1, 1] });
        state.objects[TOKEN] = { ...token, is_token: true, display_source: "Token" };
        state.battlefield.push(TOKEN);
        return [
          { type: "TokenCreated", data: { object_id: TOKEN, name: "Spirit", source_id: ANGEL } },
          zoneChanged(TOKEN, null, "Battlefield"),
        ];
      },
    ],
  },
  burnCreature: {
    title: "Burn a creature (red spell)",
    description: "Lightning Bolt throws fire at Hill Giant, which chars and burns away.",
    batches: [
      cast(BOLT, YOU),
      (state) => [
        damage(BOLT, { Object: HILL_GIANT }, 3),
        { type: "EffectResolved", data: { kind: "DealDamage", source_id: BOLT } },
        ...moveAll([BOLT], "Stack", "Graveyard")(state),
        ...moveAll([HILL_GIANT], "Battlefield", "Graveyard", diedOfDamage)(state),
      ],
    ],
  },
  burnPlayer: {
    title: "Burn a player (red spell)",
    description: "Lightning Bolt hits the opponent for 3.",
    batches: [
      cast(BOLT, YOU),
      (state) => [
        damage(BOLT, { Player: OPPONENT }, 3),
        lifeChanged(OPPONENT, -3, state),
        { type: "EffectResolved", data: { kind: "DealDamage", source_id: BOLT } },
        ...moveAll([BOLT], "Stack", "Graveyard")(state),
      ],
    ],
  },
  blueDamage: {
    title: "Blue damage spell",
    description: "Psionic Blast deals 4 to the opponent and 2 to you.",
    batches: [
      cast(PSIONIC_BLAST, YOU),
      (state) => [
        damage(PSIONIC_BLAST, { Player: OPPONENT }, 4),
        lifeChanged(OPPONENT, -4, state),
        damage(PSIONIC_BLAST, { Player: YOU }, 2),
        lifeChanged(YOU, -2, state),
        { type: "EffectResolved", data: { kind: "DealDamage", source_id: PSIONIC_BLAST } },
        ...moveAll([PSIONIC_BLAST], "Stack", "Graveyard")(state),
      ],
    ],
  },
  counterspell: {
    title: "Counter a spell",
    description: "The opponent casts Hill Giant; you counter it with Counterspell.",
    batches: [
      cast(HILL_GIANT_IN_HAND, OPPONENT),
      cast(COUNTERSPELL, YOU),
      (state) => [
        { type: "SpellCountered", data: { object_id: HILL_GIANT_IN_HAND, countered_by: COUNTERSPELL } },
        ...moveAll([HILL_GIANT_IN_HAND], "Stack", "Graveyard")(state),
        // As the engine reports it: the counter resolves, then leaves.
        { type: "EffectResolved", data: { kind: "Counter", source_id: COUNTERSPELL } },
        ...moveAll([COUNTERSPELL], "Stack", "Graveyard")(state),
      ],
    ],
  },
  combat: {
    title: "Combat damage",
    description:
      "Serra Angel and Grizzly Bears attack and Hill Giant blocks the Bears; the Angel hits the opponent, and the Bears die trading blows.",
    batches: [
      (state) => {
        state.objects[BEARS] = { ...state.objects[BEARS], tapped: true };
        state.combat = {
          attackers: [ANGEL, BEARS].map((object_id) => ({
            object_id,
            defending_player: OPPONENT,
            attack_target: { type: "Player", data: OPPONENT },
          })),
          blocker_assignments: {},
          blocker_to_attacker: {},
          blockers_declared_by: [],
          pending_blocker_declaration_events: [],
          damage_assignments: {},
          first_strike_done: false,
          damage_step_index: null,
          pending_damage: [],
          regular_damage_done: false,
        };
        return [{ type: "AttackersDeclared", data: { attacker_ids: [ANGEL, BEARS], defending_player: OPPONENT } }];
      },
      (state) => {
        const combat = state.combat!;
        combat.blocker_assignments = { [BEARS]: [HILL_GIANT] };
        combat.blocker_to_attacker = { [HILL_GIANT]: [BEARS] };
        combat.blockers_declared_by = [OPPONENT];
        return [{ type: "BlockersDeclared", data: { assignments: [[HILL_GIANT, BEARS]] } }];
      },
      (state) => {
        state.objects[HILL_GIANT] = { ...state.objects[HILL_GIANT], damage_marked: 2 };
        return [
          damage(ANGEL, { Player: OPPONENT }, 4, true),
          lifeChanged(OPPONENT, -4, state),
          damage(BEARS, { Object: HILL_GIANT }, 2, true),
          damage(HILL_GIANT, { Object: BEARS }, 3, true),
          ...moveAll([BEARS], "Battlefield", "Graveyard", diedOfDamage)(state),
        ];
      },
    ],
  },
  lifeGain: {
    title: "Gain life",
    description: "You gain 4 life.",
    batches: [(state) => [lifeChanged(YOU, 4, state)]],
  },
  lifeLoss: {
    title: "Lose life",
    description: "The opponent loses 2 life, not from damage.",
    batches: [(state) => [lifeChanged(OPPONENT, -2, state)]],
  },
  counters: {
    title: "Add and remove counters",
    description: "Grizzly Bears gets two +1/+1 counters, then loses one.",
    batches: [
      (state) => {
        state.objects[BEARS] = { ...state.objects[BEARS], counters: { P1P1: 2 }, power: 4, toughness: 4 };
        return [{ type: "CounterAdded", data: { object_id: BEARS, counter_type: "P1P1", count: 2 } }];
      },
      (state) => {
        state.objects[BEARS] = { ...state.objects[BEARS], counters: { P1P1: 1 }, power: 3, toughness: 3 };
        return [{ type: "CounterRemoved", data: { object_id: BEARS, counter_type: "P1P1", count: 1 } }];
      },
    ],
  },
};
