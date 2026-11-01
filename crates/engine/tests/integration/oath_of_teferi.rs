//! CR 606.3 + CR 611.3a: Oath of Teferi's loyalty-activation static.
//!
//! Verbatim Oracle line 2: "You may activate the loyalty abilities of
//! planeswalkers you control twice each turn rather than only once." It lowers
//! to the shared `StaticMode::ModifyActivationLimit { keyword: "loyalty",
//! new_limit: 2 }` mode, which the loyalty gate reads live (CR 611.3a) as the
//! per-permanent cap's static term: `cap = max(static limits..., 1) +
//! extra_grants`.
//!
//! Two card-data rulings shape the semantics pinned here:
//!   1. "you may activate the same ability of a planeswalker twice, or you may
//!      activate two different abilities of that planeswalker" — the cap is
//!      per-permanent across all loyalty indices.
//!   2. "If you somehow control more than one Oath of Teferi, you won't be able
//!      to activate abilities of planeswalkers you control more than twice in
//!      one turn." — multiple Oaths use max semantics, not additive.
//!
//! Loyalty abilities are activated, not cast, so these tests drive
//! `GameRunner::act(GameAction::ActivateAbility { .. })` at sorcery speed and
//! resolve each activation before the next `act` (CR 606.3 requires an empty
//! stack). Every negative carries its positive reach-guard in the same test.

use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityKind, ControllerRef, StaticDefinition, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Scryfall-verified Oracle text (Dominaria).
const OATH_OF_TEFERI: &str = "When Oath of Teferi enters, exile another target permanent you control. Return it to the battlefield under its owner's control at the beginning of the next end step.\nYou may activate the loyalty abilities of planeswalkers you control twice each turn rather than only once.";
const TEST_WALKER: &str = "+1: You gain 1 life.\n\u{2212}1: You gain 2 life.";
const DEEPGLOW_SKATE: &str = "When this creature enters, double the number of each kind of counter on any number of target permanents.";
const EIDOLON_OF_OBSTRUCTION: &str = "First strike\nLoyalty abilities of planeswalkers your opponents control cost {1} more to activate.";
const IMMORTAL_SUN: &str = "Players can't activate planeswalkers' loyalty abilities.\nAt the beginning of your draw step, draw an additional card.\nSpells you cast cost {1} less to cast.\nCreatures you control get +1/+1.";
/// Seam-test stand-ins carry their static constructed directly (Eidolon-test
/// style), so the Oracle face stays empty — the static under test must come
/// from exactly one source.
const NO_ORACLE_FACE: &str = "";

fn oath_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::White, ManaCostShard::Blue],
        generic: 3,
    }
}

fn mana_pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

/// The ability index of the loyalty ability with the given loyalty cost
/// amount (`+1` / `-1`), resolved by cost rather than position.
fn loyalty_index_by_amount(runner: &GameRunner, pw: ObjectId, amount: i32) -> usize {
    runner.state().objects[&pw]
        .abilities
        .iter()
        .position(|a| {
            matches!(a.kind, AbilityKind::Activated)
                && matches!(&a.cost, Some(AbilityCost::Loyalty { amount: a }) if *a == amount)
        })
        .unwrap_or_else(|| panic!("the test walker must have a {amount:+} loyalty ability"))
}

fn activate(
    runner: &mut GameRunner,
    pw: ObjectId,
    ability_index: usize,
) -> Result<engine::types::game_state::ActionResult, EngineError> {
    runner.act(GameAction::ActivateAbility {
        source_id: pw,
        ability_index,
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    })
}

#[track_caller]
fn assert_activation_refused(runner: &mut GameRunner, pw: ObjectId, ability_index: usize) {
    let result = activate(runner, pw, ability_index);
    assert!(
        matches!(result, Err(EngineError::ActionNotAllowed(_))),
        "activating ability {ability_index} must be refused, got {result:?}"
    );
}

fn loyalty_of(runner: &GameRunner, pw: ObjectId) -> u32 {
    runner.state().objects[&pw]
        .loyalty
        .expect("a planeswalker under test carries loyalty")
}

fn activations_of(runner: &GameRunner, pw: ObjectId) -> u32 {
    runner.state().objects[&pw].loyalty_activations_this_turn
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn pool_total(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .mana_pool
        .total()
}

/// The Oath of Teferi line-2 shape as a constructed static (Eidolon-test
/// style): `ModifyActivationLimit { loyalty, 2 }` affecting planeswalkers the
/// static's controller controls. Used for scope variants no single printed
/// line carries (opponent's Oath, double Oath, SelfRef).
fn oath_shaped_limit() -> StaticDefinition {
    StaticDefinition::new(StaticMode::ModifyActivationLimit {
        keyword: "loyalty".to_string(),
        new_limit: 2,
    })
    .affected(TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Planeswalker).controller(ControllerRef::You),
    ))
}

fn find_by_name(runner: &GameRunner, name: &str, zone: Zone) -> ObjectId {
    runner
        .state()
        .objects
        .values()
        .find(|o| o.name == name && o.zone == zone)
        .unwrap_or_else(|| panic!("{name} must be in {zone:?}"))
        .id
}

/// Hands `holder` priority in `phase` of `active`'s turn with an empty stack.
fn give_priority(runner: &mut GameRunner, active: PlayerId, holder: PlayerId, phase: Phase) {
    let state = runner.state_mut();
    state.active_player = active;
    state.phase = phase;
    state.priority_player = holder;
    state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: holder };
}

/// Cast a real Oath of Teferi through the pipeline, resolving its ETB blink
/// onto the benign "Blinking Bear" so the stack is empty afterwards (CR 606.3
/// activations need sorcery speed). Returns the runner, the test walker, and
/// the battlefield Oath.
fn cast_oath_with_walker() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Blinking Bear", 2, 2).id();
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, TEST_WALKER)
        .id();
    let oath_hand = scenario
        .add_spell_to_hand_from_oracle(P0, "Oath of Teferi", false, OATH_OF_TEFERI)
        .as_enchantment()
        .as_legendary()
        .with_mana_cost(oath_cost())
        .id();
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    runner.cast(oath_hand).target_object(bear).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "the Oath cast plus its ETB blink must fully resolve, leaving an empty stack"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Exile,
        "reach-guard: the ETB blink exiled the benign target"
    );
    let oath = find_by_name(&runner, "Oath of Teferi", Zone::Battlefield);
    (runner, walker, oath)
}

/// CR 606.3 + Oath ruling 1 (first disjunct): with Oath out, the SAME loyalty
/// ability may be activated twice — the cap is per-permanent, not per-index.
/// A 3rd activation is refused. Reverting the `planeswalker.rs` cap line flips
/// the 2nd activation to refused.
#[test]
fn oath_allows_same_loyalty_ability_twice_then_refuses_third() {
    let (mut runner, walker, _) = cast_oath_with_walker();
    let plus = loyalty_index_by_amount(&runner, walker, 1);
    let life_before = life_of(&runner, P0);

    activate(&mut runner, walker, plus).expect("1st activation must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(loyalty_of(&runner, walker), 4);
    assert_eq!(activations_of(&runner, walker), 1);

    activate(&mut runner, walker, plus).expect("2nd activation must succeed with Oath");
    runner.advance_until_stack_empty();
    assert_eq!(loyalty_of(&runner, walker), 5);
    assert_eq!(activations_of(&runner, walker), 2);
    assert_eq!(
        life_of(&runner, P0),
        life_before + 2,
        "reach-guard: both +1 resolutions observed"
    );

    assert_activation_refused(&mut runner, walker, plus);
    assert_eq!(activations_of(&runner, walker), 2);
    assert!(runner.state().stack.is_empty());
}

/// CR 606.3 + Oath ruling 1 (second disjunct): with Oath out, TWO DIFFERENT
/// abilities of the same planeswalker may be activated. Reverting the cap line
/// flips the 2nd activation to refused.
#[test]
fn oath_allows_two_different_loyalty_abilities_then_refuses_third() {
    let (mut runner, walker, _) = cast_oath_with_walker();
    let plus = loyalty_index_by_amount(&runner, walker, 1);
    let minus = loyalty_index_by_amount(&runner, walker, -1);
    let life_before = life_of(&runner, P0);

    activate(&mut runner, walker, plus).expect("the +1 must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(loyalty_of(&runner, walker), 4);

    activate(&mut runner, walker, minus).expect("the -1 must succeed with Oath");
    runner.advance_until_stack_empty();
    assert_eq!(loyalty_of(&runner, walker), 3);
    assert_eq!(activations_of(&runner, walker), 2);
    assert_eq!(
        life_of(&runner, P0),
        life_before + 3,
        "reach-guard: +1 life and +2 life both resolved"
    );

    assert_activation_refused(&mut runner, walker, plus);
    assert_activation_refused(&mut runner, walker, minus);
    assert_eq!(activations_of(&runner, walker), 2);
}

/// CR 606.3 baseline lock: without Oath, the 2nd loyalty activation is
/// refused. The 1st activation's loyalty delta is the reach-guard proving the
/// refusal comes from the cap, not from an unreachable fixture.
#[test]
fn second_loyalty_activation_refused_without_oath() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, TEST_WALKER)
        .id();
    let mut runner = scenario.build();
    let plus = loyalty_index_by_amount(&runner, walker, 1);

    activate(&mut runner, walker, plus).expect("1st activation must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(
        loyalty_of(&runner, walker),
        4,
        "reach-guard: the 1st activation resolved"
    );

    assert_activation_refused(&mut runner, walker, plus);
    assert_eq!(activations_of(&runner, walker), 1);
    assert_eq!(loyalty_of(&runner, walker), 4);
}

/// CR 606.3: the cap is per-permanent — two planeswalkers under one Oath each
/// activate twice (2 + 2), and a 3rd on either is refused. Deltas on BOTH
/// walkers prove there is no shared pool.
#[test]
fn oath_grants_two_activations_per_planeswalker_independently() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Blinking Bear", 2, 2).id();
    let walker_a = scenario
        .add_planeswalker_from_oracle(P0, "Walker A", "Test", 3, TEST_WALKER)
        .id();
    let walker_b = scenario
        .add_planeswalker_from_oracle(P0, "Walker B", "Test", 3, TEST_WALKER)
        .id();
    let oath_hand = scenario
        .add_spell_to_hand_from_oracle(P0, "Oath of Teferi", false, OATH_OF_TEFERI)
        .as_enchantment()
        .as_legendary()
        .with_mana_cost(oath_cost())
        .id();
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    runner.cast(oath_hand).target_object(bear).resolve();
    assert!(runner.state().stack.is_empty());

    for walker in [walker_a, walker_b] {
        let plus = loyalty_index_by_amount(&runner, walker, 1);
        activate(&mut runner, walker, plus).expect("1st activation must succeed");
        runner.advance_until_stack_empty();
        activate(&mut runner, walker, plus).expect("2nd activation must succeed with Oath");
        runner.advance_until_stack_empty();
    }
    assert_eq!(loyalty_of(&runner, walker_a), 5);
    assert_eq!(loyalty_of(&runner, walker_b), 5);
    assert_eq!(activations_of(&runner, walker_a), 2);
    assert_eq!(activations_of(&runner, walker_b), 2);

    let plus_a = loyalty_index_by_amount(&runner, walker_a, 1);
    let plus_b = loyalty_index_by_amount(&runner, walker_b, 1);
    assert_activation_refused(&mut runner, walker_a, plus_a);
    assert_activation_refused(&mut runner, walker_b, plus_b);
}

/// CR 603.7c + CR 606.3: Oath's ETB blink half is preserved end-to-end — the
/// blinked permanent is exiled, and returns at the beginning of the next end
/// step — while the loyalty static half is ALSO present on the same object
/// (both halves coexist; the routing change stole neither line).
#[test]
fn oath_etb_blink_preserved_and_static_coexists() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Blinking Bear", 2, 2).id();
    // A surviving attacker so the DeclareAttackers prompt surfaces when
    // crossing combat (an empty board auto-skips combat and the advance loop
    // blows past into the next turn, ending the game on an empty library).
    scenario.add_creature(P0, "Bystander Bear", 2, 2).id();
    let oath_hand = scenario
        .add_spell_to_hand_from_oracle(P0, "Oath of Teferi", false, OATH_OF_TEFERI)
        .as_enchantment()
        .as_legendary()
        .with_mana_cost(oath_cost())
        .id();
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    runner.cast(oath_hand).target_object(bear).resolve();
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Exile,
        "the blink target must be exiled by the ETB trigger"
    );

    let oath = find_by_name(&runner, "Oath of Teferi", Zone::Battlefield);
    let oath_obj = &runner.state().objects[&oath];
    assert!(
        oath_obj
            .trigger_definitions
            .iter_unchecked()
            .any(|t| t.definition.mode == TriggerMode::ChangesZone),
        "the ETB trigger half must be present, got {:?}",
        oath_obj.trigger_definitions.as_slice()
    );
    assert!(
        oath_obj
            .static_definitions
            .iter_unchecked()
            .any(|d| matches!(
                &d.mode,
                StaticMode::ModifyActivationLimit { keyword, new_limit: 2 } if keyword == "loyalty"
            )),
        "the loyalty static half must be present, got {:?}",
        oath_obj.static_definitions.as_slice()
    );

    // Cross combat with no attackers, then the delayed trigger returns the
    // blinked permanent at the beginning of the next end step.
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "the blinked permanent must return at the next end step"
    );
}

/// CR 606.3 + CR 201.5 + CR 201.5c: Urza, Planeswalker's `SelfRef` scope grants the extra
/// activation ONLY to its source. Walker A (carrying the constructed SelfRef
/// static) activates twice; Walker B's 2nd activation is refused — the scan
/// discriminates per-source rather than blanket-allowing.
#[test]
fn self_scoped_loyalty_limit_grants_only_its_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let self_limit = StaticDefinition::new(StaticMode::ModifyActivationLimit {
        keyword: "loyalty".to_string(),
        new_limit: 2,
    })
    .affected(TargetFilter::SelfRef);
    let walker_a = scenario
        .add_planeswalker_from_oracle(P0, "Walker A", "Test", 3, TEST_WALKER)
        .with_static_definition(self_limit)
        .id();
    let walker_b = scenario
        .add_planeswalker_from_oracle(P0, "Walker B", "Test", 3, TEST_WALKER)
        .id();
    let mut runner = scenario.build();

    let plus_a = loyalty_index_by_amount(&runner, walker_a, 1);
    activate(&mut runner, walker_a, plus_a).expect("A's 1st must succeed");
    runner.advance_until_stack_empty();
    activate(&mut runner, walker_a, plus_a)
        .expect("A's 2nd must succeed (SelfRef grants its source)");
    runner.advance_until_stack_empty();
    assert_eq!(loyalty_of(&runner, walker_a), 5);
    assert_eq!(activations_of(&runner, walker_a), 2);

    let plus_b = loyalty_index_by_amount(&runner, walker_b, 1);
    activate(&mut runner, walker_b, plus_b).expect("B's 1st must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(
        loyalty_of(&runner, walker_b),
        4,
        "reach-guard: B's 1st activation resolved"
    );
    assert_activation_refused(&mut runner, walker_b, plus_b);
    assert_eq!(activations_of(&runner, walker_b), 1);
}

/// CR 606.3 + CR 109.5: an opponent's Oath grants nothing to your walkers —
/// the scan's controller gate (`bf_obj.controller != player`) skips it — while
/// the SAME static grants the opponent's own walker. Multi-authority: P0's 2nd
/// refused, P1's 2nd allowed.
#[test]
fn opponents_oath_grants_nothing_to_your_walkers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P1, "Foe Oath", NO_ORACLE_FACE)
        .with_static_definition(oath_shaped_limit())
        .id();
    let walker_p0 = scenario
        .add_planeswalker_from_oracle(P0, "Walker P0", "Test", 3, TEST_WALKER)
        .id();
    let walker_p1 = scenario
        .add_planeswalker_from_oracle(P1, "Walker P1", "Test", 3, TEST_WALKER)
        .id();
    let mut runner = scenario.build();

    let plus_p0 = loyalty_index_by_amount(&runner, walker_p0, 1);
    activate(&mut runner, walker_p0, plus_p0).expect("P0's 1st must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(
        loyalty_of(&runner, walker_p0),
        4,
        "reach-guard: P0's 1st activation resolved"
    );
    assert_activation_refused(&mut runner, walker_p0, plus_p0);
    assert_eq!(activations_of(&runner, walker_p0), 1);

    // The same static grants its own controller: on P1's main phase, P1's
    // walker activates twice.
    give_priority(&mut runner, P1, P1, Phase::PreCombatMain);
    let plus_p1 = loyalty_index_by_amount(&runner, walker_p1, 1);
    activate(&mut runner, walker_p1, plus_p1).expect("P1's 1st must succeed");
    runner.advance_until_stack_empty();
    activate(&mut runner, walker_p1, plus_p1).expect("P1's 2nd must succeed (own Oath grants)");
    runner.advance_until_stack_empty();
    assert_eq!(loyalty_of(&runner, walker_p1), 5);
    assert_eq!(activations_of(&runner, walker_p1), 2);
}

/// CR 606.3 + Oath ruling 2: two Oaths do not stack — max semantics keep the
/// cap at 2, so the 3rd activation is still refused.
#[test]
fn two_oaths_do_not_stack_past_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Oath One", NO_ORACLE_FACE)
        .with_static_definition(oath_shaped_limit())
        .id();
    scenario
        .add_enchantment_from_oracle(P0, "Oath Two", NO_ORACLE_FACE)
        .with_static_definition(oath_shaped_limit())
        .id();
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, TEST_WALKER)
        .id();
    let mut runner = scenario.build();
    let plus = loyalty_index_by_amount(&runner, walker, 1);

    activate(&mut runner, walker, plus).expect("1st must succeed");
    runner.advance_until_stack_empty();
    activate(&mut runner, walker, plus).expect("2nd must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(activations_of(&runner, walker), 2);

    assert_activation_refused(&mut runner, walker, plus);
    assert_eq!(activations_of(&runner, walker), 2);
}

/// CR 606.3: the SET-cap static composes additively with The Chain Veil's ADD
/// grant — `cap = static + extra_grants`, so the 3rd activation succeeds and
/// the 4th is refused (proving +1, not +infinity).
#[test]
fn oath_composes_with_chain_veil_grant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Oath Stand-in", NO_ORACLE_FACE)
        .with_static_definition(oath_shaped_limit())
        .id();
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, TEST_WALKER)
        .id();
    let mut runner = scenario.build();
    // Seed the Chain Veil +1 grant directly (the Chain Veil resolver's own
    // unit-test precedent writes this same map).
    runner
        .state_mut()
        .extra_loyalty_activations_this_turn
        .insert(P0, 1);
    let plus = loyalty_index_by_amount(&runner, walker, 1);

    for n in 1..=3 {
        activate(&mut runner, walker, plus)
            .unwrap_or_else(|e| panic!("activation {n} must succeed (cap = 2 + 1), got {e:?}"));
        runner.advance_until_stack_empty();
    }
    assert_eq!(loyalty_of(&runner, walker), 6);
    assert_eq!(activations_of(&runner, walker), 3);

    assert_activation_refused(&mut runner, walker, plus);
    assert_eq!(activations_of(&runner, walker), 3);
}

/// CR 606.3 + CR 118.7 + CR 601.2f: Oath composes with an Eidolon tax — the
/// taxed 2nd activation succeeds, the 3rd is refused. This pins the single
/// pre-delegation cap check: `handle_activate_loyalty` checks the cap before
/// delegating to `handle_activate_ability`, with no second cap read
/// downstream. Mana-paid + loyalty-delta reach-guards prove the delegation
/// path ran rather than the fast path.
#[test]
fn oath_composes_with_eidolon_tax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Oath Stand-in", NO_ORACLE_FACE)
        .with_static_definition(oath_shaped_limit())
        .id();
    scenario.add_creature_from_oracle(P1, "Eidolon of Obstruction", 2, 1, EIDOLON_OF_OBSTRUCTION);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, TEST_WALKER)
        .id();
    scenario.with_mana_pool(P0, mana_pool(&[ManaType::Colorless, ManaType::Colorless]));
    let mut runner = scenario.build();
    let plus = loyalty_index_by_amount(&runner, walker, 1);
    let life_before = life_of(&runner, P0);

    activate(&mut runner, walker, plus).expect("taxed 1st must succeed");
    runner.advance_until_stack_empty();
    activate(&mut runner, walker, plus).expect("taxed 2nd must succeed with Oath");
    runner.advance_until_stack_empty();
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "reach-guard: both activations paid the taxed {{1}} through the delegation path"
    );
    assert_eq!(loyalty_of(&runner, walker), 5);
    assert_eq!(activations_of(&runner, walker), 2);
    assert_eq!(
        life_of(&runner, P0),
        life_before + 2,
        "reach-guard: both taxed resolutions observed"
    );

    assert_activation_refused(&mut runner, walker, plus);
    assert_eq!(activations_of(&runner, walker), 2);
}

/// CR 101.2 + CR 606.3: The Immortal Sun's loyalty prohibition wins over Oath
/// (can't-beats-can is satisfied by consulting the prohibition at all). With
/// the Sun out the activation is refused; with the Sun removed the same walker
/// activates twice — proving the refusal came from the prohibition, not the
/// counter. Reverting the prohibition consult flips the Sun-out activation to
/// allowed.
#[test]
fn immortal_sun_prohibition_wins_over_oath() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Oath Stand-in", NO_ORACLE_FACE)
        .with_static_definition(oath_shaped_limit())
        .id();
    scenario.add_artifact_from_oracle(P1, "The Immortal Sun", IMMORTAL_SUN);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, TEST_WALKER)
        .id();
    let mut runner = scenario.build();
    let plus = loyalty_index_by_amount(&runner, walker, 1);

    assert_activation_refused(&mut runner, walker, plus);
    assert_eq!(activations_of(&runner, walker), 0);

    // Remove the Sun: the Oath static was functioning all along.
    let sun = find_by_name(&runner, "The Immortal Sun", Zone::Battlefield);
    runner.state_mut().battlefield.retain(|id| *id != sun);
    runner
        .state_mut()
        .objects
        .get_mut(&sun)
        .expect("sun exists")
        .zone = Zone::Exile;
    activate(&mut runner, walker, plus).expect("1st must succeed once the Sun leaves");
    runner.advance_until_stack_empty();
    activate(&mut runner, walker, plus).expect("2nd must succeed once the Sun leaves");
    runner.advance_until_stack_empty();
    assert_eq!(activations_of(&runner, walker), 2);
}

/// CR 611.3a + CR 606.3: the static is read live, not latched — removing Oath
/// mid-turn drops the cap immediately while the per-permanent activation
/// counter persists. Walker A (one activation) loses its 2nd; Walker B (two
/// activations) is still refused a 3rd.
#[test]
fn oath_removed_mid_turn_drops_cap_live() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Oath Stand-in", NO_ORACLE_FACE)
        .with_static_definition(oath_shaped_limit())
        .id();
    let walker_a = scenario
        .add_planeswalker_from_oracle(P0, "Walker A", "Test", 3, TEST_WALKER)
        .id();
    let walker_b = scenario
        .add_planeswalker_from_oracle(P0, "Walker B", "Test", 3, TEST_WALKER)
        .id();
    let mut runner = scenario.build();

    let plus_a = loyalty_index_by_amount(&runner, walker_a, 1);
    activate(&mut runner, walker_a, plus_a).expect("A's 1st must succeed");
    runner.advance_until_stack_empty();
    let plus_b = loyalty_index_by_amount(&runner, walker_b, 1);
    activate(&mut runner, walker_b, plus_b).expect("B's 1st must succeed");
    runner.advance_until_stack_empty();
    activate(&mut runner, walker_b, plus_b).expect("B's 2nd must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(activations_of(&runner, walker_b), 2);

    // Oath leaves mid-turn: the cap drops live for both walkers.
    let oath = find_by_name(&runner, "Oath Stand-in", Zone::Battlefield);
    runner.state_mut().battlefield.retain(|id| *id != oath);
    runner
        .state_mut()
        .objects
        .get_mut(&oath)
        .expect("oath exists")
        .zone = Zone::Graveyard;

    assert_activation_refused(&mut runner, walker_a, plus_a);
    assert_eq!(activations_of(&runner, walker_a), 1);
    assert_activation_refused(&mut runner, walker_b, plus_b);
    assert_eq!(
        activations_of(&runner, walker_b),
        2,
        "the per-permanent counter persists even though the cap dropped"
    );
}

/// CR 611.3a + CR 109.5: Oath changing controllers mid-turn re-points the
/// beneficiary (the scan gate reads the CURRENT controller) — P0's walker
/// loses its 2nd activation while P1's walker gains one.
#[test]
fn oath_control_change_repoints_beneficiary() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Oath Stand-in", NO_ORACLE_FACE)
        .with_static_definition(oath_shaped_limit())
        .id();
    let walker_p0 = scenario
        .add_planeswalker_from_oracle(P0, "Walker P0", "Test", 3, TEST_WALKER)
        .id();
    let walker_p1 = scenario
        .add_planeswalker_from_oracle(P1, "Walker P1", "Test", 3, TEST_WALKER)
        .id();
    // Control moves through the real `Effect::GainControl` path: `controller`
    // is layers-derived, so a manual flip would be wiped by the next layer
    // pass instead of testing the live scan.
    let thief = scenario
        .add_creature_from_oracle(
            P1,
            "Enchantment Thief",
            1,
            1,
            "{T}: Gain control of target enchantment.",
        )
        .id();
    let mut runner = scenario.build();

    let plus_p0 = loyalty_index_by_amount(&runner, walker_p0, 1);
    activate(&mut runner, walker_p0, plus_p0).expect("P0's 1st must succeed");
    runner.advance_until_stack_empty();

    let oath = find_by_name(&runner, "Oath Stand-in", Zone::Battlefield);
    give_priority(&mut runner, P0, P1, Phase::PreCombatMain);
    runner.activate(thief, 0).target_objects(&[oath]).resolve();
    assert_eq!(
        runner.state().objects[&oath].controller,
        P1,
        "reach-guard: the theft resolved through the layers"
    );

    give_priority(&mut runner, P0, P0, Phase::PreCombatMain);
    assert_activation_refused(&mut runner, walker_p0, plus_p0);
    assert_eq!(activations_of(&runner, walker_p0), 1);

    give_priority(&mut runner, P1, P1, Phase::PreCombatMain);
    let plus_p1 = loyalty_index_by_amount(&runner, walker_p1, 1);
    activate(&mut runner, walker_p1, plus_p1).expect("P1's 1st must succeed");
    runner.advance_until_stack_empty();
    activate(&mut runner, walker_p1, plus_p1).expect("P1's 2nd must succeed (re-pointed Oath)");
    runner.advance_until_stack_empty();
    assert_eq!(activations_of(&runner, walker_p1), 2);
}

/// CR 606.3 + CR 701.10e: loyalty-counter count and activation count are
/// independent state — doubling the walker's loyalty counters (Deepglow
/// Skate's typed `Double{Counters}` trigger, resolved live) does not raise
/// the activation cap. The counter-doubling delta is the reach-guard proving
/// the doubling resolved.
#[test]
fn doubling_loyalty_counters_does_not_raise_cap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, TEST_WALKER)
        .id();
    let skate = scenario
        .add_creature_to_hand_from_oracle(P0, "Deepglow Skate", 3, 3, DEEPGLOW_SKATE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 4,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    runner.cast(skate).target_object(walker).resolve();
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&walker].counters[&CounterType::Loyalty],
        6,
        "reach-guard: the doubling resolved (3 counters became 6)"
    );

    let plus = loyalty_index_by_amount(&runner, walker, 1);
    activate(&mut runner, walker, plus).expect("1st activation must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(activations_of(&runner, walker), 1);

    assert_activation_refused(&mut runner, walker, plus);
    assert_eq!(activations_of(&runner, walker), 1);
}
