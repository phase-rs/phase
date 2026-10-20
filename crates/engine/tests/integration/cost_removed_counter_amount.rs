//! CR 602.2b + CR 601.2h + CR 608.2h: an activated ability whose cost removes
//! counters reads the number of counters actually removed while paying that
//! cost ("… counters removed this way", "for each counter removed", and, for a
//! remove-all cost, "that much"). The count is bound to the one activation that
//! paid it, on both the stack route and the mana route (CR 605.3b): the
//! activation's own payment owns the count while it is in flight, so two
//! activations of one source in flight at once (CR 601.2g + CR 605.3a) each
//! read their own.
//!
//! CR 707.10: a copy of such an ability copies all decisions made for it,
//! including the value of X, and isn't activated, so the copy reads the original's
//! removed-counter count (the official copier rulings, e.g. Abstruse Archaic:
//! "If the activated ability's cost contains a choice, such as a creature to
//! sacrifice or a number of counters to remove, the copy uses that same
//! information. You can't pay the cost again, even if you want to.").
//!
//! CR 107.1: "for each N <things>" counts complete groups of N (Sage of Hours;
//! CR 500.7 extra turns). A reference that cannot express the grouping fails
//! honestly instead of reading the bare count.
//!
//! CR 107.1c: an "any number" cost lets the player choose any number, including
//! zero.

use std::collections::VecDeque;
use std::sync::Arc;

use engine::game::casting::ActivationCostGuardSite;
use engine::game::effects::attach::attach_to;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction,
    CardSelectionMode, ChosenAttribute, Comparator, ControllerRef, CounterCostSelection,
    DamageChannel, Effect, EffectKind, ManaContribution, ManaProduction, PlayerFilter,
    QuantityExpr, QuantityRef, ReplacementDefinition, ReplacementMode, RoundingMode, TargetFilter,
    TargetRef, TriggerDefinition, TypedFilter, UnlessPayModifier, REMOVE_COUNTER_COST_ALL,
};
use engine::types::actions::GameAction;
use engine::types::counter::{parse_counter_type, CounterMatch, CounterType};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CounterCostChoice, DrainStatus, EnclosingPostReplacementFrame, GameState,
    ManaAbilityCostCursor, ManaChoice, ManaChoiceContext, ManaChoicePrompt,
    PausedCounterCostRemoval, PayCostKind, PendingCast, PendingCostMoveCompletion,
    PendingCostMoveResume, PendingCounterRemovalQueue, ReplacementChoiceKind, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::{ProposedEvent, ReplacementId};
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::ResolutionFrame;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const ESSENCE_BOTTLE: &str = "{3}, {T}: Put an elixir counter on this artifact.\n{T}, Remove all elixir counters from this artifact: You gain 2 life for each elixir counter removed this way.";
const MOLTEN_HYDRA: &str = "{1}{R}{R}: Put a +1/+1 counter on this creature.\n{T}, Remove all +1/+1 counters from this creature: It deals damage to any target equal to the number of +1/+1 counters removed this way.";
const RELIC_AMULET: &str = "Whenever you cast an instant, sorcery, or Wizard spell, put a charge counter on this artifact.\n{2}, {T}, Remove all charge counters from this artifact: It deals that much damage to target creature.";
const BLADEMANE_BAKU: &str = "Whenever you cast a Spirit or Arcane spell, you may put a ki counter on this creature.\n{1}, Remove X ki counters from this creature: For each counter removed, this creature gets +2/+0 until end of turn.";
const GEOMETRIC_NEXUS: &str = "Whenever a player casts an instant or sorcery spell, put a number of charge counters on this artifact equal to that spell's mana value.\n{6}, {T}, Remove all charge counters from this artifact: Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of charge counters removed this way.";
const VISH_KAL: &str = "Flying, lifelink\nSacrifice a creature: Put X +1/+1 counters on Vish Kal, where X is the sacrificed creature's power.\nRemove all +1/+1 counters from Vish Kal: Target creature gets -1/-1 until end of turn for each +1/+1 counter removed this way.";
const TORTURE_CHAMBER: &str = "At the beginning of your upkeep, put a pain counter on this artifact.\nAt the beginning of your end step, this artifact deals damage to you equal to the number of pain counters on it.\n{1}, {T}, Remove all pain counters from this artifact: It deals damage to target creature equal to the number of pain counters removed this way.";
const JAR_OF_EYEBALLS: &str = "Whenever a creature you control dies, put two eyeball counters on this artifact.\n{3}, {T}, Remove all eyeball counters from this artifact: Look at the top X cards of your library, where X is the number of eyeball counters removed this way. Put one of them into your hand and the rest on the bottom of your library in any order.";
const OOZE_FLUX: &str = "{1}{G}, Remove one or more +1/+1 counters from among creatures you control: Create an X/X green Ooze creature token, where X is the number of +1/+1 counters removed this way.";
const CATTI_BRIE: &str = "Reach, first strike\nWhenever Catti-brie attacks, put a +1/+1 counter on it for each Equipment attached to it.\n{1}, Remove all +1/+1 counters from Catti-brie: It deals X damage to target attacking or blocking creature an opponent controls, where X is the number of counters removed this way.";
const SAGE_OF_HOURS: &str = "Heroic — Whenever you cast a spell that targets this creature, put a +1/+1 counter on it.\nRemove all +1/+1 counters from this creature: For each five counters removed this way, take an extra turn after this one.";
const SAPRAZZAN_COVE: &str = "This land enters tapped.\n{T}: Put a storage counter on this land.\n{T}, Remove any number of storage counters from this land: Add {U} for each storage counter removed this way.";
const BLACK_MANA_BATTERY: &str = "{2}, {T}: Put a charge counter on this artifact.\n{T}, Remove any number of charge counters from this artifact: Add {B}, then add an additional {B} for each charge counter removed this way.";
const RASPUTIN: &str = "When Rasputin enters, put a dream counter on it for each opponent you have. Each opponent creates a 1/1 red Goblin creature token.\n{T}, Remove one or more dream counters from Rasputin: Add that much {C}.\n{T}, Remove a dream counter from Rasputin: Create a 2/2 white Knight creature token with protection from red.";
const GEISTFLAME_RESERVOIR: &str = "Whenever you cast an instant or sorcery spell, put a charge counter on this artifact.\n{1}{R}, {T}, Remove any number of charge counters from this artifact: It deals that much damage to any target.\n{1}{R}, {T}: Exile the top card of your library. You may play that card this turn.";
const ANT_MAN: &str = "Whenever you draw a card, put a +1/+1 counter on The Astonishing Ant-Man.\n{2}{G}, {T}, Remove any number of +1/+1 counters from The Astonishing Ant-Man: Create that many 1/1 green Insect creature tokens.";
const RINGS_OF_BRIGHTHEARTH: &str = "Whenever you activate an ability, if it isn't a mana ability, you may pay {2}. If you do, copy that ability. You may choose new targets for the copy.";
const ILLUSIONISTS_BRACERS: &str = "Whenever an ability of equipped creature is activated, if it isn't a mana ability, copy that ability. You may choose new targets for the copy.\nEquip {3}";
const OPALESCENCE: &str = "Each other non-Aura enchantment is a creature in addition to its other types and has base power and base toughness each equal to its mana value.";
const CRYPTOLITH_RITE: &str = "Creatures you control have \"{T}: Add one mana of any color.\"";
const EXPERIMENT_KRAJ: &str = "Experiment Kraj has all activated abilities of each other creature with a +1/+1 counter on it.\n{T}: Put a +1/+1 counter on target creature.";
const MARCH_OF_THE_MACHINES: &str = "Each noncreature artifact is an artifact creature with power and toughness each equal to its mana value. (Equipment that's a creature can't equip a creature.)";
const PENTAD_PRISM: &str = "Sunburst (This artifact enters with a charge counter on it for each color of mana spent to cast it.)\nRemove a charge counter from this artifact: Add one mana of any color.";
const MAGOSI: &str = "This land enters tapped.\n{T}: Add {U}.\n{U}, {T}: Put an eon counter on this land. Skip your next turn.\n{T}, Remove an eon counter from this land and return it to its owner's hand: Take an extra turn after this one.";
const EMBER_SHOT: &str = "Ember Shot deals 3 damage to any target.\nDraw a card.";
/// C5a: the effect-path control — counters removed by an EFFECT, not a cost.
const EFFECT_PATH_CONTROL: &str = "{T}: Remove all charge counters from this artifact. You gain 1 life for each charge counter removed this way.";
/// C7d: a reader of X itself (not of the counters removed).
const X_READER: &str =
    "{1}, Remove X ki counters from this creature: This creature gets +X/+0 until end of turn.";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &["Forest", "Island", "Swamp", "Mountain", "Plains", "Forest"],
    );
    scenario
}

fn counter(name: &str) -> CounterType {
    parse_counter_type(name)
}

fn add_mana(scenario: &mut GameScenario, mana: &[ManaType]) {
    scenario.with_mana_pool(
        P0,
        mana.iter()
            .map(|color| ManaUnit::new(*color, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn colorless(n: usize) -> Vec<ManaType> {
    vec![ManaType::Colorless; n]
}

/// A P1 creature that survives the damage these rows deal.
fn add_victim(scenario: &mut GameScenario, name: &str, power: i32, toughness: i32) -> ObjectId {
    scenario.add_creature(P1, name, power, toughness).id()
}

fn counters_on(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(kind)
        .copied()
        .unwrap_or(0)
}

fn damage(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id]
        .power
        .expect("a creature has power")
}

fn toughness(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id]
        .toughness
        .expect("a creature has toughness")
}

fn pool(runner: &GameRunner, color: ManaType) -> usize {
    runner.state().players[0].mana_pool.count_color(color)
}

/// CR 602.2b: the count lives only with the payment that removed the counters,
/// so once that activation is placed or has produced its mana no payment state
/// is left to hold it.
fn payment_ownership_is_clear(runner: &GameRunner) -> bool {
    let state = runner.state();
    state.pending_cast.is_none()
        && state.pending_cost_move_resume.is_none()
        && state.pending_replacement.is_none()
        && !matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. })
}

/// The removed-counter count the paused activation's own payment holds.
fn pending_activation_count(runner: &GameRunner) -> Option<u32> {
    runner
        .state()
        .pending_cast
        .as_ref()
        .expect("an activation is paused for its mana payment")
        .ability
        .context
        .cost_removed_counter_amount
}

/// The removed-counter count bound to the top stack entry's activated ability.
fn top_stack_count(runner: &GameRunner) -> Option<u32> {
    match &runner
        .state()
        .stack
        .last()
        .expect("an ability waits on the stack")
        .kind
    {
        StackEntryKind::ActivatedAbility { ability, .. } => {
            ability.context.cost_removed_counter_amount
        }
        other => panic!("the top of the stack is an activated ability: {other:?}"),
    }
}

fn is_mana_payment(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::ManaPayment { .. })
}

fn cost_removes_counters(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::RemoveCounter { .. } => true,
        AbilityCost::Composite { costs } => costs.iter().any(cost_removes_counters),
        _ => false,
    }
}

/// Index of the `nth` activated ability of `id` whose cost removes counters.
fn counter_cost_ability(runner: &GameRunner, id: ObjectId, nth: usize) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, def)| def.cost.as_ref().is_some_and(cost_removes_counters))
        .map(|(index, _)| index)
        .nth(nth)
        .expect("the card has a counter-cost activated ability")
}

/// Index of the activated mana ability of `id` whose cost removes no counters.
fn mana_ability_without_counter_cost(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|def| {
            matches!(&*def.effect, Effect::Mana { .. })
                && !def.cost.as_ref().is_some_and(cost_removes_counters)
        })
        .expect("the object has a mana ability without a counter cost")
}

fn battlefield_named(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects.get(id).is_some_and(|obj| obj.name == name))
        .collect()
}

/// Answers for the prompts a row may reach.
#[derive(Default)]
struct Answers {
    x: Option<u32>,
    pay_amount: Option<u32>,
    targets: VecDeque<TargetRef>,
    distribution: Option<Vec<CounterCostChoice>>,
    /// Place the trigger from this source last (on top) at an ordering prompt.
    copier_last: Option<ObjectId>,
    /// Cards a `DigChoice` offered, recorded for the Jar of Eyeballs row.
    dig_offered: Option<usize>,
    /// The colour chosen at a `ChooseManaColor` prompt.
    mana_color: Option<ManaType>,
    /// The option picked at each `ReplacementChoice` prompt, in order.
    replacement_choices: VecDeque<usize>,
    /// The permanents returned at a `PayCost { kind: ReturnToHand }` prompt.
    return_choice: Vec<ObjectId>,
}

impl Answers {
    fn x(value: u32) -> Self {
        Self {
            x: Some(value),
            ..Self::default()
        }
    }

    fn pay(amount: u32) -> Self {
        Self {
            pay_amount: Some(amount),
            ..Self::default()
        }
    }

    fn target(mut self, target: TargetRef) -> Self {
        self.targets.push_back(target);
        self
    }

    fn object(self, id: ObjectId) -> Self {
        self.target(TargetRef::Object(id))
    }
}

/// Answers every prompt from `answers` until `stop` holds.
fn drive(runner: &mut GameRunner, stop: impl Fn(&GameState) -> bool, answers: &mut Answers) {
    for _ in 0..96 {
        if stop(runner.state()) {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX {
                value: answers.x.expect("an X answer for this row"),
            },
            WaitingFor::PayAmountChoice { .. } => GameAction::SubmitPayAmount {
                amount: answers
                    .pay_amount
                    .expect("a pay-amount answer for this row"),
            },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(
                    answers
                        .targets
                        .pop_front()
                        .expect("a declared target for this prompt"),
                ),
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::CopyRetarget { .. } => GameAction::KeepAllCopyTargets,
            WaitingFor::OrderTriggers { triggers, .. } => {
                let mut order: Vec<usize> = (0..triggers.len()).collect();
                order.sort_by_key(|&index| Some(triggers[index].source_id) == answers.copier_last);
                GameAction::OrderTriggers { order }
            }
            WaitingFor::DigChoice {
                cards, keep_count, ..
            } => {
                answers.dig_offered = Some(cards.len());
                GameAction::SelectCards {
                    cards: cards.iter().copied().take(*keep_count).collect(),
                }
            }
            WaitingFor::ChooseManaColor { .. } => GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(
                    answers.mana_color.expect("a mana colour for this row"),
                ),
                count: 1,
            },
            WaitingFor::PayCost {
                kind: PayCostKind::RemoveCounter { .. },
                ..
            } => GameAction::ChooseRemoveCounterCostDistribution {
                distribution: answers
                    .distribution
                    .clone()
                    .expect("a counter distribution for this row"),
            },
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                ..
            } => GameAction::SelectCards {
                cards: answers.return_choice.clone(),
            },
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement {
                index: answers
                    .replacement_choices
                    .pop_front()
                    .expect("a replacement choice for this row"),
            },
            other => panic!("unexpected prompt: {}", other.variant_name()),
        };
        runner
            .act(action)
            .unwrap_or_else(|err| panic!("the engine accepts the action: {err:?}"));
    }
    panic!(
        "stop condition never reached; waiting for {:?}",
        runner.state().waiting_for
    );
}

fn priority_with_stack(len: usize) -> impl Fn(&GameState) -> bool {
    move |state: &GameState| {
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.len() == len
    }
}

fn priority_with_empty_stack(state: &GameState) -> bool {
    priority_with_stack(0)(state)
}

fn priority_with_no_trigger_waiting(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. })
        && !state
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
}

fn activate(runner: &mut GameRunner, source_id: ObjectId, ability_index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id,
            ability_index,
        })
        .unwrap_or_else(|err| panic!("the ability activates: {err:?}"));
}

/// Activate the `nth` counter-cost ability of `source`, answer every prompt and
/// resolve the stack.
fn activate_and_resolve(
    runner: &mut GameRunner,
    source: ObjectId,
    nth: usize,
    mut answers: Answers,
) -> Answers {
    let index = counter_cost_ability(runner, source, nth);
    activate(runner, source, index);
    drive(runner, priority_with_empty_stack, &mut answers);
    answers
}

// ---------------------------------------------------------------------------
// C1 — stack route, `PreviousEffectAmount`
// ---------------------------------------------------------------------------

/// C1a + C4c: Essence Bottle (remove all, direct push) gains 2 life per elixir
/// counter removed; the count is bound to its own stack entry at placement.
#[test]
fn essence_bottle_gains_two_life_per_counter_removed_by_its_cost() {
    let mut scenario = main_phase();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    let mut answers = Answers::default();
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert_eq!(
        counters_on(&runner, bottle, &counter("elixir")),
        0,
        "reach: the cost removed every elixir counter"
    );
    assert_eq!(
        top_stack_count(&runner),
        Some(3),
        "the stack entry carries its own payment's count"
    );
    assert!(
        payment_ownership_is_clear(&runner),
        "C4c: nothing else holds it"
    );

    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_eq!(runner.life(P0) - life_before, 6);
    assert!(payment_ownership_is_clear(&runner));
}

/// C1b: Molten Hydra deals damage to any target equal to the +1/+1 counters its
/// cost removed.
#[test]
fn molten_hydra_deals_damage_equal_to_counters_removed_by_its_cost() {
    let mut scenario = main_phase();
    let hydra = scenario
        .add_creature_from_oracle(P0, "Molten Hydra", 1, 1, MOLTEN_HYDRA)
        .id();
    scenario.with_counter(hydra, CounterType::Plus1Plus1, 3);
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    activate_and_resolve(
        &mut runner,
        hydra,
        0,
        Answers::default().target(TargetRef::Player(P1)),
    );

    assert_eq!(counters_on(&runner, hydra, &CounterType::Plus1Plus1), 0);
    assert_eq!(life_before - runner.life(P1), 3);
}

fn nexus_board() -> (GameRunner, ObjectId) {
    let mut scenario = main_phase();
    let nexus = scenario
        .add_artifact_from_oracle(P0, "Geometric Nexus", GEOMETRIC_NEXUS)
        .id();
    scenario.with_counter(nexus, counter("charge"), 4);
    add_mana(&mut scenario, &colorless(6));
    (scenario.build(), nexus)
}

fn assert_fractal_with_four_counters(runner: &GameRunner, nexus: ObjectId) {
    assert_eq!(counters_on(runner, nexus, &counter("charge")), 0);
    let fractals = battlefield_named(runner, "Fractal");
    assert_eq!(fractals.len(), 1, "the Fractal survives on the battlefield");
    assert_eq!(
        counters_on(runner, fractals[0], &CounterType::Plus1Plus1),
        4
    );
}

/// C1c: Geometric Nexus in the `Token.enter_with_counters` shape — the Fractal
/// enters with 4 +1/+1 counters.
#[test]
fn geometric_nexus_fractal_enters_with_the_counters_removed() {
    let (mut runner, nexus) = nexus_board();
    let index = counter_cost_ability(&runner, nexus, 0);
    let def = &runner.state().objects[&nexus].abilities[index];
    assert!(
        matches!(&*def.effect, Effect::Token { enter_with_counters, .. } if !enter_with_counters.is_empty()),
        "structural guard: the parse carries the count on the token: {:?}",
        def.effect
    );

    activate_and_resolve(&mut runner, nexus, 0, Answers::default());

    assert_fractal_with_four_counters(&runner, nexus);
}

/// C1d: Geometric Nexus in the `Token` → `PutCounter { LastCreated }` shape —
/// the seeded amount survives the head and the parent→child hand-off.
#[test]
fn geometric_nexus_fractal_gets_the_counters_removed_through_a_put_counter_sub() {
    let (mut runner, nexus) = nexus_board();
    let index = counter_cost_ability(&runner, nexus, 0);
    let mut def = runner.state().objects[&nexus].abilities[index].clone();
    let Effect::Token {
        enter_with_counters,
        ..
    } = &mut *def.effect
    else {
        panic!("the head is the token creation: {:?}", def.effect);
    };
    let (counter_type, count) = enter_with_counters
        .pop()
        .expect("the parse carries the count on the token");
    assert!(enter_with_counters.is_empty());
    assert!(def.sub_ability.is_none(), "no sub before the rewrite");
    def.sub_ability = Some(Box::new(AbilityDefinition::new(
        def.kind,
        Effect::PutCounter {
            counter_type,
            count,
            target: TargetFilter::LastCreated,
        },
    )));
    let obj = runner.state_mut().objects.get_mut(&nexus).unwrap();
    Arc::make_mut(&mut obj.abilities)[index] = def.clone();
    Arc::make_mut(&mut obj.base_abilities)[index] = def;
    let rewritten = &runner.state().objects[&nexus].abilities[index];
    assert!(
        matches!(&*rewritten.effect, Effect::Token { enter_with_counters, .. } if enter_with_counters.is_empty())
            && rewritten.sub_ability.as_deref().is_some_and(|sub| matches!(
                &*sub.effect,
                Effect::PutCounter {
                    target: TargetFilter::LastCreated,
                    ..
                }
            )),
        "structural guard: the rewrite moved the count into a PutCounter sub"
    );

    activate_and_resolve(&mut runner, nexus, 0, Answers::default());

    assert_fractal_with_four_counters(&runner, nexus);
}

/// C1e: Blademane Baku, X = 2 with 3 ki counters — +2/+0 for each of the two
/// counters removed.
#[test]
fn blademane_baku_gets_two_power_per_counter_removed() {
    let mut scenario = main_phase();
    let baku = scenario
        .add_creature_from_oracle(P0, "Blademane Baku", 1, 1, BLADEMANE_BAKU)
        .id();
    scenario.with_counter(baku, counter("ki"), 3);
    add_mana(&mut scenario, &colorless(1));
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, baku, 0, Answers::x(2));

    assert_eq!(counters_on(&runner, baku, &counter("ki")), 1);
    assert_eq!(power(&runner, baku), 5);
}

// ---------------------------------------------------------------------------
// C2 — remove-all costs (incl. "that much")
// ---------------------------------------------------------------------------

/// C2a: Relic Amulet "deals that much damage" after removing all 3 charge counters.
#[test]
fn relic_amulet_deals_that_much_damage() {
    let mut scenario = main_phase();
    let amulet = scenario
        .add_artifact_from_oracle(P0, "Relic Amulet", RELIC_AMULET)
        .id();
    scenario.with_counter(amulet, counter("charge"), 3);
    let victim = add_victim(&mut scenario, "Victim", 1, 5);
    add_mana(&mut scenario, &colorless(2));
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, amulet, 0, Answers::default().object(victim));

    assert_eq!(counters_on(&runner, amulet, &counter("charge")), 0);
    assert_eq!(damage(&runner, victim), 3);
}

/// C2b: Vish Kal removes 3 +1/+1 counters — the target gets -3/-3.
#[test]
fn vish_kal_shrinks_the_target_per_counter_removed() {
    let mut scenario = main_phase();
    let vish = scenario
        .add_creature_from_oracle(P0, "Vish Kal, Blood Arbiter", 5, 5, VISH_KAL)
        .id();
    scenario.with_counter(vish, CounterType::Plus1Plus1, 3);
    let target = add_victim(&mut scenario, "Target", 5, 5);
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, vish, 0, Answers::default().object(target));

    assert_eq!(counters_on(&runner, vish, &CounterType::Plus1Plus1), 0);
    assert_eq!((power(&runner, target), toughness(&runner, target)), (2, 2));
}

/// C2c: Torture Chamber deals damage equal to the 2 pain counters removed.
#[test]
fn torture_chamber_deals_damage_equal_to_pain_counters_removed() {
    let mut scenario = main_phase();
    let chamber = scenario
        .add_artifact_from_oracle(P0, "Torture Chamber", TORTURE_CHAMBER)
        .id();
    scenario.with_counter(chamber, counter("pain"), 2);
    let victim = add_victim(&mut scenario, "Victim", 1, 5);
    add_mana(&mut scenario, &colorless(1));
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, chamber, 0, Answers::default().object(victim));

    assert_eq!(counters_on(&runner, chamber, &counter("pain")), 0);
    assert_eq!(damage(&runner, victim), 2);
}

/// C2d: Jar of Eyeballs looks at exactly the 3 cards its cost's counters name.
#[test]
fn jar_of_eyeballs_looks_at_the_number_of_counters_removed() {
    let mut scenario = main_phase();
    let jar = scenario
        .add_artifact_from_oracle(P0, "Jar of Eyeballs", JAR_OF_EYEBALLS)
        .id();
    scenario.with_counter(jar, counter("eyeball"), 3);
    add_mana(&mut scenario, &colorless(3));
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    let answers = activate_and_resolve(&mut runner, jar, 0, Answers::default());

    assert_eq!(counters_on(&runner, jar, &counter("eyeball")), 0);
    assert_eq!(
        answers.dig_offered,
        Some(3),
        "the dig offers exactly 3 cards"
    );
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
}

/// C2e: Ooze Flux removes 2 + 1 +1/+1 counters from among two creatures and
/// creates one 3/3 Ooze.
#[test]
fn ooze_flux_creates_an_ooze_sized_by_counters_removed_from_among() {
    let mut scenario = main_phase();
    let flux = scenario
        .add_enchantment_from_oracle(P0, "Ooze Flux", OOZE_FLUX)
        .id();
    let first = scenario.add_creature(P0, "First", 1, 1).id();
    let second = scenario.add_creature(P0, "Second", 1, 1).id();
    scenario.with_counter(first, CounterType::Plus1Plus1, 2);
    scenario.with_counter(second, CounterType::Plus1Plus1, 1);
    add_mana(&mut scenario, &[ManaType::Colorless, ManaType::Green]);
    let mut runner = scenario.build();

    let answers = Answers {
        x: Some(3),
        pay_amount: Some(3),
        distribution: Some(vec![
            CounterCostChoice {
                object_id: first,
                counter_type: CounterType::Plus1Plus1,
                count: 2,
            },
            CounterCostChoice {
                object_id: second,
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            },
        ]),
        ..Answers::default()
    };
    activate_and_resolve(&mut runner, flux, 0, answers);

    assert_eq!(counters_on(&runner, first, &CounterType::Plus1Plus1), 0);
    assert_eq!(counters_on(&runner, second, &CounterType::Plus1Plus1), 0);
    let oozes = battlefield_named(&runner, "Ooze");
    assert_eq!(oozes.len(), 1, "one Ooze on the battlefield");
    assert_eq!(
        (power(&runner, oozes[0]), toughness(&runner, oozes[0])),
        (3, 3)
    );
}

/// C2f: Catti-brie deals X damage (X = counters removed) to a blocking creature
/// an opponent controls.
#[test]
fn catti_brie_deals_damage_equal_to_counters_removed() {
    let mut scenario = main_phase();
    let catti = scenario
        .add_creature_from_oracle(P0, "Catti-brie of Mithral Hall", 2, 2, CATTI_BRIE)
        .id();
    scenario.with_counter(catti, CounterType::Plus1Plus1, 3);
    let attacker = scenario.add_creature(P0, "Attacker", 1, 1).id();
    let blocker = add_victim(&mut scenario, "Blocker", 1, 5);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P1))])
        .expect("the attack is declared");
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes in the declare attackers step");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("the block is declared");
    let _ = runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));

    activate_and_resolve(&mut runner, catti, 0, Answers::default().object(blocker));

    assert_eq!(counters_on(&runner, catti, &CounterType::Plus1Plus1), 0);
    assert_eq!(damage(&runner, blocker), 3);
}

// ---------------------------------------------------------------------------
// C3 — mana route (CR 605.3b)
// ---------------------------------------------------------------------------

/// C3a + C4c: Saprazzan Cove removes 2 of 3 storage counters and adds {U}{U}.
#[test]
fn saprazzan_cove_adds_blue_per_storage_counter_removed() {
    let mut scenario = main_phase();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, cove, 0, Answers::pay(2));

    assert_eq!(counters_on(&runner, cove, &counter("storage")), 1);
    assert_eq!(pool(&runner, ManaType::Blue), 2);
    assert!(
        payment_ownership_is_clear(&runner),
        "C4c: no payment state outlives the mana ability"
    );
}

/// C3b: Black Mana Battery removes 2 charge counters: {B}, then {B}{B} more from
/// the sub-chain.
#[test]
fn black_mana_battery_adds_an_additional_black_per_counter_removed() {
    let mut scenario = main_phase();
    let battery = scenario
        .add_artifact_from_oracle(P0, "Black Mana Battery", BLACK_MANA_BATTERY)
        .id();
    scenario.with_counter(battery, counter("charge"), 2);
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, battery, 0, Answers::pay(2));

    assert_eq!(counters_on(&runner, battery, &counter("charge")), 0);
    assert_eq!(pool(&runner, ManaType::Black), 3);
}

/// The C3c board: Ooze Flux, a 4/4 creature under Opalescence with the mana
/// ability Cryptolith Rite grants, is the only untapped mana source (the animated
/// Rite, First and Second are tapped). Its counter cost removes 3 +1/+1 counters
/// from among First (2) and Second (1).
fn ooze_flux_with_granted_mana(pool: &[ManaType]) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    ooze_flux_with_granted_mana_and(pool, |_| {})
}

/// The C3c board, with `extra` seating more permanents before it is built.
fn ooze_flux_with_granted_mana_and(
    pool: &[ManaType],
    extra: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    extra(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Opalescence", OPALESCENCE);
    let rite = scenario
        .add_enchantment_from_oracle(P0, "Cryptolith Rite", CRYPTOLITH_RITE)
        .id();
    // Ooze Flux's printed mana cost {3}{G}: Opalescence makes it a 4/4.
    let flux = scenario
        .add_enchantment_from_oracle(P0, "Ooze Flux", OOZE_FLUX)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        })
        .id();
    let first = scenario.add_creature(P0, "First", 1, 1).id();
    let second = scenario.add_creature(P0, "Second", 1, 1).id();
    scenario.with_counter(first, CounterType::Plus1Plus1, 2);
    scenario.with_counter(second, CounterType::Plus1Plus1, 1);
    add_mana(&mut scenario, pool);
    let mut runner = scenario.build();
    engine::game::layers::evaluate_layers(runner.state_mut());
    for id in [rite, first, second] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    assert!(
        runner.state().objects[&flux]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature),
        "reach: Opalescence makes Ooze Flux a creature"
    );
    (runner, flux, first, second)
}

fn ooze_flux_answers(first: ObjectId, second: ObjectId) -> Answers {
    Answers {
        x: Some(3),
        pay_amount: Some(3),
        distribution: Some(vec![
            CounterCostChoice {
                object_id: first,
                counter_type: CounterType::Plus1Plus1,
                count: 2,
            },
            CounterCostChoice {
                object_id: second,
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            },
        ]),
        mana_color: Some(ManaType::Green),
        ..Answers::default()
    }
}

fn assert_one_three_three_ooze(runner: &GameRunner, first: ObjectId, second: ObjectId) {
    assert_eq!(counters_on(runner, first, &CounterType::Plus1Plus1), 0);
    assert_eq!(counters_on(runner, second, &CounterType::Plus1Plus1), 0);
    let oozes = battlefield_named(runner, "Ooze");
    assert_eq!(oozes.len(), 1, "the Ooze survives: it is not 0/0");
    assert_eq!(
        (power(runner, oozes[0]), toughness(runner, oozes[0])),
        (3, 3),
        "Ooze Flux reads its own 3 counters removed"
    );
    assert!(payment_ownership_is_clear(runner));
}

/// C3c: Ooze Flux pays its interactive counter cost (3 from among), then, with
/// its {1}{G} waiting for mana, activates the mana ability Cryptolith Rite grants
/// it (CR 601.2g + CR 605.3a). That mana ability removed no counters, so it must
/// neither read nor strip Ooze Flux's pending count: the Ooze is 3/3.
#[test]
fn a_mana_ability_activated_while_paying_does_not_take_the_pending_count() {
    // An empty pool: Ooze Flux's one mana can't pay {1}{G}, so the payment
    // waits at `ManaPayment` instead of auto-paying.
    let (mut runner, flux, first, second) = ooze_flux_with_granted_mana(&[]);
    let mut answers = ooze_flux_answers(first, second);

    let index = counter_cost_ability(&runner, flux, 0);
    activate(&mut runner, flux, index);
    drive(&mut runner, is_mana_payment, &mut answers);
    assert_eq!(
        pending_activation_count(&runner),
        Some(3),
        "the paused activation owns its 3 counters removed"
    );

    let granted_mana = mana_ability_without_counter_cost(&runner, flux);
    activate(&mut runner, flux, granted_mana);
    drive(&mut runner, is_mana_payment, &mut answers);
    assert!(
        runner.state().objects[&flux].tapped,
        "reach: Ooze Flux tapped for mana with the granted ability"
    );
    assert_eq!(
        pool(&runner, ManaType::Green),
        1,
        "reach: the granted mana ability produced its mana"
    );
    assert_eq!(
        pending_activation_count(&runner),
        Some(3),
        "the mana ability neither read nor stripped the pending count"
    );

    // The rest of {1}{G}.
    let _ = runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    );
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "reach: the pool paid {{1}}{{G}}"
    );
    assert_eq!(top_stack_count(&runner), Some(3));

    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_one_three_three_ooze(&runner, first, second);
}

/// C3c (auto-tap): with {G} in the pool, the payment auto-taps Ooze Flux for the
/// {1} with its granted mana ability (CR 605.3a). That mana ability removed no
/// counters either: the stack entry still carries Ooze Flux's own 3.
#[test]
fn an_auto_tapped_mana_ability_does_not_take_the_pending_count() {
    let (mut runner, flux, first, second) = ooze_flux_with_granted_mana(&[ManaType::Green]);
    let mut answers = ooze_flux_answers(first, second);

    let index = counter_cost_ability(&runner, flux, 0);
    activate(&mut runner, flux, index);
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert!(
        runner.state().objects[&flux].tapped,
        "reach: Ooze Flux tapped for mana with the granted ability"
    );
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "reach: the pool and the granted mana paid {{1}}{{G}}"
    );
    assert_eq!(top_stack_count(&runner), Some(3));

    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_one_three_three_ooze(&runner, first, second);
}

/// R3: a counter cost nested at any `Composite` depth is still this mana
/// ability's own counter leg — Saprazzan Cove's cost rewritten to
/// `Composite[Composite[{T}, Composite[<remove any number of storage counters>]]]`
/// removes 2 storage counters and adds {U}{U}.
#[test]
fn nested_composite_counter_cost_mana_ability_adds_per_counter_removed() {
    let mut scenario = main_phase();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let mut runner = scenario.build();
    let index = counter_cost_ability(&runner, cove, 0);
    let mut def = runner.state().objects[&cove].abilities[index].clone();
    let Some(AbilityCost::Composite { costs }) = def.cost.take() else {
        panic!(
            "structural guard: the parsed cost is composite: {:?}",
            def.cost
        );
    };
    let [tap @ AbilityCost::Tap, remove @ AbilityCost::RemoveCounter { .. }] = costs.as_slice()
    else {
        panic!("structural guard: the parsed cost is {{T}} + a counter removal: {costs:?}");
    };
    def.cost = Some(AbilityCost::Composite {
        costs: vec![AbilityCost::Composite {
            costs: vec![
                tap.clone(),
                AbilityCost::Composite {
                    costs: vec![remove.clone()],
                },
            ],
        }],
    });
    let obj = runner.state_mut().objects.get_mut(&cove).unwrap();
    Arc::make_mut(&mut obj.abilities)[index] = def.clone();
    Arc::make_mut(&mut obj.base_abilities)[index] = def;

    activate(&mut runner, cove, index);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice { .. }
        ),
        "reach: the nested counter leg was found and prompted: {:?}",
        runner.state().waiting_for
    );
    drive(&mut runner, priority_with_empty_stack, &mut Answers::pay(2));

    assert_eq!(counters_on(&runner, cove, &counter("storage")), 1);
    assert_eq!(pool(&runner, ManaType::Blue), 2);
    assert!(payment_ownership_is_clear(&runner));
}

/// B1-A: Pentad Prism's fixed-count self counter cost is paid by the mana cost
/// cursor's self-contained arm, and its count belongs to that mana activation:
/// it is on the pending mana ability at the colour prompt (CR 605.3b), and
/// "Remove a charge counter from this artifact: Add one mana of any color."
/// removes one counter and adds one mana.
#[test]
fn pentad_prism_fixed_counter_cost_is_owned_by_its_mana_activation() {
    let mut scenario = main_phase();
    let prism = scenario
        .add_artifact_from_oracle(P0, "Pentad Prism", PENTAD_PRISM)
        .id();
    scenario.with_counter(prism, counter("charge"), 2);
    let mut runner = scenario.build();

    let index = counter_cost_ability(&runner, prism, 0);
    activate(&mut runner, prism, index);
    let WaitingFor::ChooseManaColor {
        context: ManaChoiceContext::ManaAbility(pending),
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "reach: the mana ability asks for its colour: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        pending.cost_removed_counters,
        Some(1),
        "the mana activation owns the one counter its cost removed"
    );
    assert_eq!(
        counters_on(&runner, prism, &counter("charge")),
        1,
        "reach: the cost removed one charge counter"
    );

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        })
        .expect("the colour is chosen");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(pool(&runner, ManaType::Red), 1);
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    assert_eq!(counters_on(&runner, prism, &counter("charge")), 1);
    assert!(payment_ownership_is_clear(&runner));
}

/// R1: two activations of one source in flight at once (CR 601.2g + CR 605.3a).
/// Under Opalescence and March of the Machines, Ooze Flux and Black Mana Battery
/// are creatures with a +1/+1 counter, so Experiment Kraj has both of their
/// counter-cost abilities. Kraj activates Ooze Flux's (3 counters from among
/// First and Second), then, while paying its {1}{G}, activates Battery's mana
/// ability (CR 201.5b + CR 700.7: "this artifact" is Kraj, which loses 2 charge counters).
/// Each reads its own count: {B}{B}{B}, then one 3/3 Ooze.
#[test]
fn kraj_nested_same_source_counter_cost_mana_payment_keeps_each_count() {
    let mut scenario = main_phase();
    scenario.add_enchantment_from_oracle(P0, "Opalescence", OPALESCENCE);
    scenario.add_enchantment_from_oracle(P0, "March of the Machines", MARCH_OF_THE_MACHINES);
    let kraj = scenario
        .add_creature_from_oracle(P0, "Experiment Kraj", 4, 6, EXPERIMENT_KRAJ)
        .id();
    // Printed mana costs {3}{G} and {4}: each becomes a 4/4 creature.
    let flux = scenario
        .add_enchantment_from_oracle(P0, "Ooze Flux", OOZE_FLUX)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        })
        .id();
    let battery = scenario
        .add_artifact_from_oracle(P0, "Black Mana Battery", BLACK_MANA_BATTERY)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 4,
        })
        .id();
    let first = scenario.add_creature(P0, "First", 1, 1).id();
    let second = scenario.add_creature(P0, "Second", 1, 1).id();
    scenario.with_counter(flux, CounterType::Plus1Plus1, 1);
    scenario.with_counter(battery, CounterType::Plus1Plus1, 1);
    scenario.with_counter(first, CounterType::Plus1Plus1, 2);
    scenario.with_counter(second, CounterType::Plus1Plus1, 1);
    scenario.with_counter(kraj, counter("charge"), 2);
    add_mana(&mut scenario, &[ManaType::Green]);
    let mut runner = scenario.build();
    engine::game::layers::evaluate_layers(runner.state_mut());

    let flux_ability = counter_cost_ability(&runner, kraj, 0);
    let battery_ability = counter_cost_ability(&runner, kraj, 1);
    let kraj_abilities = &runner.state().objects[&kraj].abilities;
    assert!(
        matches!(&*kraj_abilities[flux_ability].effect, Effect::Token { .. })
            && matches!(
                &*kraj_abilities[battery_ability].effect,
                Effect::Mana { .. }
            ),
        "reach: Kraj has Ooze Flux's and Battery's counter-cost abilities"
    );

    let mut outer = Answers {
        x: Some(3),
        pay_amount: Some(3),
        distribution: Some(vec![
            CounterCostChoice {
                object_id: first,
                counter_type: CounterType::Plus1Plus1,
                count: 2,
            },
            CounterCostChoice {
                object_id: second,
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            },
        ]),
        ..Answers::default()
    };
    activate(&mut runner, kraj, flux_ability);
    drive(&mut runner, is_mana_payment, &mut outer);
    assert_eq!(
        pending_activation_count(&runner),
        Some(3),
        "the outer activation owns its 3 counters removed"
    );

    activate(&mut runner, kraj, battery_ability);
    drive(&mut runner, is_mana_payment, &mut Answers::pay(2));
    assert_eq!(
        counters_on(&runner, kraj, &counter("charge")),
        0,
        "reach: the inner mana ability removed Kraj's 2 charge counters"
    );
    assert_eq!(
        pool(&runner, ManaType::Black),
        3,
        "the inner mana ability reads only its own 2: {{B}} + {{B}}{{B}}"
    );
    assert_eq!(pool(&runner, ManaType::Green), 1);
    assert_eq!(
        pending_activation_count(&runner),
        Some(3),
        "the inner mana ability left the outer count in place"
    );

    drive(&mut runner, priority_with_empty_stack, &mut outer);
    let oozes = battlefield_named(&runner, "Ooze");
    assert_eq!(oozes.len(), 1, "exactly one Ooze on the battlefield");
    assert_eq!(
        (power(&runner, oozes[0]), toughness(&runner, oozes[0])),
        (3, 3),
        "the outer activation reads its own 3 counters removed"
    );
    assert_eq!(
        pool(&runner, ManaType::Black),
        2,
        "{{1}}{{G}} took the {{G}} and one {{B}}"
    );
    assert_eq!(counters_on(&runner, kraj, &counter("charge")), 0);
    assert_eq!(counters_on(&runner, first, &CounterType::Plus1Plus1), 0);
    assert_eq!(counters_on(&runner, second, &CounterType::Plus1Plus1), 0);
    assert!(payment_ownership_is_clear(&runner));
}

// ---------------------------------------------------------------------------
// R2 — a counter-cost removal deferred by a replacement choice (CR 616.1)
// ---------------------------------------------------------------------------
//
// No printed card replaces a counter removal, so these rows install the typed
// optional replacement `ReplacementDefinition::new(RemoveCounter)
// .mode(Optional { decline: None })` (the fixture of the effect-path tests in
// `effects/counters.rs`). Its applier hands the event back unchanged, so
// accepting and declining both remove what the cost asked for: the replacement
// leg's reading is derived from that applier, not from a card. What the rows
// pin is the payment around it — the removal waits for the choice, the cost is
// still paid (CR 601.2h), and the activation reads the count actually removed
// once it is settled (CR 118.11), never a zero recorded while it waited.

/// Accept the optional replacement at a `ReplacementChoice`.
const ACCEPT: usize = 0;
/// Decline it: the removal happens unreplaced.
const DECLINE: usize = 1;

/// Put `replacement` on the P0 "Steward" and return its id.
fn install_replacement_on_steward(
    runner: &mut GameRunner,
    replacement: ReplacementDefinition,
) -> ReplacementId {
    let holder = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Steward")
        .expect("the board has the replacement's holder");
    let obj = runner.state_mut().objects.get_mut(&holder).unwrap();
    let index = obj.replacement_definitions.len();
    Arc::make_mut(&mut obj.base_replacement_definitions).push(replacement.clone());
    obj.replacement_definitions.push(replacement);
    ReplacementId {
        source: holder,
        index,
    }
}

/// An optional replacement of `event`, with an optional post-effect.
fn optional_replacement(
    event: ReplacementEvent,
    execute: Option<AbilityDefinition>,
) -> ReplacementDefinition {
    let replacement =
        ReplacementDefinition::new(event).mode(ReplacementMode::Optional { decline: None });
    match execute {
        Some(execute) => replacement.execute(execute),
        None => replacement,
    }
}

/// A P0 permanent carrying the optional counter-removal replacement.
fn install_optional_counter_removal_replacement(runner: &mut GameRunner) {
    install_replacement_on_steward(
        runner,
        optional_replacement(ReplacementEvent::RemoveCounter, None),
    );
}

/// `main_phase` with the replacement's holder on P0's side.
fn replacement_board() -> GameScenario {
    let mut scenario = main_phase();
    scenario.add_creature(P0, "Steward", 1, 1);
    scenario
}

/// The optional replacement's accept/decline prompt is up (positive reach).
fn assert_optional_replacement_prompt(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice {
                candidate_count: 2,
                kind: ReplacementChoiceKind::OptionalBranch,
                ..
            }
        ),
        "reach: the counter removal waits on the optional replacement: {:?}",
        runner.state().waiting_for
    );
}

fn choose_replacement(runner: &mut GameRunner, index: usize) {
    runner
        .act(GameAction::ChooseReplacement { index })
        .unwrap_or_else(|err| panic!("the replacement choice is accepted: {err:?}"));
}

/// The parked stack-route counter-cost root: its attached payment, record and
/// completion.
fn parked_counter_cost_root(
    runner: &GameRunner,
) -> (
    &PendingCast,
    &PausedCounterCostRemoval,
    &PendingCostMoveCompletion,
) {
    match &runner.state().pending_cost_move_resume {
        Some(PendingCostMoveResume::CounterCostRemovalPayment {
            pending: Some(pending),
            removal,
            completion,
            ..
        }) => (pending, removal, completion),
        other => panic!("the payment is parked on its counter-cost removal: {other:?}"),
    }
}

/// The parked mana-route cursor.
fn parked_mana_cursor(runner: &GameRunner) -> &ManaAbilityCostCursor {
    match &runner.state().pending_cost_move_resume {
        Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. }) => cursor,
        other => panic!("the mana ability is parked on its cost cursor: {other:?}"),
    }
}

/// R2 stack route, direct pay: Essence Bottle's "Remove all elixir counters"
/// waits on the replacement choice, so the activation pauses with the cost
/// unpaid and no count. After the choice, the stack entry reads the 3 counters
/// actually removed: +6 life. Declining removes the same 3 unreplaced.
#[test]
fn deferred_cost_removal_is_counted_after_its_replacement_choice() {
    for choice in [ACCEPT, DECLINE] {
        let mut scenario = replacement_board();
        let bottle = scenario
            .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
            .id();
        scenario.with_counter(bottle, counter("elixir"), 3);
        let mut runner = scenario.build();
        install_optional_counter_removal_replacement(&mut runner);
        let life_before = runner.life(P0);

        let index = counter_cost_ability(&runner, bottle, 0);
        activate(&mut runner, bottle, index);
        assert_optional_replacement_prompt(&runner);
        assert!(runner.state().stack.is_empty(), "not activated yet");
        assert_eq!(
            counters_on(&runner, bottle, &counter("elixir")),
            3,
            "the removal waits on the choice"
        );
        let (pending, _, _) = parked_counter_cost_root(&runner);
        assert_eq!(
            pending.ability.context.cost_removed_counter_amount, None,
            "a parked payment holds no count for a removal still waiting"
        );

        choose_replacement(&mut runner, choice);
        assert_eq!(runner.state().stack.len(), 1, "choice {choice}: placed");
        assert_eq!(top_stack_count(&runner), Some(3), "choice {choice}");
        assert_eq!(counters_on(&runner, bottle, &counter("elixir")), 0);

        drive(
            &mut runner,
            priority_with_empty_stack,
            &mut Answers::default(),
        );
        assert_eq!(runner.life(P0) - life_before, 6, "choice {choice}");
        assert!(payment_ownership_is_clear(&runner), "choice {choice}");
    }
}

/// R2 stack route, selected handler with a re-park: Ooze Flux removes 2 from
/// First and 1 from Second; each removal waits on its own choice and is settled
/// before the next one is made. The Ooze is 3/3.
#[test]
fn deferred_removals_from_among_settle_one_at_a_time() {
    let mut scenario = replacement_board();
    let flux = scenario
        .add_enchantment_from_oracle(P0, "Ooze Flux", OOZE_FLUX)
        .id();
    let first = scenario.add_creature(P0, "First", 1, 1).id();
    let second = scenario.add_creature(P0, "Second", 1, 1).id();
    scenario.with_counter(first, CounterType::Plus1Plus1, 2);
    scenario.with_counter(second, CounterType::Plus1Plus1, 1);
    add_mana(&mut scenario, &[ManaType::Colorless, ManaType::Green]);
    let mut runner = scenario.build();
    install_optional_counter_removal_replacement(&mut runner);
    let mut answers = Answers {
        x: Some(3),
        pay_amount: Some(3),
        distribution: Some(vec![
            CounterCostChoice {
                object_id: first,
                counter_type: CounterType::Plus1Plus1,
                count: 2,
            },
            CounterCostChoice {
                object_id: second,
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            },
        ]),
        ..Answers::default()
    };

    let index = counter_cost_ability(&runner, flux, 0);
    activate(&mut runner, flux, index);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut answers,
    );
    assert_optional_replacement_prompt(&runner);
    let (pending, removal, _) = parked_counter_cost_root(&runner);
    assert_eq!(pending.ability.context.cost_removed_counter_amount, None);
    assert_eq!(removal.in_flight.removal.object_id, first);
    assert_eq!(removal.remaining.len(), 1, "Second's removal is still owed");

    choose_replacement(&mut runner, ACCEPT);
    assert_optional_replacement_prompt(&runner);
    assert_eq!(counters_on(&runner, first, &CounterType::Plus1Plus1), 0);
    assert_eq!(counters_on(&runner, second, &CounterType::Plus1Plus1), 1);
    let (pending, removal, _) = parked_counter_cost_root(&runner);
    assert_eq!(
        pending.ability.context.cost_removed_counter_amount,
        Some(2),
        "First's settled 2 are credited before Second's removal is made"
    );
    assert_eq!(removal.in_flight.removal.object_id, second);
    assert!(removal.remaining.is_empty());

    answers.replacement_choices.push_back(ACCEPT);
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert!(
        answers.replacement_choices.is_empty(),
        "Second's removal raised its own prompt"
    );
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "reach: the activation went on to pay {{1}}{{G}}"
    );
    assert_eq!(top_stack_count(&runner), Some(3));

    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_one_three_three_ooze(&runner, first, second);
}

/// R2 mana route, chosen-count leaf: Saprazzan Cove's "Remove any number of
/// storage counters" (2 of 3) waits on the choice. The cursor holds the removal,
/// has moved past the counter component and holds no count; after the choice
/// the mana ability reads the 2 removed: {U}{U}.
#[test]
fn deferred_mana_cost_removal_is_counted_after_its_replacement_choice() {
    let mut scenario = replacement_board();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let mut runner = scenario.build();
    install_optional_counter_removal_replacement(&mut runner);

    let index = counter_cost_ability(&runner, cove, 0);
    activate(&mut runner, cove, index);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::pay(2),
    );
    assert_optional_replacement_prompt(&runner);
    let cursor = parked_mana_cursor(&runner);
    assert!(
        cursor.counter_removal.is_some(),
        "the cursor holds the removal"
    );
    assert!(
        !cursor.remaining.iter().any(cost_removes_counters),
        "the cursor moved past the counter component: {:?}",
        cursor.remaining
    );
    assert_eq!(cursor.cost_removed_counters, None);
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 3);

    choose_replacement(&mut runner, ACCEPT);
    assert_eq!(pool(&runner, ManaType::Blue), 2);
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 1);
    assert!(payment_ownership_is_clear(&runner));
}

/// B1-B, mana route, fixed-count self leaf: Pentad Prism's "Remove a charge
/// counter" is paid by the cursor's self-contained arm. It waits on the choice
/// inside the cursor, and after the choice the mana activation owns the 1
/// counter removed: one mana, charge 2 -> 1. Declining reads the same.
#[test]
fn deferred_fixed_mana_cost_removal_pauses_in_the_cursor() {
    for choice in [ACCEPT, DECLINE] {
        let mut scenario = replacement_board();
        let prism = scenario
            .add_artifact_from_oracle(P0, "Pentad Prism", PENTAD_PRISM)
            .id();
        scenario.with_counter(prism, counter("charge"), 2);
        let mut runner = scenario.build();
        install_optional_counter_removal_replacement(&mut runner);

        let index = counter_cost_ability(&runner, prism, 0);
        activate(&mut runner, prism, index);
        assert_optional_replacement_prompt(&runner);
        let cursor = parked_mana_cursor(&runner);
        assert!(
            cursor.counter_removal.is_some(),
            "the cursor holds the removal"
        );
        assert!(!cursor.remaining.iter().any(cost_removes_counters));
        assert_eq!(
            cursor.cost_removed_counters, None,
            "nothing is credited while the removal waits"
        );
        assert_eq!(counters_on(&runner, prism, &counter("charge")), 2);

        choose_replacement(&mut runner, choice);
        let WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "choice {choice}: the mana ability asks for its colour: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(pending.cost_removed_counters, Some(1), "choice {choice}");
        assert_eq!(counters_on(&runner, prism, &counter("charge")), 1);

        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            })
            .expect("the colour is chosen");
        assert_eq!(runner.state().players[0].mana_pool.total(), 1);
        assert_eq!(counters_on(&runner, prism, &counter("charge")), 1);
        assert!(payment_ownership_is_clear(&runner), "choice {choice}");
    }
}

/// B4-a: Geometric Nexus pays {6} first through the mana-first detour; its
/// residual "{T}, Remove all charge counters" is paid as the ability is pushed
/// and waits on the choice. The count rides the paused activation across both
/// entries into the payment: the Fractal gets the 3 counters removed.
#[test]
fn geometric_nexus_deferred_residual_counter_leg_counts_after_the_choice() {
    let mut scenario = replacement_board();
    let nexus = scenario
        .add_artifact_from_oracle(P0, "Geometric Nexus", GEOMETRIC_NEXUS)
        .id();
    scenario.with_counter(nexus, counter("charge"), 3);
    add_mana(&mut scenario, &colorless(6));
    let mut runner = scenario.build();
    install_optional_counter_removal_replacement(&mut runner);

    let index = counter_cost_ability(&runner, nexus, 0);
    perf_counters::reset();
    activate(&mut runner, nexus, index);
    let reaches = perf_counters::activation_cost_route_snapshot().guard_reaches;
    assert!(
        reaches[ActivationCostGuardSite::PushToStack as usize] > 0
            && reaches[ActivationCostGuardSite::ManaResume as usize] > 0,
        "reach: the mana was paid first and the counter leg as the push's residual: {reaches:?}"
    );
    assert_optional_replacement_prompt(&runner);
    assert!(runner.state().players[0].mana_pool.is_empty(), "{{6}} paid");
    assert!(runner.state().stack.is_empty());
    assert_eq!(counters_on(&runner, nexus, &counter("charge")), 3);
    let (pending, _, _) = parked_counter_cost_root(&runner);
    assert_eq!(pending.activation_cost, None, "no residual is left");
    assert_eq!(pending.ability.context.cost_removed_counter_amount, None);

    choose_replacement(&mut runner, ACCEPT);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(top_stack_count(&runner), Some(3));
    assert_eq!(counters_on(&runner, nexus, &counter("charge")), 0);

    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    let fractals = battlefield_named(&runner, "Fractal");
    assert_eq!(fractals.len(), 1, "the Fractal survives on the battlefield");
    assert_eq!(
        counters_on(&runner, fractals[0], &CounterType::Plus1Plus1),
        3
    );
    assert!(payment_ownership_is_clear(&runner));
}

/// B4-b: Magosi's automatic "{T}, Remove an eon counter" is paid before its
/// selected self-return; the removal waits on the choice, so the root carries
/// the selected return. After the choice the removal is settled first, then
/// Magosi returns to hand, and the activation reads its 1 counter removed.
/// Magosi starts with 2 eon counters so the order is observable: settling after
/// the return would also count the eon counter that ceased to exist when Magosi
/// left the battlefield (CR 122.2) and read 2, not the 1 the cost removed.
/// Unreplaced control first: the return completes in the same action.
#[test]
fn magosi_deferred_counter_leg_completes_its_selected_return_after_the_choice() {
    for replaced in [false, true] {
        let mut scenario = replacement_board();
        let magosi = scenario
            .add_land_from_oracle(P0, "Magosi, the Waterveil", MAGOSI)
            .id();
        scenario.with_counter(magosi, counter("eon"), 2);
        let mut runner = scenario.build();
        runner.state_mut().objects.get_mut(&magosi).unwrap().tapped = false;
        if replaced {
            install_optional_counter_removal_replacement(&mut runner);
        }

        let index = counter_cost_ability(&runner, magosi, 0);
        activate(&mut runner, magosi, index);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::PayCost {
                    kind: PayCostKind::ReturnToHand,
                    ..
                }
            ),
            "reach: the self-return is a selected cost: {:?}",
            runner.state().waiting_for
        );
        let mut answers = Answers {
            return_choice: vec![magosi],
            ..Answers::default()
        };
        drive(
            &mut runner,
            |state| {
                priority_with_stack(1)(state)
                    || matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. })
            },
            &mut answers,
        );

        if replaced {
            assert_optional_replacement_prompt(&runner);
            assert!(runner.state().stack.is_empty());
            assert_eq!(runner.state().objects[&magosi].zone, Zone::Battlefield);
            assert_eq!(counters_on(&runner, magosi, &counter("eon")), 2);
            let (pending, _, completion) = parked_counter_cost_root(&runner);
            assert_eq!(pending.ability.context.cost_removed_counter_amount, None);
            assert!(
                matches!(
                    completion,
                    PendingCostMoveCompletion::CompleteSelectedReturnToHand { selected, .. }
                        if selected == &vec![magosi]
                ),
                "the root carries the selected return: {completion:?}"
            );
            choose_replacement(&mut runner, ACCEPT);
        }

        assert_eq!(runner.state().stack.len(), 1, "replaced {replaced}");
        assert_eq!(runner.state().objects[&magosi].zone, Zone::Hand);
        assert_eq!(top_stack_count(&runner), Some(1), "replaced {replaced}");

        drive(&mut runner, priority_with_empty_stack, &mut answers);
        assert_eq!(runner.state().extra_turns.len(), 1, "replaced {replaced}");
        assert!(payment_ownership_is_clear(&runner), "replaced {replaced}");
    }
}

/// Typed control: "Whenever a counter is removed from a permanent you control,
/// you gain 1 life." The parser reads a counter-removed trigger only when it is
/// self-referential (Vanishing, suspend), so the observer is built typed.
fn counter_removed_witness() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::CounterRemoved)
        .valid_card(TargetFilter::Typed(
            TypedFilter::permanent().controller(ControllerRef::You),
        ))
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ))
}

/// The sources of an ordering prompt, sorted; panics when `waiting_for` is not one.
fn ordering_prompt_sources(waiting_for: &WaitingFor, controller: PlayerId) -> Vec<ObjectId> {
    let WaitingFor::OrderTriggers { player, triggers } = waiting_for else {
        panic!("expected one ordering prompt, got {waiting_for:?}");
    };
    assert_eq!(*player, controller);
    let mut sources: Vec<ObjectId> = triggers.iter().map(|trigger| trigger.source_id).collect();
    sources.sort();
    sources
}

/// Sync-review F1: a counter-cost root whose resumed selected return pauses on
/// its own replacement. Magosi's removal waits on its choice; once it settles,
/// the root resumes into the selected self-return, which pauses again (a Moved
/// replacement), parking the activation as a cost-move root. That cost park
/// stands down for a live activation (CR 602.2a-b + CR 603.3b), so the
/// removal's observer waits in the activation's carrier, and the activation's
/// own observer joins it: one CR 603.3b batch, one ordering prompt, both above
/// the activation.
#[test]
fn a_stood_down_cost_park_inside_a_resumed_counter_root_keeps_one_activation_batch() {
    let mut scenario = replacement_board();
    let magosi = scenario
        .add_land_from_oracle(P0, "Magosi, the Waterveil", MAGOSI)
        .id();
    scenario.with_counter(magosi, counter("eon"), 2);
    let witness = {
        let mut builder = scenario.add_artifact_from_oracle(P0, "Witness", "");
        builder.with_trigger_definition(counter_removed_witness());
        builder.id()
    };
    let rings = scenario
        .add_artifact_from_oracle(P0, "Rings of Brighthearth", RINGS_OF_BRIGHTHEARTH)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&magosi).unwrap().tapped = false;
    install_optional_counter_removal_replacement(&mut runner);
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::Moved, None).destination_zone(Zone::Hand),
    );
    for observer in [witness, rings] {
        assert_eq!(
            runner.state().objects[&observer].trigger_definitions.len(),
            1,
            "reach: each observer has its trigger"
        );
    }

    let index = counter_cost_ability(&runner, magosi, 0);
    activate(&mut runner, magosi, index);
    runner
        .act(GameAction::SelectCards {
            cards: vec![magosi],
        })
        .expect("Magosi is chosen for its return");
    assert_optional_replacement_prompt(&runner);
    let (_, _, completion) = parked_counter_cost_root(&runner);
    assert!(
        matches!(
            completion,
            PendingCostMoveCompletion::CompleteSelectedReturnToHand { .. }
        ),
        "reach: the counter root carries the selected return"
    );

    // The removal settles and the root resumes into the selected return, which
    // pauses on its own replacement.
    choose_replacement(&mut runner, ACCEPT);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach: the selected return waits on its replacement: {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::Cast {
                pending: Some(_),
                ..
            })
        ),
        "reach: the activation is parked on its cost move: {:?}",
        runner.state().pending_cost_move_resume
    );
    assert_eq!(counters_on(&runner, magosi, &counter("eon")), 1);
    assert_eq!(runner.state().objects[&magosi].zone, Zone::Battlefield);
    assert!(
        runner.state().stack.is_empty(),
        "nothing is on the stack mid-activation"
    );
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "the removal's observer is not parked publicly"
    );

    choose_replacement(&mut runner, ACCEPT);
    assert_eq!(runner.state().objects[&magosi].zone, Zone::Hand);
    assert_eq!(
        ordering_prompt_sources(&runner.state().waiting_for, P0),
        {
            let mut both = vec![witness, rings];
            both.sort();
            both
        },
        "one ordering prompt for the whole activation batch"
    );
    let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
        unreachable!("checked above");
    };
    runner
        .act(GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        })
        .expect("the batch is ordered");
    let stack = &runner.state().stack;
    assert!(
        matches!(stack[0].kind, StackEntryKind::ActivatedAbility { .. })
            && stack[0].source_id == magosi,
        "the activation is at the bottom: {:?}",
        stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(stack.len(), 3, "the activation and its two triggers");
    assert_eq!(
        witness_entries(runner.state(), witness),
        1,
        "collected once"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "no second ordering prompt follows"
    );
}

const PROFESSOR_HOJO: &str = "The first activated ability you activate during your turn that targets a creature you control costs {2} less to activate.\nWhenever one or more creatures you control become the target of an activated ability, draw a card. This ability triggers only once each turn.";
/// Synthetic control: a modal activated ability whose mode targets and whose
/// cost asks which artifact to sacrifice.
const MODAL_SACRIFICE_ABILITY: &str = "{2}, Sacrifice an artifact: Choose one \u{2014}\n\u{2022} Tap target creature you control.\n\u{2022} You gain 1 life.";

/// CR 602.2a + CR 602.2b + CR 700.2a: an activated ability's modes are chosen as
/// part of activating it, so its announcement is live while the mode prompt is
/// answered. Choosing the targeting mode makes Hojo a target, and the activation
/// then waits on its sacrifice choice; Hojo's trigger waits with the
/// announcement and goes on the stack once, above the activation (CR 603.2 +
/// CR 603.3).
#[test]
fn a_target_trigger_from_an_activated_mode_choice_survives_its_cost_prompt() {
    let mut scenario = main_phase();
    let hojo = scenario
        .add_creature_from_oracle(P0, "Professor Hojo", 2, 2, PROFESSOR_HOJO)
        .id();
    let modal = scenario
        .add_artifact_from_oracle(P0, "Modal Engine", MODAL_SACRIFICE_ABILITY)
        .id();
    let fodder = scenario.add_artifact_from_oracle(P0, "Fodder", "").id();
    scenario.add_artifact_from_oracle(P0, "Spare", "");
    add_mana(&mut scenario, &colorless(2));
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&hojo].trigger_definitions.len(),
        1,
        "reach: Hojo has its trigger"
    );

    activate(&mut runner, modal, 0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice {
                is_activated: true,
                ..
            }
        ),
        "reach: the activation asks for its mode: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("the targeting mode is chosen");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "reach: the activation waits on its sacrifice choice: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing is on the stack mid-activation"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("the artifact is sacrificed");
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    let stack = &runner.state().stack;
    assert!(
        matches!(stack[0].kind, StackEntryKind::ActivatedAbility { .. })
            && stack[0].source_id == modal,
        "the activation is at the bottom: {:?}",
        stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        witness_entries(runner.state(), hojo),
        1,
        "Hojo triggers once"
    );
}

/// Stage a full prevention of the parked counter removal. No RemoveCounter
/// applier yields `Prevented` today (`remove_counter_applier` always modifies
/// the event), so the parked choice is re-pointed at a skip-turn definition on
/// the same holder, whose applier prevents its event outright. This mirrors the
/// staged regeneration-shield producer of the CR 608.3e prevented-entry test in
/// `engine_replacement.rs`. The parked event is still the RemoveCounter, and the
/// resume runs through the real `GameAction::ChooseReplacement` entry.
fn stage_prevented_counter_removal(runner: &mut GameRunner) {
    stage_prevented_pending_event(runner, |proposed| {
        matches!(proposed, ProposedEvent::RemoveCounter { .. })
    });
}

/// `stage_prevented_counter_removal` for any parked event `expected` accepts.
fn stage_prevented_pending_event(runner: &mut GameRunner, expected: fn(&ProposedEvent) -> bool) {
    let prevention = install_replacement_on_steward(
        runner,
        ReplacementDefinition::new(ReplacementEvent::BeginTurn),
    );
    let pending = runner
        .state_mut()
        .pending_replacement
        .as_mut()
        .expect("an event waits on its replacement choice");
    assert!(
        expected(&pending.proposed),
        "reach: the parked event is the expected one: {:?}",
        pending.proposed
    );
    pending.candidates = vec![prevention];
    pending.is_optional = false;
}

/// Prevented boundary, a removal no cost owns (CR 616.1): Ember Shot's 3 damage
/// to a planeswalker removes loyalty counters (CR 120.3c), and that removal
/// waits on the choice with the spell's "Draw a card." still to resolve. When
/// the removal is prevented outright, no loyalty is removed (CR 614.6) and the
/// spell resumes: its controller draws the card and the spell finishes. Only a
/// cost-move root's removal may leave this drain for the cost-move resume.
#[test]
fn a_prevented_damage_counter_removal_resumes_the_resolving_spell() {
    let mut scenario = replacement_board();
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Walker", "Gideon", 5, "+1: You gain 1 life.")
        .id();
    let shot = scenario
        .add_spell_to_hand_from_oracle(P0, "Ember Shot", true, EMBER_SHOT)
        .id();
    let mut mana = colorless(6);
    mana.push(ManaType::Red);
    add_mana(&mut scenario, &mana);
    let mut runner = scenario.build();
    install_optional_counter_removal_replacement(&mut runner);

    let card_id = runner.state().objects[&shot].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: shot,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|err| panic!("Ember Shot is cast: {err:?}"));
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::default().object(walker),
    );
    assert_optional_replacement_prompt(&runner);
    assert!(
        runner.state().active_ability_continuation().is_some(),
        "reach: the spell's draw waits behind the removal"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(counters_on(&runner, walker, &CounterType::Loyalty), 5);
    let hand_at_choice = runner.state().players[0].hand.len();
    stage_prevented_counter_removal(&mut runner);

    choose_replacement(&mut runner, 0);
    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(
        counters_on(&runner, walker, &CounterType::Loyalty),
        5,
        "the prevented removal removed nothing"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_at_choice + 1,
        "the spell's draw still resolves"
    );
    assert_eq!(runner.state().objects[&shot].zone, Zone::Graveyard);
    assert!(runner.state().active_ability_continuation().is_none());
    assert!(payment_ownership_is_clear(&runner));
}

/// Prevented boundary, a cost-move root's removal: Essence Bottle's "Remove all
/// elixir counters" waits on the choice with the activation parked. When the
/// removal is prevented outright, the cost is still paid (CR 118.11) and no
/// counter was removed (CR 614.6), so the parked root resumes: the ability goes
/// on the stack reading 0 counters removed and gains no life.
#[test]
fn a_prevented_cost_counter_removal_resumes_its_cost_root() {
    let mut scenario = replacement_board();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    install_optional_counter_removal_replacement(&mut runner);
    let life_before = runner.life(P0);

    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    assert_optional_replacement_prompt(&runner);
    let (pending, _, _) = parked_counter_cost_root(&runner);
    assert_eq!(pending.ability.context.cost_removed_counter_amount, None);
    stage_prevented_counter_removal(&mut runner);

    choose_replacement(&mut runner, 0);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the parked activation is placed"
    );
    assert_eq!(top_stack_count(&runner), Some(0));
    assert_eq!(counters_on(&runner, bottle, &counter("elixir")), 3);

    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(runner.life(P0), life_before);
    assert!(payment_ownership_is_clear(&runner));
}

// ---------------------------------------------------------------------------
// R5 — a counter-cost root resumes only once its replacement subtree retires
// ---------------------------------------------------------------------------
//
// CR 601.2h + CR 602.2b + CR 616.1 + CR 608.2c: a replacement accepted for a
// cost's counter removal can do further work (its post-effect: a choice, an
// effect removal queue, damage, a rider). That work is the removal's subtree:
// it finishes first, wherever it finishes, and the paused cost root resumes
// once it has retired, before any outer payment the root is paying for. Every
// row is a typed-input witness: no printed card has a RemoveCounter
// replacement.

/// Synthetic control: a mill cost with no other cost leg (CR 701.17a).
const MILL_COST_CONTROL: &str = "{T}, Mill a card: You gain 1 life.";
/// Synthetic control: a life gain the GainLife replacement can act on.
const GAIN_CONTROL: &str = "{T}: You gain 1 life.";
const DIVINATION: &str = "Draw two cards.";

/// The index of the first event matching `pred`. The panic is the reach of
/// every ordering assertion.
fn event_index(events: &[GameEvent], pred: impl Fn(&GameEvent) -> bool) -> usize {
    events
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("the expected event is in {events:#?}"))
}

/// Whether a rider continuation is parked anywhere on the resolution stack.
/// The top-only accessor cannot see one under an effect removal queue.
fn parked_child_rider(state: &GameState) -> bool {
    state
        .resolution_stack
        .iter()
        .any(|frame| matches!(frame, ResolutionFrame::AbilityContinuation(_)))
}

/// The effect removal queue anywhere on the resolution stack. A mill cost's
/// batch delivery can sit above it, where the top-only accessor cannot see it.
fn removal_queue(state: &GameState) -> Option<&PendingCounterRemovalQueue> {
    state.resolution_stack.iter().find_map(|frame| match frame {
        ResolutionFrame::CounterRemovals(queue) => Some(queue),
        _ => None,
    })
}

/// A P0 artifact with a charge counter and a +1/+1 counter for a child removal.
fn add_battery(scenario: &mut GameScenario) -> ObjectId {
    let battery = scenario.add_artifact_from_oracle(P0, "Battery", "").id();
    scenario.with_counter(battery, counter("charge"), 1);
    scenario.with_counter(battery, CounterType::Plus1Plus1, 1);
    battery
}

/// The child post-effect: remove every counter from the Battery (two queue
/// entries), then gain 1 life per counter that removal actually removed.
fn t3_child_execute(battery: ObjectId) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RemoveCounter {
            counter_type: None,
            count: QuantityExpr::Fixed { value: -1 },
            target: TargetFilter::SpecificObject { id: battery },
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectCount,
            },
            player: TargetFilter::Controller,
        },
    ))
}

/// T10's child: the same queue as `t3_child_execute`, with a rider that draws
/// one card per counter that removal actually removed. It is not a life gain, so
/// it cannot match T10's GainLife replacement.
fn t10_child_execute(battery: ObjectId) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RemoveCounter {
            counter_type: None,
            count: QuantityExpr::Fixed { value: -1 },
            target: TargetFilter::SpecificObject { id: battery },
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectCount,
            },
            target: TargetFilter::Controller,
        },
    ))
}

fn scry_one() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
}

fn is_life_gain(event: &GameEvent) -> bool {
    matches!(event, GameEvent::LifeChanged { player_id, amount, .. } if *player_id == P0 && *amount > 0)
}

fn is_counter_removal_resolved(event: &GameEvent) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::RemoveCounter,
            ..
        }
    )
}

fn is_activation_of(source: ObjectId) -> impl Fn(&GameEvent) -> bool {
    move |event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == source)
}

fn choose_replacement_events(runner: &mut GameRunner, index: usize) -> Vec<GameEvent> {
    runner
        .act(GameAction::ChooseReplacement { index })
        .unwrap_or_else(|err| panic!("the replacement choice is accepted: {err:?}"))
        .events
}

fn assert_root_is_counter_cost(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::CounterCostRemovalPayment { .. })
        ),
        "the activation is still parked on its counter cost: {:?}",
        runner.state().pending_cost_move_resume
    );
}

/// M-F1: Essence Bottle's removal is replaced by a replacement whose post-effect
/// scries. Answering the Scry finishes the subtree at the priority boundary, and
/// the parked activation is placed reading the 3 counters removed.
#[test]
fn a_counter_cost_root_resumes_after_its_replacement_scry_child() {
    let mut scenario = replacement_board();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(scry_one())),
    );
    let life_before = runner.life(P0);

    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    assert_optional_replacement_prompt(&runner);
    choose_replacement(&mut runner, ACCEPT);
    let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach: the post-effect scries: {:?}",
            runner.state().waiting_for
        );
    };
    assert_root_is_counter_cost(&runner);
    assert!(runner.state().stack.is_empty(), "not placed yet");
    assert_eq!(counters_on(&runner, bottle, &counter("elixir")), 0);

    runner
        .act(GameAction::SelectCards { cards })
        .expect("the scry is answered");
    assert_eq!(runner.state().stack.len(), 1, "the activation is placed");
    assert_eq!(top_stack_count(&runner), Some(3));
    assert!(payment_ownership_is_clear(&runner));
    assert!(removal_queue(runner.state()).is_none());

    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(runner.life(P0) - life_before, 6);
}

/// The mana route of the same shape: the cursor's root was already admitted at
/// the priority boundary; it still waits for the Scry and then produces the 2
/// blue the removal paid for.
#[test]
fn a_mana_cost_root_resumes_after_its_replacement_scry_child() {
    let mut scenario = replacement_board();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(scry_one())),
    );

    let index = counter_cost_ability(&runner, cove, 0);
    activate(&mut runner, cove, index);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::pay(2),
    );
    choose_replacement(&mut runner, ACCEPT);
    let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach: the post-effect scries: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        parked_mana_cursor(&runner).counter_removal.is_some(),
        "reach: the cursor holds the removal at the Scry"
    );

    runner
        .act(GameAction::SelectCards { cards })
        .expect("the scry is answered");
    assert_eq!(pool(&runner, ManaType::Blue), 2);
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 1);
    assert!(payment_ownership_is_clear(&runner));
}

/// M-F2: the root's replacement starts a child removal queue (two entries) with
/// a rider. The child's first removal is prevented, its second declined: the
/// queue and its rider finish before the activation is placed, and the rider
/// reads the 1 counter its own queue removed, not the root's 3.
#[test]
fn a_prevented_child_removal_queue_and_rider_finish_before_the_cost_root() {
    let mut scenario = replacement_board();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let battery = add_battery(&mut scenario);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::RemoveCounter,
            Some(t3_child_execute(battery)),
        ),
    );
    let life_before = runner.life(P0);

    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    assert_optional_replacement_prompt(&runner);
    choose_replacement(&mut runner, ACCEPT);

    // Entry 1 of the child queue waits on its own choice.
    assert_optional_replacement_prompt(&runner);
    assert!(
        parked_child_rider(runner.state()),
        "reach: the rider is parked"
    );
    assert!(
        removal_queue(runner.state()).is_some_and(|queue| queue.in_flight.is_some()),
        "reach: the child queue's removal is in flight"
    );
    assert_root_is_counter_cost(&runner);
    assert!(runner.state().stack.is_empty());
    stage_prevented_counter_removal(&mut runner);
    choose_replacement(&mut runner, 0);

    // Entry 2 waits on its own choice; the activation is still not placed.
    assert_optional_replacement_prompt(&runner);
    assert!(
        parked_child_rider(runner.state()),
        "reach: the rider is parked"
    );
    assert!(
        removal_queue(runner.state()).is_some_and(|queue| queue.in_flight.is_some()),
        "reach: the child queue's second removal is in flight"
    );
    assert_root_is_counter_cost(&runner);
    assert!(
        runner.state().stack.is_empty(),
        "the root waits for its subtree"
    );

    let events = choose_replacement_events(&mut runner, DECLINE);
    let battery_counters = counters_on(&runner, battery, &counter("charge"))
        + counters_on(&runner, battery, &CounterType::Plus1Plus1);
    assert_eq!(battery_counters, 1, "one child removal was prevented");
    assert_eq!(
        runner.life(P0) - life_before,
        1,
        "the rider reads its own queue's 1"
    );
    assert_eq!(runner.state().stack.len(), 1, "the activation is placed");
    assert_eq!(top_stack_count(&runner), Some(3));
    assert!(removal_queue(runner.state()).is_none());
    assert!(!parked_child_rider(runner.state()));
    let resolved = event_index(&events, is_counter_removal_resolved);
    let gain = event_index(&events, is_life_gain);
    let activated = event_index(&events, is_activation_of(bottle));
    assert!(
        resolved < gain && gain < activated,
        "queue {resolved} < rider {gain} < activation {activated}"
    );
    assert!(payment_ownership_is_clear(&runner));

    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(runner.life(P0) - life_before, 7, "+1 rider, +6 Bottle");
}

/// A mill cost whose optional Mill replacement removes a charge counter from the
/// Battery as its post-effect; that child removal waits on the Steward's
/// optional RemoveCounter replacement. Returns the runner at the child's prompt.
fn mill_child_at_its_prompt() -> (GameRunner, ObjectId, ObjectId, i32) {
    let mut scenario = replacement_board();
    let miller = scenario
        .add_artifact_from_oracle(P0, "Miller", MILL_COST_CONTROL)
        .id();
    let battery = scenario.add_artifact_from_oracle(P0, "Battery", "").id();
    scenario.with_counter(battery, counter("charge"), 1);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::Mill,
            Some(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::RemoveCounter {
                    counter_type: Some(counter("charge")),
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SpecificObject { id: battery },
                },
            )),
        ),
    );
    install_optional_counter_removal_replacement(&mut runner);
    let life_before = runner.life(P0);

    activate(&mut runner, miller, 0);
    assert_optional_replacement_prompt(&runner);
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::ActivationMillPayment { .. })
        ),
        "reach: the mill cost is parked: {:?}",
        runner.state().pending_cost_move_resume
    );
    choose_replacement(&mut runner, ACCEPT);
    assert_optional_replacement_prompt(&runner);
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::ActivationMillPayment { .. })
        ),
        "reach: the mill root is still parked at the child's prompt"
    );
    assert!(
        removal_queue(runner.state()).is_some_and(|queue| queue.in_flight.is_some()),
        "reach: the child queue's removal is in flight"
    );
    assert!(
        matches!(
            runner.state().resolution_stack.last(),
            Some(ResolutionFrame::BatchDelivery(_))
        ),
        "reach: the mill's batch is parked above the child's queue"
    );
    (runner, miller, battery, life_before)
}

fn is_milled(event: &GameEvent) -> bool {
    matches!(event, GameEvent::Milled { .. })
}

/// Control, an unrelated outer root: a mill cost's replacement starts a child
/// counter removal that is prevented. The mill's batch and the child's queue
/// both settle (CR 614.6: they are the replaced mill), then the mill root
/// resumes and the activation is placed.
#[test]
fn a_prevented_child_removal_under_a_mill_cost_root_resumes_that_root() {
    let (mut runner, miller, battery, life_before) = mill_child_at_its_prompt();
    stage_prevented_counter_removal(&mut runner);

    let events = choose_replacement_events(&mut runner, 0);
    assert!(removal_queue(runner.state()).is_none());
    assert_eq!(counters_on(&runner, battery, &counter("charge")), 1);
    assert_eq!(runner.state().stack.len(), 1, "the activation is placed");
    let resolved = event_index(&events, is_counter_removal_resolved);
    let milled = event_index(&events, is_milled);
    let activated = event_index(&events, is_activation_of(miller));
    assert!(
        resolved < activated && milled < activated,
        "queue {resolved} and mill {milled} < activation {activated}"
    );
    assert!(payment_ownership_is_clear(&runner));

    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(runner.life(P0) - life_before, 1);
    assert!(runner.state().resolution_stack.is_empty());
}

/// The Execute arm of the same shape: the child's removal is declined, so it
/// happens. Settling the batch exposes the child's queue, which finishes before
/// the mill root resumes.
#[test]
fn a_declined_child_removal_under_a_mill_cost_root_resumes_that_root() {
    let (mut runner, miller, battery, life_before) = mill_child_at_its_prompt();

    let events = choose_replacement_events(&mut runner, DECLINE);
    assert_eq!(
        counters_on(&runner, battery, &counter("charge")),
        0,
        "the declined removal happened"
    );
    assert!(removal_queue(runner.state()).is_none());
    assert_eq!(runner.state().stack.len(), 1, "the activation is placed");
    let resolved = event_index(&events, is_counter_removal_resolved);
    let milled = event_index(&events, is_milled);
    let activated = event_index(&events, is_activation_of(miller));
    assert!(
        resolved < activated && milled < activated,
        "queue {resolved} and mill {milled} < activation {activated}"
    );
    assert!(payment_ownership_is_clear(&runner));

    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(runner.life(P0) - life_before, 1);
    assert!(runner.state().resolution_stack.is_empty());
}

/// A1: a top batch that is not the removal's own. The Mill replacement's
/// post-effect is an unless payment, so the mill's batch parks at that prompt.
/// Saprazzan Cove is activated to pay it and its own removal is prevented: the
/// Cove resumes and the unless payment is offered again, with the batch still
/// parked under it. Paying settles the batch. The row pins the batch's owner
/// only: what becomes of the mill activation after an unless payment inside its
/// cost is outside this row.
#[test]
fn a_mana_root_prevented_above_an_unrelated_parked_batch_leaves_the_batch() {
    let mut scenario = replacement_board();
    let miller = scenario
        .add_artifact_from_oracle(P0, "Miller", MILL_COST_CONTROL)
        .id();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    add_mana(&mut scenario, &colorless(2));
    let mut runner = scenario.build();
    let lose_unless = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: None,
        },
    )
    .unless_pay(UnlessPayModifier {
        cost: AbilityCost::Mana {
            cost: ManaCost::generic(2),
        },
        payer: TargetFilter::Controller,
    });
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::Mill, Some(lose_unless)),
    );
    install_optional_counter_removal_replacement(&mut runner);
    let life_before = runner.life(P0);

    activate(&mut runner, miller, 0);
    choose_replacement(&mut runner, ACCEPT);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "reach: the mill's post-effect waits on its unless payment: {:?}",
        runner.state().waiting_for
    );

    let index = counter_cost_ability(&runner, cove, 0);
    activate(&mut runner, cove, index);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::pay(2),
    );
    assert!(
        parked_mana_cursor(&runner).counter_removal.is_some(),
        "reach: the Cove's own removal waits"
    );
    assert!(
        matches!(
            runner.state().resolution_stack.last(),
            Some(ResolutionFrame::BatchDelivery(_))
        ),
        "reach: the mill's batch is the top frame at the Cove's prompt"
    );
    stage_prevented_counter_removal(&mut runner);

    let events = choose_replacement_events(&mut runner, 0);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the unless payment is offered again: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .resolution_stack
            .iter()
            .any(|frame| matches!(frame, ResolutionFrame::BatchDelivery(_))),
        "the mill's batch stays parked under the unless payment"
    );
    assert!(
        !events.iter().any(is_milled),
        "the batch does not settle mid-payment"
    );
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 3);

    let events = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the unless cost is paid")
        .events;
    assert!(events.iter().any(is_milled), "paying settles the batch");
    assert!(
        !runner
            .state()
            .resolution_stack
            .iter()
            .any(|frame| matches!(frame, ResolutionFrame::BatchDelivery(_))),
        "the batch is gone"
    );
    assert_eq!(runner.life(P0), life_before, "paid: no life loss");
}

/// An unrecorded child removal: the replacement's post-effect deals damage to a
/// planeswalker (CR 120.3c removes loyalty) with a rider. The damage removal is
/// prevented; the rider still runs before the activation is placed.
#[test]
fn a_prevented_damage_child_finishes_its_rider_before_the_cost_root() {
    let mut scenario = replacement_board();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Walker", "Gideon", 5, "+1: You gain 1 life.")
        .id();
    let mut runner = scenario.build();
    // A template post-effect binds no targets, so the damage is non-targeted
    // ("deals 1 damage to the Walker").
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DamageAll {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SpecificObject { id: walker },
            player_filter: None,
            damage_source: None,
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    ));
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(execute)),
    );
    let life_before = runner.life(P0);

    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    assert_optional_replacement_prompt(&runner);
    choose_replacement(&mut runner, ACCEPT);
    assert_optional_replacement_prompt(&runner);
    assert!(
        parked_child_rider(runner.state()),
        "reach: the rider is parked"
    );
    assert!(
        removal_queue(runner.state()).is_none(),
        "reach: the damage removal is not an effect queue"
    );
    assert_root_is_counter_cost(&runner);
    stage_prevented_counter_removal(&mut runner);

    let events = choose_replacement_events(&mut runner, 0);
    assert_eq!(counters_on(&runner, walker, &CounterType::Loyalty), 5);
    assert_eq!(runner.life(P0) - life_before, 1, "the rider ran");
    assert_eq!(runner.state().stack.len(), 1, "the activation is placed");
    assert_eq!(top_stack_count(&runner), Some(3));
    assert!(!parked_child_rider(runner.state()));
    let gain = event_index(&events, is_life_gain);
    let activated = event_index(&events, is_activation_of(bottle));
    assert!(gain < activated, "rider {gain} < activation {activated}");
    assert!(payment_ownership_is_clear(&runner));
}

/// A same-subject child: the replacement's post-effect removes elixir counters
/// from the Bottle itself. Matching by (object, counter type, count) cannot tell
/// it from the root's own removal; the subtree boundary can.
#[test]
fn a_same_subject_child_removal_is_still_the_roots_child() {
    let mut scenario = replacement_board();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RemoveCounter {
            counter_type: Some(counter("elixir")),
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::SpecificObject { id: bottle },
        },
    );
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(execute)),
    );

    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    assert_optional_replacement_prompt(&runner);
    choose_replacement(&mut runner, ACCEPT);
    assert_optional_replacement_prompt(&runner);
    let (_, root_removal, _) = parked_counter_cost_root(&runner);
    let child_removal = removal_queue(runner.state())
        .and_then(|queue| queue.in_flight.as_ref())
        .map(|in_flight| in_flight.removal.clone())
        .expect("reach: the child queue's removal is in flight");
    assert_eq!(
        child_removal, root_removal.in_flight.removal,
        "reach: the child's removal is indistinguishable by identity"
    );
    stage_prevented_counter_removal(&mut runner);

    let events = choose_replacement_events(&mut runner, 0);
    assert!(removal_queue(runner.state()).is_none());
    assert_eq!(runner.state().stack.len(), 1, "the activation is placed");
    assert_eq!(top_stack_count(&runner), Some(3));
    let resolved = event_index(&events, is_counter_removal_resolved);
    let activated = event_index(&events, is_activation_of(bottle));
    assert!(
        resolved < activated,
        "queue {resolved} < activation {activated}"
    );
    assert!(payment_ownership_is_clear(&runner));
}

/// The mana route's own prevented removal: the cursor resumes, adds no mana and
/// leaves the storage counters on.
#[test]
fn a_prevented_mana_cost_counter_removal_resumes_its_cursor() {
    let mut scenario = replacement_board();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let mut runner = scenario.build();
    install_optional_counter_removal_replacement(&mut runner);

    let index = counter_cost_ability(&runner, cove, 0);
    activate(&mut runner, cove, index);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::pay(2),
    );
    assert!(
        parked_mana_cursor(&runner).counter_removal.is_some(),
        "reach: the cursor holds the removal"
    );
    stage_prevented_counter_removal(&mut runner);

    choose_replacement(&mut runner, 0);
    assert_eq!(pool(&runner, ManaType::Blue), 0);
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 3);
    assert!(payment_ownership_is_clear(&runner));
}

/// Reach shared by T9/T10 at each child prompt: the mana root still holds its
/// removal and the child's rider is parked.
fn assert_mana_root_waits_with_child_rider(runner: &GameRunner) {
    assert_optional_replacement_prompt(runner);
    assert!(
        parked_mana_cursor(runner).counter_removal.is_some(),
        "reach: the cursor holds its removal"
    );
    assert!(
        parked_child_rider(runner.state()),
        "reach: the rider is parked"
    );
}

/// B1: Saprazzan Cove pays Divination's cost. The Cove's replacement starts the
/// child queue and rider; they finish, then the Cove's mana root produces its
/// mana, and only then does Divination's payment continue.
#[test]
fn a_child_under_a_mana_cost_root_paying_a_spell_settles_before_the_spell_resumes() {
    let mut scenario = replacement_board();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let battery = add_battery(&mut scenario);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::RemoveCounter,
            Some(t3_child_execute(battery)),
        ),
    );
    let life_before = runner.life(P0);

    let card_id = runner.state().objects[&divination].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: divination,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap_or_else(|err| panic!("Divination is cast: {err:?}"));
    assert!(
        is_mana_payment(runner.state()),
        "reach: Divination waits for mana: {:?}",
        runner.state().waiting_for
    );
    let index = counter_cost_ability(&runner, cove, 0);
    activate(&mut runner, cove, index);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::pay(2),
    );
    choose_replacement(&mut runner, ACCEPT);

    assert_mana_root_waits_with_child_rider(&runner);
    assert!(
        runner.state().pending_cast.is_some(),
        "reach: Divination waits"
    );
    stage_prevented_counter_removal(&mut runner);
    choose_replacement(&mut runner, 0);
    assert_mana_root_waits_with_child_rider(&runner);
    assert!(
        runner.state().pending_cast.is_some(),
        "reach: Divination waits"
    );

    let events = choose_replacement_events(&mut runner, DECLINE);
    let resolved = event_index(&events, is_counter_removal_resolved);
    let gain = event_index(&events, is_life_gain);
    let mana = event_index(
        &events,
        |event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == cove),
    );
    assert!(
        resolved < gain && gain < mana,
        "queue {resolved} < rider {gain} < Cove mana {mana}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::SpellCast { object_id, .. } | GameEvent::StackPushed { object_id }
                if *object_id == divination
        )),
        "Divination's payment does not finish in this action"
    );
    assert!(
        is_mana_payment(runner.state()),
        "Divination still waits for mana: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(pool(&runner, ManaType::Blue), 2, "the Cove's 2 blue");
    assert_eq!(runner.life(P0) - life_before, 1, "the rider ran");
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 1);
    assert!(removal_queue(runner.state()).is_none());
}

/// The nested configuration through public actions: a life gain's replacement
/// post-effect (a life loss) pauses at an unless payment, and the Cove is
/// activated to pay it. The Cove's own child (queue and a draw rider) nests into
/// that paused frame; it finishes, the Cove produces its mana, and the unless
/// payment is offered again. The rider draws rather than gains life, so it does
/// not re-trigger the gain's replacement (CR 614.5 forbids re-application only to
/// the same event).
#[test]
fn a_child_under_a_mana_root_paying_inside_a_paused_life_gain_post_effect_settles_first() {
    let mut scenario = replacement_board();
    let gainer = scenario
        .add_artifact_from_oracle(P0, "Gainer", GAIN_CONTROL)
        .id();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let battery = add_battery(&mut scenario);
    let mut runner = scenario.build();
    let lose_unless = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: None,
        },
    )
    .unless_pay(UnlessPayModifier {
        cost: AbilityCost::Mana {
            cost: ManaCost::generic(2),
        },
        payer: TargetFilter::Controller,
    });
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::GainLife, Some(lose_unless)),
    );
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::RemoveCounter,
            Some(t10_child_execute(battery)),
        ),
    );

    activate(&mut runner, gainer, 0);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::default(),
    );
    choose_replacement(&mut runner, ACCEPT);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "reach (P6): the post-effect waits on its unless payment: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .resolution_stack
            .active_post_replacement_or_paired_parent()
            .and_then(|drains| drains.resident())
            .is_some_and(|drain| matches!(drain.status, DrainStatus::Paused)),
        "reach (P6): the post-effect's drain is paused in the active frame"
    );
    let life_at_unless = runner.life(P0);
    let hand_at_unless = runner.state().players[0].hand.len();

    let index = counter_cost_ability(&runner, cove, 0);
    activate(&mut runner, cove, index);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::pay(2),
    );
    choose_replacement(&mut runner, ACCEPT);
    assert_mana_root_waits_with_child_rider(&runner);
    assert!(
        parked_mana_cursor(&runner)
            .counter_removal
            .as_ref()
            .is_some_and(|removal| removal
                .replacement_subtree
                .enclosing
                .is_some_and(|enclosing| enclosing.frame.is_some())),
        "reach: the Cove's removal was proposed inside the paused, stamped frame"
    );
    stage_prevented_counter_removal(&mut runner);
    choose_replacement(&mut runner, 0);
    assert_mana_root_waits_with_child_rider(&runner);

    let events = choose_replacement_events(&mut runner, DECLINE);
    let resolved = event_index(&events, is_counter_removal_resolved);
    let drew = event_index(
        &events,
        |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P0),
    );
    let mana = event_index(
        &events,
        |event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == cove),
    );
    assert!(
        resolved < drew && drew < mana,
        "queue {resolved} < rider {drew} < Cove mana {mana}"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the unless payment is offered again: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 1);
    assert!(removal_queue(runner.state()).is_none());
    assert_eq!(
        runner.state().players[0].hand.len() - hand_at_unless,
        1,
        "the rider drew the 1 its own queue removed"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the unless cost is paid");
    assert_eq!(runner.life(P0), life_at_unless, "paid: no life loss");
    assert!(runner.state().resolution_stack.is_empty());
    assert!(payment_ownership_is_clear(&runner));
}

// ---------------------------------------------------------------------------
// R6 — once the subtree retires, no outer work runs before the root resumes
// ---------------------------------------------------------------------------
//
// CR 601.2h + CR 605.3b + CR 608.2c: a cost being paid is the innermost
// unfinished work. Once the counter-cost root's replacement subtree has
// retired, the root resumes before any later instruction of an outer owner is
// followed, and before the action returns.

fn lose(amount: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: amount },
            target: None,
        },
    )
}

/// "You lose `amount` life unless you pay {2}."
fn lose_unless_two(amount: i32) -> AbilityDefinition {
    lose(amount).unless_pay(UnlessPayModifier {
        cost: AbilityCost::Mana {
            cost: ManaCost::generic(2),
        },
        payer: TargetFilter::Controller,
    })
}

fn active_drain_is_paused(state: &GameState) -> bool {
    state
        .resolution_stack
        .active_post_replacement_or_paired_parent()
        .and_then(|drains| drains.resident())
        .is_some_and(|drain| matches!(drain.status, DrainStatus::Paused))
}

fn is_cove_mana(cove: ObjectId) -> impl Fn(&GameEvent) -> bool {
    move |event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == cove)
}

fn cove_removal_enclosing(runner: &GameRunner) -> Option<EnclosingPostReplacementFrame> {
    parked_mana_cursor(runner)
        .counter_removal
        .as_ref()
        .expect("reach: the Cove's removal is parked")
        .replacement_subtree
        .enclosing
}

/// The A2 T10 board at the life gain's `UnlessPayment`: the Steward's GainLife
/// replacement runs `outer` as its post-effect, and its RemoveCounter
/// replacement runs `child`. Both P6 reaches are asserted.
fn gain_board_at_unless(
    mut scenario: GameScenario,
    outer: AbilityDefinition,
    child: impl FnOnce(ObjectId) -> AbilityDefinition,
) -> (GameRunner, ObjectId) {
    let gainer = scenario
        .add_artifact_from_oracle(P0, "Gainer", GAIN_CONTROL)
        .id();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let battery = add_battery(&mut scenario);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::GainLife, Some(outer)),
    );
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(child(battery))),
    );
    activate(&mut runner, gainer, 0);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::default(),
    );
    choose_replacement(&mut runner, ACCEPT);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "reach (P6): the post-effect waits on its unless payment: {:?}",
        runner.state().waiting_for
    );
    assert!(
        active_drain_is_paused(runner.state()),
        "reach (P6): the post-effect's drain is paused in the active frame"
    );
    (runner, cove)
}

/// Give the P0 artifact `taxer` the typed activated ability "{T}: <effect>",
/// pushed the way `install_replacement_on_steward` pushes a replacement.
fn install_taxer_ability(runner: &mut GameRunner, taxer: ObjectId, effect: AbilityDefinition) {
    let mut ability = effect;
    ability.kind = AbilityKind::Activated;
    let ability = ability.cost(AbilityCost::Tap);
    let obj = runner.state_mut().objects.get_mut(&taxer).unwrap();
    Arc::make_mut(&mut obj.base_abilities).push(ability.clone());
    Arc::make_mut(&mut obj.abilities).push(ability);
}

/// Activate the Cove at the unless payment, pay 2, and accept the replacement of
/// its own removal: the child queue's first entry then waits on its choice.
fn cove_to_first_child_entry(runner: &mut GameRunner, cove: ObjectId) {
    let index = counter_cost_ability(runner, cove, 0);
    activate(runner, cove, index);
    drive(
        runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::pay(2),
    );
    choose_replacement(runner, ACCEPT);
    assert_mana_root_waits_with_child_rider(runner);
}

/// Whether the unless prompt still holds the paused effect's later instruction
/// (its `sub_ability`): the outer tail has not run.
fn unless_prompt_holds_its_tail(state: &GameState) -> bool {
    matches!(
        &state.waiting_for,
        WaitingFor::UnlessPayment { pending_effect, .. } if pending_effect.sub_ability.is_some()
    )
}

fn is_life_loss(event: &GameEvent) -> bool {
    matches!(event, GameEvent::LifeChanged { player_id, amount, .. } if *player_id == P0 && *amount < 0)
}

/// Trace 1: the outer post-effect's later instruction (lose 3) waits until the
/// Cove's root has resumed and the unless payment is answered.
#[test]
fn an_outer_tail_waits_for_the_mana_root_paying_its_unless_cost() {
    let (mut runner, cove) = gain_board_at_unless(
        replacement_board(),
        lose_unless_two(1).sub_ability(lose(3)),
        t10_child_execute,
    );
    assert!(
        unless_prompt_holds_its_tail(runner.state()),
        "reach: the unless prompt holds the outer tail"
    );
    let life_at_unless = runner.life(P0);

    cove_to_first_child_entry(&mut runner, cove);
    assert!(cove_removal_enclosing(&runner).is_some_and(|e| e.frame.is_some()));
    choose_replacement(&mut runner, DECLINE);
    assert_mana_root_waits_with_child_rider(&runner);
    assert!(cove_removal_enclosing(&runner).is_some_and(|e| e.frame.is_some()));

    let events = choose_replacement_events(&mut runner, DECLINE);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the unless payment is offered again: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.life(P0),
        life_at_unless,
        "the outer tail has not run"
    );
    assert!(active_drain_is_paused(runner.state()));
    assert!(
        unless_prompt_holds_its_tail(runner.state()),
        "the unless prompt still holds the outer tail"
    );
    let resolved = event_index(&events, is_counter_removal_resolved);
    let mana = event_index(&events, is_cove_mana(cove));
    assert!(resolved < mana, "queue {resolved} < Cove mana {mana}");

    // Declined, so the unless loss and then the tail run (paying skips the whole
    // pending effect, its later instruction included).
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("the unless cost is declined");
    assert_eq!(
        runner.life(P0) - life_at_unless,
        -4,
        "the loss and then the tail ran"
    );
    assert!(runner.state().resolution_stack.is_empty());
    assert!(payment_ownership_is_clear(&runner));
}

/// Trace 2: the child's last entry is prevented. The Cove's root resumes and
/// the outer post-effect's dispatch stays paused under the re-offered unless
/// payment.
#[test]
fn a_prevented_last_child_entry_leaves_the_outer_unless_post_effect_paused() {
    let (mut runner, cove) =
        gain_board_at_unless(replacement_board(), lose_unless_two(1), t10_child_execute);
    let life_at_unless = runner.life(P0);

    cove_to_first_child_entry(&mut runner, cove);
    choose_replacement(&mut runner, DECLINE);
    assert_mana_root_waits_with_child_rider(&runner);
    assert!(
        removal_queue(runner.state()).is_some_and(|queue| queue.in_flight.is_some()),
        "reach: the second entry is in flight"
    );
    assert!(cove_removal_enclosing(&runner).is_some_and(|e| e.frame.is_some()));
    stage_prevented_counter_removal(&mut runner);

    let events = choose_replacement_events(&mut runner, 0);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the unless payment is offered again: {:?}",
        runner.state().waiting_for
    );
    assert!(
        active_drain_is_paused(runner.state()),
        "the outer dispatch was not retired"
    );
    let resolved = event_index(&events, is_counter_removal_resolved);
    let mana = event_index(&events, is_cove_mana(cove));
    assert!(resolved < mana, "queue {resolved} < Cove mana {mana}");
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 1);
    assert!(removal_queue(runner.state()).is_none());

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the unless cost is paid");
    assert_eq!(runner.life(P0), life_at_unless, "paid: no unless loss");
    assert!(runner.state().resolution_stack.is_empty());
}

/// The non-nested control: the root parks with no post-replacement frame on
/// top, and the ability's later instruction rides in the unless prompt's
/// pending effect, so it can only run after the prompt is answered.
#[test]
fn an_outer_tail_below_the_boundary_waits_for_the_mana_root() {
    let mut scenario = replacement_board();
    let taxer = scenario.add_artifact_from_oracle(P0, "Taxer", "").id();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let battery = add_battery(&mut scenario);
    let mut runner = scenario.build();
    install_taxer_ability(&mut runner, taxer, lose_unless_two(1).sub_ability(lose(3)));
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::RemoveCounter,
            Some(t10_child_execute(battery)),
        ),
    );
    activate(&mut runner, taxer, 0);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
        &mut Answers::default(),
    );
    let life_at_unless = runner.life(P0);
    assert!(
        unless_prompt_holds_its_tail(runner.state()),
        "reach: the unless prompt holds the tail"
    );

    cove_to_first_child_entry(&mut runner, cove);
    assert!(
        cove_removal_enclosing(&runner).is_none(),
        "reach: the root parked with no post-replacement frame on top"
    );
    choose_replacement(&mut runner, DECLINE);
    assert_mana_root_waits_with_child_rider(&runner);

    choose_replacement(&mut runner, DECLINE);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the unless payment is offered again: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P0), life_at_unless, "the tail has not run");
    assert!(
        unless_prompt_holds_its_tail(runner.state()),
        "the unless prompt still holds the tail"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("the unless cost is declined");
    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(
        runner.life(P0) - life_at_unless,
        -4,
        "the loss and then the tail ran"
    );
}

/// Gate (b): the child's rider is a Clash with a life-loss tail. With both
/// libraries empty it queues its tail without a placement prompt, and the
/// queued tail must not retire the outer paused dispatch.
#[test]
fn a_queued_child_rider_leaves_the_outer_unless_post_effect_paused() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Steward", 1, 1);
    let child = |battery: ObjectId| {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RemoveCounter {
                counter_type: None,
                count: QuantityExpr::Fixed { value: -1 },
                target: TargetFilter::SpecificObject { id: battery },
            },
        )
        .sub_ability(AbilityDefinition::new(AbilityKind::Spell, Effect::Clash).sub_ability(lose(1)))
    };
    let (mut runner, cove) = gain_board_at_unless(scenario, lose_unless_two(1), child);
    assert!(
        runner
            .state()
            .players
            .iter()
            .all(|player| player.library.is_empty()),
        "reach: both libraries are empty"
    );
    let life_at_unless = runner.life(P0);

    cove_to_first_child_entry(&mut runner, cove);
    assert!(cove_removal_enclosing(&runner).is_some_and(|e| e.frame.is_some()));
    choose_replacement(&mut runner, DECLINE);
    assert_mana_root_waits_with_child_rider(&runner);

    let events = choose_replacement_events(&mut runner, DECLINE);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the unless payment is offered again: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P0) - life_at_unless, -1, "the queued tail ran");
    assert!(
        active_drain_is_paused(runner.state()),
        "the outer dispatch survived"
    );
    let resolved = event_index(&events, is_counter_removal_resolved);
    let mana = event_index(&events, is_cove_mana(cove));
    assert!(resolved < mana, "queue {resolved} < Cove mana {mana}");

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the unless cost is paid");
    assert_eq!(runner.life(P0) - life_at_unless, -1, "paid: no unless loss");
    assert!(runner.state().resolution_stack.is_empty());
}

/// Gate (c): an outer "repeat twice" loop waits for the Cove's root before
/// starting its second iteration.
#[test]
fn an_outer_repeat_waits_for_the_mana_root_paying_its_unless_cost() {
    let mut scenario = replacement_board();
    let mut repeated = lose_unless_two(1);
    repeated.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
    let taxer = scenario.add_artifact_from_oracle(P0, "Taxer", "").id();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let battery = add_battery(&mut scenario);
    let mut runner = scenario.build();
    install_taxer_ability(&mut runner, taxer, repeated);
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::RemoveCounter,
            Some(t10_child_execute(battery)),
        ),
    );
    let life_before = runner.life(P0);
    activate(&mut runner, taxer, 0);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
        &mut Answers::default(),
    );
    let repeat = runner
        .state()
        .resolution_stack
        .iter()
        .find(|frame| matches!(frame, ResolutionFrame::RepeatFor(_)))
        .cloned();
    assert!(
        repeat.is_some(),
        "reach (P9): the repeat is parked under iteration 1"
    );

    cove_to_first_child_entry(&mut runner, cove);
    assert!(cove_removal_enclosing(&runner).is_none());
    choose_replacement(&mut runner, DECLINE);
    assert_mana_root_waits_with_child_rider(&runner);

    let events = choose_replacement_events(&mut runner, DECLINE);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the unless payment is offered again: {:?}",
        runner.state().waiting_for
    );
    let repeat_now = runner
        .state()
        .resolution_stack
        .iter()
        .find(|frame| matches!(frame, ResolutionFrame::RepeatFor(_)))
        .cloned();
    assert_eq!(repeat_now, repeat, "iteration 2 has not started");
    assert!(events.iter().any(is_cove_mana(cove)));
    assert_eq!(counters_on(&runner, cove, &counter("storage")), 1);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("iteration 1 is paid");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "iteration 2 asks again: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("iteration 2 is declined");
    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(runner.life(P0) - life_before, -1, "only iteration 2's loss");
    assert!(runner.state().resolution_stack.is_empty());
}

/// D4(c): a prevented non-counter event inside the root's subtree (the child's
/// life gain) still lets the child's rider run before the activation is placed.
#[test]
fn a_prevented_non_counter_child_event_finishes_its_rider_before_the_cost_root() {
    let mut scenario = replacement_board();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
    )
    .sub_ability(lose(1));
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(execute)),
    );
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::GainLife, None),
    );
    let life_before = runner.life(P0);

    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    choose_replacement(&mut runner, ACCEPT);
    assert_root_is_counter_cost(&runner);
    assert!(
        parked_child_rider(runner.state()),
        "reach: the rider is parked"
    );
    assert!(runner.state().stack.is_empty());
    stage_prevented_pending_event(&mut runner, |proposed| {
        matches!(proposed, ProposedEvent::LifeGain { .. })
    });

    let events = choose_replacement_events(&mut runner, 0);
    assert_eq!(
        runner.life(P0) - life_before,
        -1,
        "the gain was prevented; the rider ran"
    );
    let loss = event_index(&events, is_life_loss);
    let activated = event_index(&events, is_activation_of(bottle));
    assert!(loss < activated, "rider {loss} < activation {activated}");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(top_stack_count(&runner), Some(3));
    assert!(!parked_child_rider(runner.state()));
    assert!(payment_ownership_is_clear(&runner));

    let mut answers = Answers::default();
    answers.replacement_choices.push_back(DECLINE);
    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_eq!(runner.life(P0) - life_before, 5, "-1 rider, +6 Bottle");
}

/// "Add one mana of any color. You lose 1 life." as the Bottle's removal's
/// post-effect.
fn mana_choice_child() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: vec![
                    ManaColor::White,
                    ManaColor::Blue,
                    ManaColor::Black,
                    ManaColor::Red,
                    ManaColor::Green,
                ],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .sub_ability(lose(1))
}

fn bottle_at_mana_choice(scenario: GameScenario) -> (GameRunner, ObjectId, i32) {
    bottle_at_child_mana_choice(scenario, mana_choice_child())
}

/// Essence Bottle (3 elixir) whose removal's accepted replacement runs `child`,
/// stopped at the child's `ResolvingEffect` colour prompt.
fn bottle_at_child_mana_choice(
    scenario: GameScenario,
    child: AbilityDefinition,
) -> (GameRunner, ObjectId, i32) {
    let mut scenario = scenario;
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(child)),
    );
    let life_before = runner.life(P0);
    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    choose_replacement(&mut runner, ACCEPT);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor {
                context: ManaChoiceContext::ResolvingEffect(_),
                ..
            }
        ),
        "reach: the child asks for its colour: {:?}",
        runner.state().waiting_for
    );
    assert_root_is_counter_cost(&runner);
    assert!(runner.state().stack.is_empty());
    (runner, bottle, life_before)
}

fn choose_green(runner: &mut GameRunner) -> Vec<GameEvent> {
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("the colour is chosen")
        .events
}

/// FD1: the child finishes inside the mana-colour handler, which drains
/// directly; the activation is still placed in that same action.
#[test]
fn a_mana_choice_child_finishes_and_places_the_activation_in_the_same_action() {
    let (mut runner, bottle, life_before) = bottle_at_mana_choice(replacement_board());
    assert!(
        parked_child_rider(runner.state()),
        "reach: the rider is parked"
    );

    let events = choose_green(&mut runner);
    assert_eq!(pool(&runner, ManaType::Green), 1);
    assert_eq!(runner.life(P0) - life_before, -1);
    let loss = event_index(&events, is_life_loss);
    let activated = event_index(&events, is_activation_of(bottle));
    assert!(loss < activated, "rider {loss} < activation {activated}");
    assert_eq!(runner.state().stack.len(), 1, "placed in the same action");
    assert_eq!(top_stack_count(&runner), Some(3));
    assert!(!parked_child_rider(runner.state()));
    assert!(payment_ownership_is_clear(&runner));

    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!(runner.life(P0) - life_before, 5);
}

/// Synthetic control: a trigger the child's life loss raises.
const WITNESS: &str = "Whenever you lose life, you gain 1 life.";

fn witness_entries(state: &GameState, witness: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                && entry.source_id == witness
        })
        .count()
}

fn stack_pushes(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackPushed { .. }))
        .count()
}

/// The stack is [the Bottle's activation reading 3, one trigger from `source`].
fn assert_activation_then_one_trigger(runner: &GameRunner, bottle: ObjectId, source: ObjectId) {
    let stack = &runner.state().stack;
    assert_eq!(
        stack.len(),
        2,
        "the activation and one trigger: {:?}",
        stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>()
    );
    match &stack[0].kind {
        StackEntryKind::ActivatedAbility { ability, .. } => {
            assert_eq!(stack[0].source_id, bottle);
            assert_eq!(ability.context.cost_removed_counter_amount, Some(3));
        }
        other => panic!("the activation is at the bottom: {other:?}"),
    }
    assert!(
        matches!(stack[1].kind, StackEntryKind::TriggeredAbility { .. })
            && stack[1].source_id == source,
        "the trigger is on top: {:?}",
        stack[1].kind
    );
}

/// F1: a trigger raised by the child's payment waits in the activation's
/// announcement and goes on the stack once, above the activation (CR 603.3).
#[test]
fn a_trigger_from_the_childs_payment_goes_on_the_stack_above_the_activation() {
    let mut scenario = replacement_board();
    let witness = scenario
        .add_artifact_from_oracle(P0, "Witness", WITNESS)
        .id();
    let (mut runner, bottle, _) = bottle_at_mana_choice(scenario);
    assert_eq!(
        runner.state().objects[&witness].trigger_definitions.len(),
        1,
        "reach: Witness has its trigger"
    );

    let events = choose_green(&mut runner);
    assert_activation_then_one_trigger(&runner, bottle, witness);
    assert_eq!(
        witness_entries(runner.state(), witness),
        1,
        "collected once"
    );
    assert_eq!(stack_pushes(&events), 2, "one StackPushed per entry");
}

/// C4: the T12 child under Saprazzan Cove's mana root paying Divination. The
/// child's life loss happens during the cast, so Witness's trigger waits for
/// the cast and is collected once, with it (CR 601.2i + CR 603.3).
#[test]
fn a_trigger_from_a_child_under_a_mana_root_is_collected_with_the_cast() {
    let mut scenario = replacement_board();
    let witness = scenario
        .add_artifact_from_oracle(P0, "Witness", WITNESS)
        .id();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(mana_choice_child())),
    );
    let card_id = runner.state().objects[&divination].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: divination,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap_or_else(|err| panic!("Divination is cast: {err:?}"));
    assert!(
        is_mana_payment(runner.state()),
        "reach: Divination waits for mana"
    );
    let index = counter_cost_ability(&runner, cove, 0);
    activate(&mut runner, cove, index);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        &mut Answers::pay(2),
    );
    choose_replacement(&mut runner, ACCEPT);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor {
                context: ManaChoiceContext::ResolvingEffect(_),
                ..
            }
        ),
        "reach: the child asks for its colour: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_cast.is_some(),
        "reach: Divination waits"
    );

    choose_green(&mut runner);
    assert!(
        is_mana_payment(runner.state()),
        "Divination still waits for mana: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![divination],
        "only the spell being cast is on the stack"
    );
    assert_eq!(
        witness_entries(runner.state(), witness),
        0,
        "nothing triggers mid-cast"
    );
    assert_eq!(pool(&runner, ManaType::Blue), 2);
    assert_eq!(pool(&runner, ManaType::Green), 1);

    runner
        .act(GameAction::PassPriority)
        .expect("the payment completes");
    let stack = &runner.state().stack;
    assert_eq!(
        stack.len(),
        2,
        "Divination and one trigger: {:?}",
        stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(stack[0].id, divination, "Divination is below the trigger");
    assert_eq!(
        witness_entries(runner.state(), witness),
        1,
        "collected once"
    );
    assert_eq!(stack[1].source_id, witness);
}

/// Synthetic control: a creature whose death triggers.
const DIES_GAIN: &str = "When this creature dies, you gain 1 life.";

/// Site 1 (C5): the child marks lethal damage, then asks for a colour (and
/// loses 1 life, the rider that retires the child). The creature dies to the
/// state-based actions checked after the activation is on the stack (CR 704.3 +
/// CR 117.5), so its trigger goes above the activation.
#[test]
fn an_sba_trigger_from_the_childs_payment_goes_above_the_activation() {
    let mut scenario = replacement_board();
    let victim = scenario
        .add_creature_from_oracle(P0, "Victim", 1, 1, DIES_GAIN)
        .id();
    let child = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DamageAll {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SpecificObject { id: victim },
            player_filter: None,
            damage_source: None,
        },
    )
    .sub_ability(mana_choice_child());
    let (mut runner, bottle, _) = bottle_at_child_mana_choice(scenario, child);
    assert_eq!(
        damage(&runner, victim),
        1,
        "reach: the Victim has lethal damage marked"
    );
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);

    choose_green(&mut runner);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_activation_then_one_trigger(&runner, bottle, victim);
}

/// Whether the resolution stack holds exactly one continuation, and it is the
/// `player_scope` leg bound to P1 (CR 608.2e: P1's own "unless" choice).
fn p1_leg_is_the_only_continuation(state: &GameState) -> bool {
    let legs: Vec<_> = state
        .resolution_stack
        .iter()
        .filter_map(|frame| match frame {
            ResolutionFrame::AbilityContinuation(continuation) => Some(continuation),
            _ => None,
        })
        .collect();
    legs.len() == 1
        && matches!(legs[0].pending.chain.effect, Effect::LoseLife { .. })
        && legs[0].pending.chain.controller == P1
}

/// A resolving "each player loses 1 life unless that player pays {2}" asks P0
/// first (CR 101.4, APNAP) with P1's leg parked; P0 activates the Cove to pay,
/// and the Cove's replacement runs `child`. When the child finishes, the Cove
/// resumes and P0's prompt is offered again with P1's leg still parked; P0 then
/// declines and P1 is asked (CR 608.2e + CR 118.12a).
fn outer_player_scope_seat_waits_for_the_mana_root(child: fn(ObjectId) -> AbilityDefinition) {
    let mut scenario = replacement_board();
    let taxer = scenario.add_artifact_from_oracle(P0, "Taxer", "").id();
    let cove = scenario
        .add_land_from_oracle(P0, "Saprazzan Cove", SAPRAZZAN_COVE)
        .id();
    scenario.with_counter(cove, counter("storage"), 3);
    let battery = add_battery(&mut scenario);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach: P0's seat is first"
    );
    // "Each player loses 1 life unless that player pays {2}": the unless payer
    // is the scoped seat, as the parser emits it for that card class.
    let mut each_player = lose(1).unless_pay(UnlessPayModifier {
        cost: AbilityCost::Mana {
            cost: ManaCost::generic(2),
        },
        payer: TargetFilter::ScopedPlayer,
    });
    each_player.player_scope = Some(PlayerFilter::All);
    install_taxer_ability(&mut runner, taxer, each_player);
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(ReplacementEvent::RemoveCounter, Some(child(battery))),
    );
    activate(&mut runner, taxer, 0);
    drive(
        &mut runner,
        |state| matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
        &mut Answers::default(),
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { player, .. } if player == P0),
        "reach: P0's seat asks first: {:?}",
        runner.state().waiting_for
    );
    assert!(
        p1_leg_is_the_only_continuation(runner.state()),
        "reach: P1's leg is parked as the only continuation"
    );
    let p1_life = runner.life(P1);

    cove_to_first_child_entry(&mut runner, cove);
    assert!(cove_removal_enclosing(&runner).is_none());
    choose_replacement(&mut runner, DECLINE);
    assert_mana_root_waits_with_child_rider(&runner);

    let events = choose_replacement_events(&mut runner, DECLINE);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { player, .. } if player == P0),
        "P0's unless payment is offered again: {:?}",
        runner.state().waiting_for
    );
    assert!(
        p1_leg_is_the_only_continuation(runner.state()),
        "P1's leg is still parked, and nothing else is"
    );
    assert_eq!(runner.life(P1), p1_life, "P1's leg has not run");
    assert!(events.iter().any(is_cove_mana(cove)));

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P0 declines");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::UnlessPayment { player, pending_effect, .. }
                if *player == P1
                    && matches!(pending_effect.effect, Effect::LoseLife { .. })
                    && pending_effect.controller == P1
        ),
        "P1 is asked next, for P1's leg: {:?}",
        runner.state().waiting_for
    );
}

/// F3: the child's rider draws, and the draw's completion must not lift P1's
/// outer leg above the paused drain of the Cove's subtree.
#[test]
fn an_outer_player_scope_seat_waits_for_the_mana_root() {
    outer_player_scope_seat_waits_for_the_mana_root(t10_child_execute);
}

/// Gate (a): the same with a life-gain rider, which raises no draw: once the
/// child's drain retires, the recursive continuation drain must stop at the
/// due root instead of running P1's leg.
#[test]
fn an_outer_player_scope_seat_waits_for_the_mana_root_after_a_life_gain_rider() {
    outer_player_scope_seat_waits_for_the_mana_root(t3_child_execute);
}

/// F2: a rider-free "add one mana of any color" child. Nothing drains after the
/// mana, so the child's drain is left paused; the action boundary still
/// finishes it and places the activation in the same action.
#[test]
fn a_rider_free_mana_choice_child_places_the_activation_in_the_same_action() {
    let mut child = mana_choice_child();
    child.sub_ability = None;
    let (mut runner, _, _) = bottle_at_child_mana_choice(replacement_board(), child);

    choose_green(&mut runner);
    assert_eq!(runner.state().stack.len(), 1, "placed in the same action");
    assert_eq!(top_stack_count(&runner), Some(3));
    assert_eq!(pool(&runner, ManaType::Green), 1);
    assert!(payment_ownership_is_clear(&runner));
    assert!(
        !runner
            .state()
            .resolution_stack
            .iter()
            .any(|frame| matches!(frame, ResolutionFrame::PostReplacement(_))),
        "the child's drain retired"
    );
}

/// F1: "Scry 1. You lose 1 life. Scry 1." as the child. The life loss happens
/// while the second Scry waits, so it is parked by the Scry handler under the
/// live announcement; Witness's trigger goes on the stack once, above the
/// activation.
#[test]
fn a_trigger_between_two_scry_children_goes_on_the_stack_once_above_the_activation() {
    let mut scenario = replacement_board();
    let witness = scenario
        .add_artifact_from_oracle(P0, "Witness", WITNESS)
        .id();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::RemoveCounter,
            Some(scry_one().sub_ability(lose(1).sub_ability(scry_one()))),
        ),
    );
    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    choose_replacement(&mut runner, ACCEPT);
    for scry in ["first", "second"] {
        let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
            panic!(
                "reach: the {scry} Scry asks: {:?}",
                runner.state().waiting_for
            );
        };
        assert_root_is_counter_cost(&runner);
        assert!(
            runner.state().stack.is_empty(),
            "nothing is on the stack yet"
        );
        runner
            .act(GameAction::SelectCards { cards })
            .unwrap_or_else(|err| panic!("the {scry} Scry is answered: {err:?}"));
    }
    assert_activation_then_one_trigger(&runner, bottle, witness);
    assert_eq!(
        witness_entries(runner.state(), witness),
        1,
        "collected once"
    );
}

/// Synthetic control: an observer of discards.
const DISCARD_WITNESS: &str = "Whenever a player discards a card, you gain 1 life.";

/// R4-F1: a "discard a card" child asks which card; the discard's trigger goes
/// on the stack once, above the activation, although the completion resumes
/// the root (whose commit collects it) before its own observer collector runs.
#[test]
fn a_discard_trigger_from_a_discard_child_goes_on_the_stack_once_above_the_activation() {
    let mut scenario = replacement_board();
    let witness = scenario
        .add_artifact_from_oracle(P0, "Witness", DISCARD_WITNESS)
        .id();
    let first = scenario.add_card_to_hand(P0, "Forest");
    scenario.add_card_to_hand(P0, "Island");
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::RemoveCounter,
            Some(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                    selection: CardSelectionMode::Chosen,
                    unless_filter: None,
                    filter: None,
                },
            )),
        ),
    );
    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    choose_replacement(&mut runner, ACCEPT);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::DiscardChoice { cards, .. } if cards.contains(&first)),
        "reach: the child asks which card: {:?}",
        runner.state().waiting_for
    );
    assert_root_is_counter_cost(&runner);

    runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("Forest is discarded");
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_activation_then_one_trigger(&runner, bottle, witness);
    assert_eq!(
        witness_entries(runner.state(), witness),
        1,
        "collected once"
    );
}

/// Synthetic control: an observer of creatures dying.
const DIES_WITNESS: &str = "Whenever a creature you control dies, you gain 1 life.";

/// F5: a "sacrifice a creature" child asks which creature; the sacrifice's
/// dies trigger goes on the stack once, above the activation.
#[test]
fn a_dies_trigger_from_a_sacrifice_child_goes_on_the_stack_once_above_the_activation() {
    let mut scenario = replacement_board();
    let witness = scenario
        .add_artifact_from_oracle(P0, "Witness", DIES_WITNESS)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let bottle = scenario
        .add_artifact_from_oracle(P0, "Essence Bottle", ESSENCE_BOTTLE)
        .id();
    scenario.with_counter(bottle, counter("elixir"), 3);
    let mut runner = scenario.build();
    install_replacement_on_steward(
        &mut runner,
        optional_replacement(
            ReplacementEvent::RemoveCounter,
            Some(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Sacrifice {
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 1,
                },
            )),
        ),
    );
    let index = counter_cost_ability(&runner, bottle, 0);
    activate(&mut runner, bottle, index);
    choose_replacement(&mut runner, ACCEPT);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::EffectZoneChoice { cards, .. } if cards.contains(&fodder)),
        "reach: the child asks which creature: {:?}",
        runner.state().waiting_for
    );
    assert_root_is_counter_cost(&runner);

    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("Fodder is sacrificed");
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_activation_then_one_trigger(&runner, bottle, witness);
    assert_eq!(
        witness_entries(runner.state(), witness),
        1,
        "collected once"
    );
}

// ---------------------------------------------------------------------------
// C4 — hostiles: the amount belongs to its own activation
// ---------------------------------------------------------------------------

/// C4a: two Vish Kal activations stacked; the top one removed 0 counters and
/// shrinks nothing, the bottom one removed 3.
#[test]
fn stacked_activations_each_read_their_own_removed_count() {
    let mut scenario = main_phase();
    let vish = scenario
        .add_creature_from_oracle(P0, "Vish Kal, Blood Arbiter", 5, 5, VISH_KAL)
        .id();
    scenario.with_counter(vish, CounterType::Plus1Plus1, 3);
    let target = add_victim(&mut scenario, "Target", 5, 5);
    let mut runner = scenario.build();
    let index = counter_cost_ability(&runner, vish, 0);

    activate(&mut runner, vish, index);
    drive(
        &mut runner,
        priority_with_stack(1),
        &mut Answers::default().object(target),
    );
    activate(&mut runner, vish, index);
    drive(
        &mut runner,
        priority_with_stack(2),
        &mut Answers::default().object(target),
    );
    assert_eq!(counters_on(&runner, vish, &CounterType::Plus1Plus1), 0);

    drive(&mut runner, priority_with_stack(1), &mut Answers::default());
    assert_eq!(
        (power(&runner, target), toughness(&runner, target)),
        (5, 5),
        "the second activation removed no counters"
    );
    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );
    assert_eq!((power(&runner, target), toughness(&runner, target)), (2, 2));
}

/// C4b: a remove-all activation with no counters reads 0, not the previous 3.
#[test]
fn a_later_activation_with_no_counters_reads_zero_not_a_stale_amount() {
    let mut scenario = main_phase();
    let amulet = scenario
        .add_artifact_from_oracle(P0, "Relic Amulet", RELIC_AMULET)
        .id();
    scenario.with_counter(amulet, counter("charge"), 3);
    let first = add_victim(&mut scenario, "First Victim", 1, 5);
    let second = add_victim(&mut scenario, "Second Victim", 1, 5);
    add_mana(&mut scenario, &colorless(4));
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, amulet, 0, Answers::default().object(first));
    assert_eq!(
        damage(&runner, first),
        3,
        "reach: the first activation dealt 3"
    );

    runner.state_mut().objects.get_mut(&amulet).unwrap().tapped = false;
    activate_and_resolve(&mut runner, amulet, 0, Answers::default().object(second));
    assert_eq!(damage(&runner, second), 0);
}

/// C4d: Geistflame Reservoir, chosen count 2 of 3 — "that much" is 2.
#[test]
fn geistflame_reservoir_chosen_count_reads_that_much() {
    let mut scenario = main_phase();
    let reservoir = scenario
        .add_artifact_from_oracle(P0, "Geistflame Reservoir", GEISTFLAME_RESERVOIR)
        .id();
    scenario.with_counter(reservoir, counter("charge"), 3);
    let victim = add_victim(&mut scenario, "Victim", 1, 10);
    add_mana(&mut scenario, &[ManaType::Colorless, ManaType::Red]);
    let mut runner = scenario.build();

    activate_and_resolve(
        &mut runner,
        reservoir,
        0,
        Answers {
            pay_amount: Some(2),
            ..Answers::x(2)
        }
        .object(victim),
    );

    assert_eq!(counters_on(&runner, reservoir, &counter("charge")), 1);
    assert_eq!(damage(&runner, victim), 2);
}

/// C4d: The Astonishing Ant-Man, chosen count 2 — "that many" Insects.
#[test]
fn ant_man_chosen_count_creates_that_many_insects() {
    let mut scenario = main_phase();
    let ant_man = scenario
        .add_creature_from_oracle(P0, "The Astonishing Ant-Man", 1, 1, ANT_MAN)
        .id();
    scenario.with_counter(ant_man, CounterType::Plus1Plus1, 3);
    add_mana(
        &mut scenario,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Green],
    );
    let mut runner = scenario.build();

    activate_and_resolve(
        &mut runner,
        ant_man,
        0,
        Answers {
            pay_amount: Some(2),
            ..Answers::x(2)
        },
    );

    assert_eq!(counters_on(&runner, ant_man, &CounterType::Plus1Plus1), 1);
    assert_eq!(battlefield_named(&runner, "Insect").len(), 2);
}

/// C4d + C4c: Rasputin, chosen count 2 — "that much {C}" is {C}{C}.
#[test]
fn rasputin_chosen_count_adds_that_much_colorless() {
    let mut scenario = main_phase();
    let rasputin = scenario
        .add_creature_from_oracle(P0, "Rasputin, the Oneiromancer", 4, 1, RASPUTIN)
        .id();
    scenario.with_counter(rasputin, counter("dream"), 3);
    let mut runner = scenario.build();

    activate_and_resolve(
        &mut runner,
        rasputin,
        0,
        Answers {
            pay_amount: Some(2),
            ..Answers::x(2)
        },
    );

    assert_eq!(counters_on(&runner, rasputin, &counter("dream")), 1);
    assert_eq!(pool(&runner, ManaType::Colorless), 2);
    assert!(
        payment_ownership_is_clear(&runner),
        "C4c: no payment state outlives the mana ability"
    );
}

// ---------------------------------------------------------------------------
// C5 — siblings: no counter cost / no reader
// ---------------------------------------------------------------------------

/// C5a: counters removed by an EFFECT keep reading the effect's own count.
#[test]
fn effect_path_removal_still_reads_its_own_count() {
    let mut scenario = main_phase();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Control Relic", EFFECT_PATH_CONTROL)
        .id();
    scenario.with_counter(relic, counter("charge"), 3);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    activate(&mut runner, relic, 0);
    drive(
        &mut runner,
        priority_with_empty_stack,
        &mut Answers::default(),
    );

    assert_eq!(counters_on(&runner, relic, &counter("charge")), 0);
    assert_eq!(runner.life(P0) - life_before, 3);
    assert!(payment_ownership_is_clear(&runner));
}

/// C5b: a fixed counter cost with no reader creates exactly one Knight.
#[test]
fn rasputin_fixed_counter_cost_creates_one_knight() {
    let mut scenario = main_phase();
    let rasputin = scenario
        .add_creature_from_oracle(P0, "Rasputin, the Oneiromancer", 4, 1, RASPUTIN)
        .id();
    scenario.with_counter(rasputin, counter("dream"), 3);
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, rasputin, 1, Answers::default());

    assert_eq!(counters_on(&runner, rasputin, &counter("dream")), 2);
    assert_eq!(battlefield_named(&runner, "Knight").len(), 1);
}

// ---------------------------------------------------------------------------
// C6 — Sage of Hours (CR 107.1 + CR 500.7)
// ---------------------------------------------------------------------------

fn sage_extra_turns(counters: u32) -> usize {
    let mut scenario = main_phase();
    let sage = scenario
        .add_creature_from_oracle(P0, "Sage of Hours", 1, 1, SAGE_OF_HOURS)
        .id();
    scenario.with_counter(sage, CounterType::Plus1Plus1, counters);
    let mut runner = scenario.build();

    activate_and_resolve(&mut runner, sage, 0, Answers::default());

    assert_eq!(counters_on(&runner, sage, &CounterType::Plus1Plus1), 0);
    runner.state().extra_turns.len()
}

/// C6: one extra turn for each complete group of five counters removed.
#[test]
fn sage_of_hours_takes_an_extra_turn_per_five_counters_removed() {
    assert_eq!(sage_extra_turns(5), 1);
    assert_eq!(sage_extra_turns(4), 0);
    assert_eq!(sage_extra_turns(12), 2);
}

// ---------------------------------------------------------------------------
// C7 — copies (CR 707.10)
// ---------------------------------------------------------------------------

/// C7a: Relic Amulet copied by Rings of Brighthearth — the copy deals the
/// original's 3, then the original deals 3 (6 total).
///
/// A remove-ALL cost involves no player choice, so the copier rulings' "the
/// copy uses that same information" does not literally name it. The copy still
/// reads 3: CR 707.10 copies "all decisions made for it, including … additional
/// or alternative costs", and the number of counters removed is fixed when the
/// cost is paid during activation (CR 602.2b + CR 601.2h), not on resolution, so
/// it is not a "choice normally made on resolution" that the copy re-makes. The
/// copy cannot pay the cost again (it isn't activated, CR 707.10), so its "that
/// much" is the amount bound to the activation it copies — the same reading
/// Lithoform Engine's ruling gives a copied spell's paid costs ("effects based on
/// any alternative or additional costs that were paid for the original spell are
/// copied as though those same costs were paid for the copy").
#[test]
fn rings_copy_of_relic_amulet_deals_the_originals_amount() {
    let mut scenario = main_phase();
    let rings = scenario
        .add_artifact_from_oracle(P0, "Rings of Brighthearth", RINGS_OF_BRIGHTHEARTH)
        .id();
    let amulet = scenario
        .add_artifact_from_oracle(P0, "Relic Amulet", RELIC_AMULET)
        .id();
    scenario.with_counter(amulet, counter("charge"), 3);
    let victim = add_victim(&mut scenario, "Victim", 1, 10);
    add_mana(&mut scenario, &colorless(4));
    let mut runner = scenario.build();
    let mut answers = Answers {
        copier_last: Some(rings),
        ..Answers::default()
    }
    .object(victim);

    let index = counter_cost_ability(&runner, amulet, 0);
    activate(&mut runner, amulet, index);
    drive(&mut runner, priority_with_no_trigger_waiting, &mut answers);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach: the original and its copy wait on the stack"
    );
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert_eq!(
        damage(&runner, victim),
        3,
        "the copy deals the original's 3"
    );
    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_eq!(damage(&runner, victim), 6, "the original deals 3 more");
}

/// C7b: Geistflame Reservoir (2 of 3) copied by Rings — the copy's "that much"
/// uses the original's chosen count (CR 707.10; the copier rulings' "a number of
/// counters to remove, the copy uses that same information"): 2, then 2 more.
#[test]
fn rings_copy_of_geistflame_reservoir_deals_the_chosen_amount() {
    let mut scenario = main_phase();
    let rings = scenario
        .add_artifact_from_oracle(P0, "Rings of Brighthearth", RINGS_OF_BRIGHTHEARTH)
        .id();
    let reservoir = scenario
        .add_artifact_from_oracle(P0, "Geistflame Reservoir", GEISTFLAME_RESERVOIR)
        .id();
    scenario.with_counter(reservoir, counter("charge"), 3);
    let victim = add_victim(&mut scenario, "Victim", 1, 10);
    let mut mana = colorless(3);
    mana.push(ManaType::Red);
    add_mana(&mut scenario, &mana);
    let mut runner = scenario.build();
    let mut answers = Answers {
        copier_last: Some(rings),
        pay_amount: Some(2),
        ..Answers::x(2)
    }
    .object(victim);

    let index = counter_cost_ability(&runner, reservoir, 0);
    activate(&mut runner, reservoir, index);
    drive(&mut runner, priority_with_no_trigger_waiting, &mut answers);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach: the original and its copy wait on the stack"
    );
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert_eq!(
        damage(&runner, victim),
        2,
        "the copy deals the original's 2"
    );
    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_eq!(damage(&runner, victim), 4, "the original deals 2 more");
    assert_eq!(counters_on(&runner, reservoir, &counter("charge")), 1);
}

fn baku_with_bracers(oracle: &str) -> (GameRunner, ObjectId) {
    let mut scenario = main_phase();
    let bracers = scenario
        .add_creature(P0, "Illusionist's Bracers", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(ILLUSIONISTS_BRACERS)
        .id();
    let baku = scenario
        .add_creature_from_oracle(P0, "Blademane Baku", 1, 1, oracle)
        .id();
    scenario.with_counter(baku, counter("ki"), 3);
    add_mana(&mut scenario, &colorless(1));
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), bracers, baku);
    (runner, baku)
}

/// C7c: Blademane Baku (X = 2) copied by Illusionist's Bracers — the copy uses
/// the original's 2 counters removed (CR 707.10; the copier rulings) and gives
/// +4/+0, then the original gives +4/+0: power 1 → 5 → 9, and only 2 ki counters
/// leave (the copy pays no cost).
#[test]
fn bracers_copy_of_blademane_baku_uses_the_originals_count() {
    let (mut runner, baku) = baku_with_bracers(BLADEMANE_BAKU);
    let mut answers = Answers::x(2);

    let index = counter_cost_ability(&runner, baku, 0);
    activate(&mut runner, baku, index);
    drive(&mut runner, priority_with_no_trigger_waiting, &mut answers);
    assert_eq!(runner.state().stack.len(), 2, "reach: original and copy");
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert_eq!(power(&runner, baku), 5, "the copy gives +4/+0");
    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_eq!(power(&runner, baku), 9, "the original gives +4/+0 more");
    assert_eq!(counters_on(&runner, baku, &counter("ki")), 1);
}

/// C7d: a reader of X itself on a copy still reads the copied X (+2 each).
#[test]
fn bracers_copy_of_an_x_reader_keeps_x() {
    let (mut runner, baku) = baku_with_bracers(X_READER);
    let mut answers = Answers::x(2);

    let index = counter_cost_ability(&runner, baku, 0);
    activate(&mut runner, baku, index);
    drive(&mut runner, priority_with_no_trigger_waiting, &mut answers);
    assert_eq!(runner.state().stack.len(), 2, "reach: original and copy");
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert_eq!(power(&runner, baku), 3, "the copy gives +X/+0");
    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_eq!(power(&runner, baku), 5);
}

// ---------------------------------------------------------------------------
// C8 — the colour prompt reads the paid count (CR 605.3b + CR 608.2d)
// ---------------------------------------------------------------------------
//
// The rows below use a synthetic typed witness derived from Pentad Prism. It is
// NOT printed text: no printed card pairs a counter-removal mana cost with a
// colour prompt whose size or gate reads the counters removed, but the class
// is reachable by data. The witness is an artifact whose one activated mana
// ability removes its own charge counters and adds mana counted by
// `PreviousEffectAmount`, the "counters removed this way" / "that much" amount.
//
// CR 605.3b + CR 602.2b + CR 608.2h: the mana ability resolves immediately, so
// the colour prompt it offers (CR 608.2d) is sized by this activation's own
// paid count, exactly as its production is. The enclosing "this way" amount
// (`last_effect_amount`) neither sizes the prompt nor is changed by it.

/// Pentad Prism's self counter cost ("Remove a charge counter from this
/// artifact"), with `count` charge counters, or `REMOVE_COUNTER_COST_ALL` for
/// "Remove all charge counters from this artifact".
fn remove_charge_counters(count: u32) -> AbilityCost {
    AbilityCost::RemoveCounter {
        count,
        counter_type: CounterMatch::OfType(counter("charge")),
        target: None,
        selection: CounterCostSelection::SingleObject,
    }
}

fn mana_effect(produced: ManaProduction) -> Effect {
    Effect::Mana {
        produced,
        restrictions: vec![],
        grants: vec![],
        expiry: None,
        target: None,
    }
}

/// "Add that much mana in any combination of {R} and/or {G}."
fn red_green_combination() -> ManaProduction {
    ManaProduction::AnyCombination {
        count: previous_effect_amount(),
        color_options: vec![ManaColor::Red, ManaColor::Green],
    }
}

/// The synthetic witness: an artifact with `charge` charge counters whose only
/// ability is the activated mana ability `cost: <ability>`.
fn add_charge_font(
    scenario: &mut GameScenario,
    cost: AbilityCost,
    ability: AbilityDefinition,
    charge: u32,
) -> ObjectId {
    let font = scenario
        .add_artifact_from_oracle(P0, "Charge Font", "")
        .with_ability_definition(ability.cost(cost))
        .id();
    scenario.with_counter(font, counter("charge"), charge);
    font
}

fn mana_ability(produced: ManaProduction) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Activated, mana_effect(produced))
}

/// Seeds `outer` as the enclosing "this way" amount, activates `source`'s mana
/// ability, and returns the colour prompt it surfaced, if any.
fn activate_with_outer_amount(
    runner: &mut GameRunner,
    source: ObjectId,
    outer: Option<i32>,
) -> Option<ManaChoicePrompt> {
    runner.state_mut().last_effect_amount = outer;
    activate(runner, source, 0);
    assert_eq!(
        runner.state().last_effect_amount,
        outer,
        "the enclosing amount is unchanged when the activation returns"
    );
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor { choice, .. } => Some(choice.clone()),
        _ => None,
    }
}

/// The prompt offers `answer`'s shape: a single colour among its options, or a
/// combination of `paid` units.
fn assert_prompt_offers(
    prompt: &Option<ManaChoicePrompt>,
    answer: &ManaChoice,
    paid: usize,
    row: &str,
) {
    match (prompt, answer) {
        (Some(ManaChoicePrompt::SingleColor { options }), ManaChoice::SingleColor(color)) => {
            assert!(
                options.contains(color),
                "{row}: {color:?} is among the offered colours {options:?}"
            );
        }
        (
            Some(ManaChoicePrompt::AnyCombination { count, options }),
            ManaChoice::Combination(combination),
        ) => {
            assert_eq!(
                *count, paid,
                "{row}: the combination is sized by the {paid} counters removed"
            );
            assert!(
                combination.iter().all(|unit| options.contains(unit)),
                "{row}: {combination:?} is drawn from {options:?}"
            );
        }
        (other, _) => panic!(
            "{row}: a prompt for {answer:?}, sized by the {paid} counters removed, \
             is surfaced; got {other:?}"
        ),
    }
}

fn choose_mana(runner: &mut GameRunner, choice: ManaChoice, row: &str) {
    runner
        .act(GameAction::ChooseManaColor { choice, count: 1 })
        .unwrap_or_else(|err| panic!("{row}: the answer is legal: {err:?}"));
}

/// Exactly `paid` mana reached the pool, as `answer` chose it.
fn assert_pool_holds(runner: &GameRunner, answer: &ManaChoice, paid: usize, row: &str) {
    let chosen = match answer {
        ManaChoice::SingleColor(color) => vec![*color; paid],
        ManaChoice::Combination(combination) => combination.clone(),
    };
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        paid,
        "{row}: exactly the {paid} paid units are added"
    );
    for color in &chosen {
        let expected = chosen.iter().filter(|unit| *unit == color).count();
        assert_eq!(
            pool(runner, *color),
            expected,
            "{row}: the pool holds the chosen {color:?}"
        );
    }
}

/// One count-sensitive `mana_choice_prompt` arm: its production, the board its
/// options read, and the answer the row gives.
struct PromptSibling {
    name: &'static str,
    produced: ManaProduction,
    /// Seats the permanents the production's options read.
    board: fn(&mut GameScenario),
    /// Gives the built source the state its options read.
    source: fn(&mut GameRunner, ObjectId),
    answer: ManaChoice,
}

fn no_board(_: &mut GameScenario) {}

fn no_source_state(_: &mut GameRunner, _: ObjectId) {}

fn add_colored_creature(scenario: &mut GameScenario, name: &str, color: ManaColor) {
    scenario
        .add_creature(P0, name, 1, 1)
        .with_color(vec![color]);
}

/// Every arm of `mana_choice_prompt` that reads the count through
/// `resolve_mana_types_for_ability`: the count-sized combination and the
/// single-colour arms whose prompt is gated on the production adding mana
/// (CR 106.5).
///
/// `AnyCombinationOfObjectColors` has no row: its options come from
/// `object_colors_for_scope`, which binds only an object TARGET, and a mana
/// ability requires no target (CR 605.1a), so the arm returns no prompt before
/// it reads the count — it cannot be reached from a mana ability's prompt.
fn prompt_siblings() -> Vec<PromptSibling> {
    let you_control =
        |filter: TypedFilter| TargetFilter::Typed(filter.controller(ControllerRef::You));
    vec![
        PromptSibling {
            name: "AnyCombination",
            produced: red_green_combination(),
            board: no_board,
            source: no_source_state,
            answer: ManaChoice::Combination(vec![ManaType::Red, ManaType::Green]),
        },
        PromptSibling {
            name: "AnyOneColor",
            produced: ManaProduction::AnyOneColor {
                count: previous_effect_amount(),
                color_options: vec![ManaColor::Red, ManaColor::Green],
                contribution: ManaContribution::Base,
            },
            board: no_board,
            source: no_source_state,
            answer: ManaChoice::SingleColor(ManaType::Green),
        },
        PromptSibling {
            name: "AnyOneColorAmongPermanents",
            produced: ManaProduction::AnyOneColorAmongPermanents {
                count: previous_effect_amount(),
                filter: you_control(TypedFilter::creature()),
                contribution: ManaContribution::Base,
            },
            board: |scenario| {
                add_colored_creature(scenario, "Red Creature", ManaColor::Red);
                add_colored_creature(scenario, "Green Creature", ManaColor::Green);
            },
            source: no_source_state,
            answer: ManaChoice::SingleColor(ManaType::Green),
        },
        PromptSibling {
            name: "ChosenColor with a fixed alternative",
            produced: ManaProduction::ChosenColor {
                count: previous_effect_amount(),
                contribution: ManaContribution::Base,
                fixed_alternative: Some(ManaColor::White),
            },
            board: no_board,
            source: |runner, font| {
                runner
                    .state_mut()
                    .objects
                    .get_mut(&font)
                    .unwrap()
                    .chosen_attributes
                    .push(ChosenAttribute::Color(ManaColor::Green));
            },
            answer: ManaChoice::SingleColor(ManaType::Green),
        },
        PromptSibling {
            name: "AnyTypeProduceableBy",
            produced: ManaProduction::AnyTypeProduceableBy {
                count: previous_effect_amount(),
                land_filter: you_control(TypedFilter::land()),
            },
            board: |scenario| {
                scenario.add_basic_land(P0, ManaColor::Blue);
                scenario.add_basic_land(P0, ManaColor::Green);
            },
            source: no_source_state,
            answer: ManaChoice::SingleColor(ManaType::Green),
        },
        PromptSibling {
            name: "OpponentLandColors",
            produced: ManaProduction::OpponentLandColors {
                count: previous_effect_amount(),
            },
            board: |scenario| {
                scenario.add_basic_land(P1, ManaColor::Red);
                scenario.add_basic_land(P1, ManaColor::Green);
            },
            source: no_source_state,
            answer: ManaChoice::SingleColor(ManaType::Green),
        },
        PromptSibling {
            name: "AnyInCommandersColorIdentity",
            produced: ManaProduction::AnyInCommandersColorIdentity {
                count: previous_effect_amount(),
                contribution: ManaContribution::Base,
            },
            board: |scenario| {
                scenario
                    .add_creature(P0, "Red-Green Commander", 2, 2)
                    .with_color(vec![ManaColor::Red, ManaColor::Green])
                    .commander();
            },
            source: no_source_state,
            answer: ManaChoice::SingleColor(ManaType::Green),
        },
    ]
}

/// R4 (B2): every count-sensitive colour prompt reads this activation's own
/// 2 counters removed. With no enclosing amount (or 0) the prompt is still
/// surfaced and sized by 2, and exactly the 2 chosen units reach the pool.
#[test]
fn count_sensitive_mana_prompts_read_the_paid_count() {
    for row in prompt_siblings() {
        for outer in [None, Some(0)] {
            let name = format!("{} (outer {outer:?})", row.name);
            let mut scenario = main_phase();
            (row.board)(&mut scenario);
            let font = add_charge_font(
                &mut scenario,
                remove_charge_counters(REMOVE_COUNTER_COST_ALL),
                mana_ability(row.produced.clone()),
                2,
            );
            let mut runner = scenario.build();
            (row.source)(&mut runner, font);

            let prompt = activate_with_outer_amount(&mut runner, font, outer);
            assert_eq!(
                counters_on(&runner, font, &counter("charge")),
                0,
                "{name}: reach: the cost removed both charge counters"
            );
            assert_prompt_offers(&prompt, &row.answer, 2, &name);

            choose_mana(&mut runner, row.answer.clone(), &name);
            assert_pool_holds(&runner, &row.answer, 2, &name);
            assert_eq!(
                runner.state().last_effect_amount,
                outer,
                "{name}: the enclosing amount is restored after production"
            );
            assert!(payment_ownership_is_clear(&runner), "{name}");
        }
    }
}

/// R4: Pentad Prism's one-counter cost with a combination production. Whatever
/// the enclosing amount — none, 0, or a stale 3 — the prompt asks for the 1
/// counter removed: a 3-unit answer is rejected and the 1-unit answer adds
/// exactly 1 mana.
#[test]
fn a_stale_enclosing_amount_does_not_size_the_combination_prompt() {
    let mut scenario = main_phase();
    let prism = scenario
        .add_artifact_from_oracle(P0, "Pentad Prism", PENTAD_PRISM)
        .id();
    let runner = scenario.build();
    let index = counter_cost_ability(&runner, prism, 0);
    assert_eq!(
        runner.state().objects[&prism].abilities[index].cost,
        Some(remove_charge_counters(1)),
        "structural guard: the witness cost is the cost Pentad Prism parses to"
    );

    for outer in [None, Some(0), Some(3)] {
        let name = format!("AnyCombination, paid 1 (outer {outer:?})");
        let mut scenario = main_phase();
        let font = add_charge_font(
            &mut scenario,
            remove_charge_counters(1),
            mana_ability(red_green_combination()),
            2,
        );
        let mut runner = scenario.build();

        let prompt = activate_with_outer_amount(&mut runner, font, outer);
        assert_eq!(
            counters_on(&runner, font, &counter("charge")),
            1,
            "{name}: reach: the cost removed one charge counter"
        );
        let answer = ManaChoice::Combination(vec![ManaType::Green]);
        assert_prompt_offers(&prompt, &answer, 1, &name);

        assert!(
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::Combination(vec![ManaType::Red; 3]),
                    count: 1,
                })
                .is_err(),
            "{name}: three units are not legal for one counter removed"
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseManaColor { .. }
            ),
            "{name}: the rejected answer leaves the prompt pending"
        );

        choose_mana(&mut runner, answer.clone(), &name);
        assert_pool_holds(&runner, &answer, 1, &name);
        assert_eq!(
            runner.state().last_effect_amount,
            outer,
            "{name}: the enclosing amount is restored after production"
        );
    }
}

/// R4 (B1): the head adds {C}; "instead", if 2 or more counters were removed
/// this way, it adds that much mana in any combination of {R} and/or {G}. The
/// instead condition reads this activation's own paid count (CR 608.2c), as the
/// stack route's resolution does — not the enclosing amount.
#[test]
fn an_instead_condition_reads_the_paid_count() {
    let instead_ability = || {
        let combination =
            mana_ability(red_green_combination()).condition(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::PreviousEffectAmount {
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 2 },
                    channel: DamageChannel::Total,
                }),
            });
        mana_ability(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 1 },
        })
        .sub_ability(combination)
    };

    // Paid 2, no enclosing amount: the instead production applies and asks for
    // 2 units.
    let name = "instead, paid 2 (outer None)";
    let mut scenario = main_phase();
    let font = add_charge_font(
        &mut scenario,
        remove_charge_counters(REMOVE_COUNTER_COST_ALL),
        instead_ability(),
        2,
    );
    let mut runner = scenario.build();
    let prompt = activate_with_outer_amount(&mut runner, font, None);
    assert_eq!(
        counters_on(&runner, font, &counter("charge")),
        0,
        "{name}: reach: the cost removed both charge counters"
    );
    let answer = ManaChoice::Combination(vec![ManaType::Red, ManaType::Green]);
    assert_prompt_offers(&prompt, &answer, 2, name);
    choose_mana(&mut runner, answer.clone(), name);
    assert_pool_holds(&runner, &answer, 2, name);
    assert_eq!(runner.state().last_effect_amount, None, "{name}");

    // Paid 1 against a stale enclosing 3: the condition is false, so {C} with
    // no prompt.
    let name = "instead, paid 1 (outer Some(3))";
    let mut scenario = main_phase();
    let font = add_charge_font(
        &mut scenario,
        remove_charge_counters(REMOVE_COUNTER_COST_ALL),
        instead_ability(),
        1,
    );
    let mut runner = scenario.build();
    let prompt = activate_with_outer_amount(&mut runner, font, Some(3));
    assert_eq!(
        counters_on(&runner, font, &counter("charge")),
        0,
        "{name}: reach: the cost removed the charge counter"
    );
    assert_eq!(prompt, None, "{name}: the head {{C}} needs no choice");
    assert_eq!(pool(&runner, ManaType::Colorless), 1, "{name}");
    assert_eq!(runner.state().players[0].mana_pool.total(), 1, "{name}");
    assert_eq!(runner.state().last_effect_amount, Some(3), "{name}");
}

/// R4 (B3): while Ooze Flux waits for its {1}{G} with its own 3 counters
/// removed (CR 601.2g + CR 605.3a), a counter-cost mana ability removing 1
/// counter asks for 1 unit, not the enclosing 3, and leaves both the enclosing
/// amount and Ooze Flux's pending count untouched.
#[test]
fn a_nested_mana_prompt_reads_its_own_paid_count() {
    let mut font = None;
    let (mut runner, flux, first, second) = ooze_flux_with_granted_mana_and(&[], |scenario| {
        font = Some(add_charge_font(
            scenario,
            remove_charge_counters(1),
            mana_ability(red_green_combination()),
            2,
        ));
    });
    let font = font.expect("the board seats the font");
    let mut answers = ooze_flux_answers(first, second);

    let index = counter_cost_ability(&runner, flux, 0);
    activate(&mut runner, flux, index);
    drive(&mut runner, is_mana_payment, &mut answers);
    assert_eq!(pending_activation_count(&runner), Some(3));

    let name = "nested AnyCombination, paid 1 (outer Some(3))";
    let prompt = activate_with_outer_amount(&mut runner, font, Some(3));
    assert_eq!(
        counters_on(&runner, font, &counter("charge")),
        1,
        "{name}: reach: the cost removed one charge counter"
    );
    let answer = ManaChoice::Combination(vec![ManaType::Green]);
    assert_prompt_offers(&prompt, &answer, 1, name);

    choose_mana(&mut runner, answer.clone(), name);
    assert_pool_holds(&runner, &answer, 1, name);
    assert_eq!(runner.state().last_effect_amount, Some(3), "{name}");
    assert!(
        is_mana_payment(runner.state()),
        "{name}: the enclosing payment resumes: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        pending_activation_count(&runner),
        Some(3),
        "{name}: Ooze Flux's pending count is unchanged"
    );

    // The rest of {1}{G}.
    let _ = runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    );
    drive(&mut runner, priority_with_stack(1), &mut answers);
    assert_eq!(top_stack_count(&runner), Some(3));
    drive(&mut runner, priority_with_empty_stack, &mut answers);
    assert_one_three_three_ooze(&runner, first, second);
}

/// R4 (B4): "{T}, Remove all charge counters" with none on it removes 0, and a
/// paid 0 is this activation's count, not an absent one. Against a stale
/// enclosing 3 the combination is sized by 0: no prompt and no mana.
#[test]
fn removing_zero_counters_adds_no_mana_against_a_stale_amount() {
    let mut scenario = main_phase();
    let font = add_charge_font(
        &mut scenario,
        AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                remove_charge_counters(REMOVE_COUNTER_COST_ALL),
            ],
        },
        mana_ability(red_green_combination()),
        0,
    );
    let mut runner = scenario.build();
    runner.state_mut().last_effect_amount = Some(3);

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: font,
            ability_index: 0,
        })
        .unwrap_or_else(|err| panic!("the ability activates: {err:?}"));

    assert!(
        runner.state().objects[&font].tapped,
        "reach: the {{T}} cost was paid"
    );
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated { source_id, .. } if *source_id == font
        )),
        "reach: the mana ability was activated"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no colour prompt for 0 counters removed: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().last_effect_amount, Some(3));
}

// ---------------------------------------------------------------------------
// P — parser shapes (U3)
// ---------------------------------------------------------------------------

fn parsed_json(oracle: &str) -> (String, String) {
    let mut scenario = main_phase();
    let id = scenario
        .add_creature_from_oracle(P0, "Shape Probe", 1, 1, oracle)
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&id];
    (
        serde_json::to_string(&*obj.abilities).unwrap(),
        format!("{:?}", obj.static_definitions.as_slice()),
    )
}

fn previous_effect_amount() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::PreviousEffectAmount {
            channel: DamageChannel::Total,
            aggregate: AggregateFunction::Sum,
        },
    }
}

/// P1: Sage of Hours repeats once per complete group of five counters removed.
#[test]
fn sage_of_hours_parses_to_groups_of_five() {
    let mut scenario = main_phase();
    let sage = scenario
        .add_creature_from_oracle(P0, "Sage of Hours", 1, 1, SAGE_OF_HOURS)
        .id();
    let runner = scenario.build();
    let def = &runner.state().objects[&sage].abilities[0];
    assert!(
        !serde_json::to_string(def)
            .unwrap()
            .contains("Unimplemented"),
        "reach: the ability is fully parsed"
    );
    assert_eq!(
        def.repeat_for,
        Some(QuantityExpr::DivideRounded {
            inner: Box::new(previous_effect_amount()),
            divisor: 5,
            rounding: RoundingMode::Down,
        })
    );
}

/// P2: "for each <N ≥ 2> …" never reads the bare count — an expression-level
/// consumer divides it, a reference-only consumer fails honestly.
#[test]
fn for_each_group_of_n_never_reads_the_bare_count() {
    // Reference-only consumers of "counters removed this way" with a group size.
    for oracle in [
        "{T}, Remove all +1/+1 counters from this creature: Add {C} for each two counters removed this way.",
        "{T}, Remove all +1/+1 counters from this creature: Create a 1/1 colorless Thopter artifact creature token with flying for each two counters removed this way.",
        "{T}, Remove all +1/+1 counters from this creature: Target creature gets -1/-1 until end of turn for each two +1/+1 counters removed this way.",
        "{T}, Remove all +1/+1 counters from this creature: You gain 1 life for each two counters removed this way.",
        "{T}: You gain 1 life for each two creatures you control.",
        "{T}: Draw a card for each two cards in your hand.",
        "{T}: For each two creatures you control, draw a card.",
    ] {
        let (abilities, _) = parsed_json(oracle);
        let divided = abilities.contains("\"DivideRounded\"") && abilities.contains("\"divisor\":2");
        let unsupported = abilities.contains("Unimplemented") || abilities == "[]";
        assert!(
            divided || unsupported,
            "{oracle}: neither divided nor unsupported: {abilities}"
        );
        assert!(
            divided || !abilities.contains("PreviousEffectAmount"),
            "{oracle}: reads the bare count: {abilities}"
        );
    }

    // "the number of five counters removed this way" is not a grouping
    // construct: the quantity declines honestly as an unparsed quantity (not a
    // bare count, a division, or another "the number of …" count).
    let (abilities, _) = parsed_json(
        "{T}, Remove all +1/+1 counters from this creature: It deals damage to any target equal to the number of five counters removed this way.",
    );
    let abilities_json: serde_json::Value = serde_json::from_str(&abilities).unwrap();
    assert_eq!(
        abilities_json.as_array().map(Vec::len),
        Some(1),
        "reach: THENUM5 parses to one ability: {abilities}"
    );
    assert_eq!(
        (
            &abilities_json[0]["effect"]["type"],
            &abilities_json[0]["effect"]["name"],
        ),
        (
            &serde_json::json!("Unimplemented"),
            &serde_json::json!("unparsed_quantity"),
        ),
        "THENUM5 declines honestly: {abilities}"
    );
    assert!(
        !abilities.contains("PreviousEffectAmount") && !abilities.contains("DivideRounded"),
        "THENUM5 reads no count: {abilities}"
    );

    // A static consumer divides: +1/+1 per complete pair of creatures.
    let (_, statics) = parsed_json("This creature gets +1/+1 for each two creatures you control.");
    let pairs = "value: DivideRounded { inner: Ref { qty: ObjectCount { filter: Typed(TypedFilter { type_filters: [Creature], controller: Some(You), properties: [] }) } }, divisor: 2, rounding: Down }";
    assert!(
        statics.contains(&format!(
            "modifications: [AddDynamicPower {{ {pairs} }}, AddDynamicToughness {{ {pairs} }}]"
        )),
        "STATIC2 divides on both power and toughness: {statics}"
    );

    // N = 1 is unchanged.
    let (abilities, _) =
        parsed_json("{T}: You gain 1 life for each 1 life your opponents have lost this turn.");
    assert!(
        !abilities.contains("Unimplemented") && !abilities.contains("DivideRounded"),
        "ONE_LIFE parses as before: {abilities}"
    );
}
