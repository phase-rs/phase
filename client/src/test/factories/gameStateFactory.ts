import { Factory } from "fishery";

import type {
  FormatConfig,
  GameAction,
  GameObject,
  GameState,
  LegalActionsResult,
  ObjectId,
  PendingCast,
  Phase,
  Player,
  PlayerId,
  StackEntry,
  TargetSelectionProgress,
  TargetSelectionSlot,
  WaitingFor,
} from "../../adapter/types.ts";
import { buildObjectMap } from "./gameObjectFactory.ts";

type PriorityWaitingFor = Extract<WaitingFor, { type: "Priority" }>;
type ManaPaymentWaitingFor = Extract<WaitingFor, { type: "ManaPayment" }>;
type TargetSelectionWaitingFor = Extract<WaitingFor, { type: "TargetSelection" }>;
type TriggerTargetSelectionWaitingFor = Extract<
  WaitingFor,
  { type: "TriggerTargetSelection" }
>;
type ChooseXValueWaitingFor = Extract<WaitingFor, { type: "ChooseXValue" }>;
type AssistPaymentWaitingFor = Extract<WaitingFor, { type: "AssistPayment" }>;

/**
 * Convenience-method factory for `Player`:
 * `playerFactory.withId(1).withLife(12).withHand(3, 4).build()`.
 */
export class PlayerFactory extends Factory<Player> {
  withId(id: PlayerId) {
    return this.params({ id });
  }

  withLife(life: number) {
    return this.params({ life });
  }

  withHand(...cards: ObjectId[]) {
    return this.params({ hand: cards });
  }

  withLibrary(...cards: ObjectId[]) {
    return this.params({ library: cards });
  }

  withGraveyard(...cards: ObjectId[]) {
    return this.params({ graveyard: cards });
  }
}

// Player ids are 0-based seat numbers, so the sequence is shifted down by one.
export const playerFactory = PlayerFactory.define(({ sequence }): Player => ({
  id: sequence - 1,
  life: 20,
  poison_counters: 0,
  mana_pool: { mana: [] },
  library: [],
  hand: [],
  graveyard: [],
  has_drawn_this_turn: false,
  lands_played_this_turn: 0,
  turns_taken: 0,
}));

export const buildPlayer = (overrides: Partial<Player> = {}): Player => {
  return { ...playerFactory.build(), ...overrides };
};

export const buildPlayers = (
  players: Array<PlayerId | Partial<Player>>,
): Player[] => {
  return players.map((player) =>
    typeof player === "number" ? buildPlayer({ id: player }) : buildPlayer(player),
  );
};

export const formatConfigFactory = Factory.define<FormatConfig>(() => ({
  format: "Standard",
  starting_life: 20,
  min_players: 2,
  max_players: 2,
  deck_size: 60,
  singleton: false,
  command_zone: false,
  commander_damage_threshold: null,
  range_of_influence: null,
  team_based: false,
  uses_commander: false,
  allow_debug_actions: false,
}));

export const buildFormatConfig = (
  overrides: Partial<FormatConfig> = {},
): FormatConfig => {
  return { ...formatConfigFactory.build(), ...overrides };
};

export const buildCommanderFormatConfig = (
  overrides: Partial<FormatConfig> = {},
): FormatConfig => {
  return buildFormatConfig({
    format: "Commander",
    starting_life: 40,
    min_players: 2,
    max_players: 4,
    deck_size: 100,
    singleton: true,
    command_zone: true,
    commander_damage_threshold: 21,
    uses_commander: true,
    ...overrides,
  });
};

export const priorityWaitingForFactory = Factory.define<PriorityWaitingFor>(() => ({
  type: "Priority",
  data: { player: 0 },
}));

export const buildPriorityWaitingFor = (
  overrides: Partial<PriorityWaitingFor> = {},
): PriorityWaitingFor => {
  return { ...priorityWaitingForFactory.build(), ...overrides };
};

export const manaPaymentWaitingForFactory = Factory.define<ManaPaymentWaitingFor>(() => ({
  type: "ManaPayment",
  data: { player: 0 },
}));

export const buildManaPaymentWaitingFor = (
  overrides: Partial<ManaPaymentWaitingFor> = {},
): ManaPaymentWaitingFor => {
  return { ...manaPaymentWaitingForFactory.build(), ...overrides };
};

export const pendingCastFactory = Factory.define<PendingCast>(() => ({
  object_id: 1,
  card_id: 1,
  ability: { targets: [] },
  cost: { type: "NoCost" },
}));

export const buildPendingCast = (
  overrides: Partial<PendingCast> = {},
): PendingCast => {
  return { ...pendingCastFactory.build(), ...overrides };
};

export const targetSelectionSlotFactory = Factory.define<TargetSelectionSlot>(() => ({
  legal_targets: [],
  optional: false,
}));

export const buildTargetSelectionSlot = (
  overrides: Partial<TargetSelectionSlot> = {},
): TargetSelectionSlot => {
  return { ...targetSelectionSlotFactory.build(), ...overrides };
};

export const targetSelectionProgressFactory =
  Factory.define<TargetSelectionProgress>(() => ({
    current_slot: 0,
    current_legal_targets: [],
  }));

export const buildTargetSelectionProgress = (
  overrides: Partial<TargetSelectionProgress> = {},
): TargetSelectionProgress => {
  return { ...targetSelectionProgressFactory.build(), ...overrides };
};

export const targetSelectionWaitingForFactory =
  Factory.define<TargetSelectionWaitingFor>(() => ({
    type: "TargetSelection",
    data: {
      player: 0,
      pending_cast: buildPendingCast(),
      target_slots: [buildTargetSelectionSlot()],
      selection: buildTargetSelectionProgress(),
    },
  }));

export const buildTargetSelectionWaitingFor = (
  overrides: Partial<TargetSelectionWaitingFor> = {},
): TargetSelectionWaitingFor => {
  return { ...targetSelectionWaitingForFactory.build(), ...overrides };
};

export const triggerTargetSelectionWaitingForFactory =
  Factory.define<TriggerTargetSelectionWaitingFor>(() => ({
    type: "TriggerTargetSelection",
    data: {
      player: 0,
      target_slots: [buildTargetSelectionSlot()],
      selection: buildTargetSelectionProgress(),
    },
  }));

export const buildTriggerTargetSelectionWaitingFor = (
  overrides: Partial<TriggerTargetSelectionWaitingFor> = {},
): TriggerTargetSelectionWaitingFor => {
  return { ...triggerTargetSelectionWaitingForFactory.build(), ...overrides };
};

export const chooseXValueWaitingForFactory =
  Factory.define<ChooseXValueWaitingFor>(() => ({
    type: "ChooseXValue",
    data: {
      player: 0,
      max: 0,
      pending_cast: buildPendingCast(),
    },
  }));

export const buildChooseXValueWaitingFor = (
  overrides: Partial<ChooseXValueWaitingFor> = {},
): ChooseXValueWaitingFor => {
  return { ...chooseXValueWaitingForFactory.build(), ...overrides };
};

export const assistPaymentWaitingForFactory =
  Factory.define<AssistPaymentWaitingFor>(() => ({
    type: "AssistPayment",
    data: {
      caster: 1,
      chosen: 0,
      max_generic: 0,
    },
  }));

export const buildAssistPaymentWaitingFor = (
  overrides: Partial<AssistPaymentWaitingFor> = {},
): AssistPaymentWaitingFor => {
  return { ...assistPaymentWaitingForFactory.build(), ...overrides };
};

interface WaitingForTransient {
  variant?: WaitingFor;
}

/**
 * Convenience-method factory over the `WaitingFor` union — one method per
 * variant, defaults sourced from the per-variant factories above:
 *
 *   waitingForFactory.targetSelection({ player: 1 }).build()
 *
 * The variant is carried as a transient param (not `params`) so switching
 * variants never deep-merges one variant's `data` keys into another's.
 * Use one variant method per chain.
 */
export class WaitingForFactory extends Factory<WaitingFor, WaitingForTransient> {
  priority(player: PlayerId = 0) {
    return this.variant(buildPriorityWaitingFor({ data: { player } }));
  }

  manaPayment(player: PlayerId = 0) {
    return this.variant(buildManaPaymentWaitingFor({ data: { player } }));
  }

  targetSelection(data: Partial<TargetSelectionWaitingFor["data"]> = {}) {
    const base = buildTargetSelectionWaitingFor();
    return this.variant({ ...base, data: { ...base.data, ...data } });
  }

  triggerTargetSelection(
    data: Partial<TriggerTargetSelectionWaitingFor["data"]> = {},
  ) {
    const base = buildTriggerTargetSelectionWaitingFor();
    return this.variant({ ...base, data: { ...base.data, ...data } });
  }

  chooseXValue(data: Partial<ChooseXValueWaitingFor["data"]> = {}) {
    const base = buildChooseXValueWaitingFor();
    return this.variant({ ...base, data: { ...base.data, ...data } });
  }

  assistPayment(data: Partial<AssistPaymentWaitingFor["data"]> = {}) {
    const base = buildAssistPaymentWaitingFor();
    return this.variant({ ...base, data: { ...base.data, ...data } });
  }

  private variant(variant: WaitingFor) {
    return this.transient({ variant });
  }
}

export const waitingForFactory = WaitingForFactory.define(
  ({ transientParams }) => transientParams.variant ?? buildPriorityWaitingFor(),
);

export const stackEntryFactory = Factory.define<StackEntry>(({ sequence }) => ({
  id: sequence,
  source_id: 1,
  controller: 0,
  kind: {
    type: "Spell",
    data: {
      card_id: 1,
      ability: { targets: [] },
    },
  },
}));

export const buildStackEntry = (
  overrides: Partial<StackEntry> = {},
): StackEntry => {
  return { ...stackEntryFactory.build(), ...overrides };
};

export const legalActionsResultFactory = Factory.define<LegalActionsResult>(() => ({
  actions: [],
  autoPassRecommended: false,
}));

export const buildLegalActionsResult = (
  overrides: Partial<LegalActionsResult> = {},
): LegalActionsResult => {
  return { ...legalActionsResultFactory.build(), ...overrides };
};

export const buildGameActions = (...actions: GameAction[]): GameAction[] => actions;

/**
 * Convenience-method factory for `GameState`. Methods chain and compose:
 *
 *   gameStateFactory
 *     .withPlayers(0, 1)
 *     .withObjects(bolt, bear)      // derives objects map, battlefield, next_object_id
 *     .targetSelection({ player: 0 })
 *     .build()
 *
 * Prefer chaining these over passing raw override objects to `.build()`.
 */
export class GameStateFactory extends Factory<GameState> {
  // --- Turn structure ---
  activePlayer(player: PlayerId) {
    return this.params({ active_player: player, priority_player: player });
  }

  inPhase(phase: Phase) {
    return this.params({ phase });
  }

  onTurn(turn: number) {
    return this.params({ turn_number: turn });
  }

  // --- Players / objects / stack ---
  withPlayers(...players: Array<PlayerId | Partial<Player>>) {
    const built = buildPlayers(players);
    return this.params({ players: built, seat_order: built.map((p) => p.id) });
  }

  /** Sets `objects` and derives `battlefield` and `next_object_id` from them. */
  withObjects(...objects: GameObject[]) {
    return this.params({
      objects: buildObjectMap(...objects),
      battlefield: objects.filter((o) => o.zone === "Battlefield").map((o) => o.id),
      next_object_id: objects.length
        ? Math.max(...objects.map((o) => o.id)) + 1
        : 1,
    });
  }

  withStack(...entries: StackEntry[]) {
    return this.params({ stack: entries });
  }

  commander() {
    return this.params({ format_config: buildCommanderFormatConfig() });
  }

  // --- WaitingFor variants (each replaces `waiting_for` exactly) ---
  priority(player: PlayerId = 0) {
    return this.waitingFor(waitingForFactory.priority(player).build());
  }

  manaPayment(player: PlayerId = 0) {
    return this.waitingFor(waitingForFactory.manaPayment(player).build());
  }

  targetSelection(data: Partial<TargetSelectionWaitingFor["data"]> = {}) {
    return this.waitingFor(waitingForFactory.targetSelection(data).build());
  }

  triggerTargetSelection(
    data: Partial<TriggerTargetSelectionWaitingFor["data"]> = {},
  ) {
    return this.waitingFor(waitingForFactory.triggerTargetSelection(data).build());
  }

  chooseXValue(data: Partial<ChooseXValueWaitingFor["data"]> = {}) {
    return this.waitingFor(waitingForFactory.chooseXValue(data).build());
  }

  assistPayment(data: Partial<AssistPaymentWaitingFor["data"]> = {}) {
    return this.waitingFor(waitingForFactory.assistPayment(data).build());
  }

  /**
   * Replace `waiting_for` exactly. Uses `afterBuild` instead of `params` so no
   * keys from the default Priority variant survive a deep-merge into the new
   * variant's `data`. Note this runs last, so it also wins over a
   * `waiting_for` passed directly to `.build()` — chain, don't mix.
   */
  waitingFor(waitingFor: WaitingFor) {
    return this.afterBuild((state) => {
      state.waiting_for = waitingFor;
    });
  }
}

export const gameStateFactory = GameStateFactory.define((): GameState => ({
  turn_number: 1,
  active_player: 0,
  phase: "PreCombatMain",
  players: buildPlayers([0, 1]),
  priority_player: 0,
  objects: {},
  next_object_id: 1,
  battlefield: [],
  stack: [],
  exile: [],
  rng_seed: 1,
  combat: null,
  waiting_for: buildPriorityWaitingFor(),
  has_pending_cast: false,
  lands_played_this_turn: 0,
  max_lands_per_turn: 1,
  priority_pass_count: 0,
  pending_replacement: null,
  layers_dirty: false,
  next_timestamp: 1,
  seat_order: [0, 1],
  format_config: buildFormatConfig(),
  eliminated_players: [],
}));

export const buildGameState = (overrides: Partial<GameState> = {}): GameState => {
  return { ...gameStateFactory.build(), ...overrides };
};

export const buildGameStateWithoutSeatOrder = (
  overrides: Partial<GameState> = {},
): GameState => {
  const { seat_order: _seatOrder, ...state } = buildGameState(overrides);
  return state;
};
