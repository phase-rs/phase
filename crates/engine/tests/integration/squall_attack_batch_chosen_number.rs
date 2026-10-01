//! Squall, Gunblade Duelist — the attack-batch chosen-number existential
//! intervening-if, including its CR 603.4 resolution recheck.
//!
//! Verbatim Oracle text (Scryfall, 2026-10-01):
//!   "First strike
//!    As Squall enters, choose a number.
//!    Whenever one or more creatures attack one of your opponents, if any of
//!    those creatures have power or toughness equal to the chosen number,
//!    Squall deals damage equal to its power to defending player."
//!
//! CR 506.2 + CR 508.1 + CR 508.1b — the attacked-player relation comes from
//! the trigger head. CR 603.4 — the intervening-if is checked at trigger time
//! and again at resolution; a filter-carrying condition must survive onto the
//! stack entry. CR 608.2h + CR 400.7 — a departed attacker is answered by its
//! last-battlefield power/toughness (Squall's official ruling: "If any of those
//! creatures have left the battlefield since the first time the ability checked
//! their power and toughness, use their power and toughness as they last
//! existed on the battlefield."). CR 614.12a — the chosen number is the
//! as-enters choice persisted on the source.
//!
//! Attackers are built at 0 power so the defending player's life delta is
//! attributable to Squall alone, and each row resolves the trigger before the
//! combat-damage step (the stack empties while the turn is still in declare
//! attackers).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::game::{layers, sba};
use engine::types::ability::{
    AttackersDeclaredCountSubject, ChosenAttribute, Comparator, ControllerRef, FilterProp, PtStat,
    PtValueScope, QuantityExpr, QuantityRef, TargetFilter, TriggerCondition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::AttackTargetFilter;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const SQUALL: &str = "First strike\n\
As Squall enters, choose a number.\n\
Whenever one or more creatures attack one of your opponents, if any of those creatures have power \
or toughness equal to the chosen number, Squall deals damage equal to its power to defending player.";

struct SquallFixture {
    runner: GameRunner,
    squall: ObjectId,
    attacker: ObjectId,
}

/// Squall under P0 with `chosen` seeded on the source, plus a P0 vanilla
/// attacker built at `power`/`toughness`.
fn squall_fixture(chosen: u32, power: i32, toughness: i32) -> SquallFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let squall = scenario
        .add_creature_from_oracle(P0, "Squall, Gunblade Duelist", 3, 2, SQUALL)
        .id();
    let attacker = scenario.add_vanilla(P0, power, toughness);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&squall)
        .expect("Squall source exists")
        .chosen_attributes
        .push(ChosenAttribute::Number(chosen));
    SquallFixture {
        runner,
        squall,
        attacker,
    }
}

fn order_triggers_if_needed(runner: &mut GameRunner) {
    while let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("ordering attack triggers should succeed");
    }
}

fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!("expected DeclareAttackers");
}

fn stack_condition_for_source(
    runner: &GameRunner,
    source_id: ObjectId,
) -> Option<TriggerCondition> {
    runner.state().stack.iter().find_map(|entry| {
        if entry.source_id != source_id {
            return None;
        }
        match &entry.kind {
            StackEntryKind::TriggeredAbility { condition, .. } => condition.clone(),
            _ => None,
        }
    })
}

/// The parsed condition's expected value: head scope `controller` + the shared
/// chosen-number P/T filter, existential count 1.
fn chosen_number_condition(controller: ControllerRef) -> TriggerCondition {
    TriggerCondition::AttackersDeclaredCount {
        subject: AttackersDeclaredCountSubject::AttackTarget {
            controller,
            attacked: AttackTargetFilter::Player,
            filter: Some(TargetFilter::Typed(TypedFilter::default().properties(
                vec![FilterProp::AnyOf {
                    props: vec![
                        FilterProp::PtComparison {
                            stat: PtStat::Power,
                            scope: PtValueScope::Current,
                            comparator: Comparator::EQ,
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::ChosenNumber,
                            },
                        },
                        FilterProp::PtComparison {
                            stat: PtStat::Toughness,
                            scope: PtValueScope::Current,
                            comparator: Comparator::EQ,
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::ChosenNumber,
                            },
                        },
                    ],
                }],
            ))),
        },
        comparator: Comparator::GE,
        count: 1,
    }
}

/// Declare the vanilla attacker against P1 and stack the Squall trigger.
fn declare_squall_attack(fixture: &mut SquallFixture) {
    hand_turn_to(&mut fixture.runner, P0);
    fixture
        .runner
        .declare_attackers(&[(fixture.attacker, AttackTarget::Player(P1))])
        .expect("the attack declaration must be legal");
    order_triggers_if_needed(&mut fixture.runner);
}

/// CR 400.7 + CR 603.4 + CR 608.2h: flicker the attacker between the trigger's
/// stacking and its resolution — leave the battlefield (the exit-time snapshot
/// is keyed to the attacking incarnation) and return as a new incarnation at
/// the same storage id.
fn blink_attacker(runner: &mut GameRunner, attacker: ObjectId) {
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), attacker, Zone::Exile, &mut events);
    move_to_zone(runner.state_mut(), attacker, Zone::Battlefield, &mut events);
}

/// The attacking incarnation pinned by the declaration ledger.
fn attacking_pin(runner: &GameRunner, attacker: ObjectId) -> ObjectIncarnationRef {
    runner
        .state()
        .combat
        .as_ref()
        .expect("combat is active")
        .attacking_incarnations_this_combat
        .iter()
        .find(|reference| reference.object_id == attacker)
        .copied()
        .expect("the declaration ledger pins the attacker")
}

/// Row 4.3 positive: a qualifying attacker attacking an opponent of Squall's
/// controller fires and damages the defending player.
#[test]
fn matching_attacker_attacking_an_opponent_deals_damage() {
    let mut fixture = squall_fixture(2, 0, 2);
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);

    assert_eq!(
        fixture.runner.state().objects[&fixture.attacker].zone,
        Zone::Battlefield,
        "reach guard: the declaration succeeded"
    );
    assert_eq!(
        stack_condition_for_source(&fixture.runner, fixture.squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "P1-7 reach guard: the filter-carrying condition is retained on the stack entry"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before - 3,
        "Squall's power must be dealt to the defending player"
    );
}

/// Row 4.3 negative: a non-matching attacker (no axis equals the chosen
/// number) does not even stack the ability.
#[test]
fn non_matching_attacker_deals_no_damage() {
    let mut fixture = squall_fixture(4, 0, 2);
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);

    assert_eq!(
        fixture.runner.state().objects[&fixture.attacker].zone,
        Zone::Battlefield,
        "reach guard: the declaration succeeded"
    );
    assert!(
        stack_condition_for_source(&fixture.runner, fixture.squall).is_none(),
        "the fire-time existential is false: no Squall trigger"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before,
        "no qualifying attacker → no damage"
    );
}

/// Row 4.4: the chosen number is the trigger source's persisted choice — the
/// event subject (attacker) carries no chosen attribute, so a subject-scoped
/// read would answer 0. Positive (chosen 2 on Squall, 0/2 attacker) deals
/// exactly Squall's power; the mismatch sibling (chosen 3) does nothing.
#[test]
fn chosen_number_reads_the_source_object() {
    let damage_for_chosen = |chosen: u32| {
        let mut fixture = squall_fixture(chosen, 0, 2);
        let life_before = fixture.runner.life(P1);
        declare_squall_attack(&mut fixture);
        fixture.runner.advance_until_stack_empty();
        life_before - fixture.runner.life(P1)
    };

    assert_eq!(
        damage_for_chosen(2),
        3,
        "the number seeded on Squall qualifies the 0/2 attacker"
    );
    assert_eq!(
        damage_for_chosen(3),
        0,
        "a number the attacker does not match leaves the ability unstacked"
    );
}

/// Row 4.3 hostile (3 players): a MATCHING attacker attacking Squall's
/// controller does not qualify (wrong attacked-player relation), and a
/// non-matching attacker attacking an opponent cannot satisfy the filter — the
/// existential is false and no damage lands on either player.
#[test]
fn hostile_scope_matching_attacker_on_controller_does_not_qualify() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let squall = scenario
        .add_creature_from_oracle(P1, "Squall, Gunblade Duelist", 3, 2, SQUALL)
        .id();
    let matching = scenario.add_vanilla(P0, 2, 2);
    let non_matching = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&squall)
        .expect("Squall source exists")
        .chosen_attributes
        .push(ChosenAttribute::Number(2));

    let p1_before = runner.life(P1);
    let p2_before = runner.life(P2);
    hand_turn_to(&mut runner, P0);
    runner
        .declare_attackers(&[
            (matching, AttackTarget::Player(P1)),
            (non_matching, AttackTarget::Player(P2)),
        ])
        .expect("declaring attacks on two opponents must be legal");
    order_triggers_if_needed(&mut runner);

    assert_eq!(
        runner.state().combat.as_ref().map(|c| c.attackers.len()),
        Some(2),
        "reach guard: both attacks were declared"
    );
    assert!(
        stack_condition_for_source(&runner, squall).is_none(),
        "the matching attacker attacked the controller, not an opponent"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P1),
        p1_before,
        "no damage to the controller attacked by the qualifying creature"
    );
    assert_eq!(
        runner.life(P2),
        p2_before,
        "no damage to the opponent attacked by the non-matching creature"
    );
}

/// Row 4.3 hostile twin positive: moving the matching creature onto the
/// opponent's attack makes the existential true and damages that opponent.
#[test]
fn hostile_scope_matching_attacker_on_the_opponent_fires() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let squall = scenario
        .add_creature_from_oracle(P1, "Squall, Gunblade Duelist", 3, 2, SQUALL)
        .id();
    let matching = scenario.add_vanilla(P0, 2, 2);
    let non_matching = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&squall)
        .expect("Squall source exists")
        .chosen_attributes
        .push(ChosenAttribute::Number(2));

    let p1_before = runner.life(P1);
    let p2_before = runner.life(P2);
    hand_turn_to(&mut runner, P0);
    runner
        .declare_attackers(&[
            (matching, AttackTarget::Player(P2)),
            (non_matching, AttackTarget::Player(P1)),
        ])
        .expect("declaring attacks on two opponents must be legal");
    order_triggers_if_needed(&mut runner);

    assert_eq!(
        stack_condition_for_source(&runner, squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "reach guard: the qualifying attacker on the opponent stacks the trigger"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P1),
        p1_before,
        "the non-matching attacker's declaration contributes nothing"
    );
    assert_eq!(
        runner.life(P2),
        p2_before - 3,
        "the matching attacker's declaration damages its defending player"
    );
}

/// Row 7.4 (positive reach): a still-qualifying declaration resolves the
/// retained condition's recheck and deals damage.
#[test]
fn still_qualifying_declaration_deals_damage() {
    let mut fixture = squall_fixture(2, 0, 2);
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);
    assert_eq!(
        stack_condition_for_source(&fixture.runner, fixture.squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "P1-7 retention reach guard"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before - 3,
        "a still-qualifying declaration keeps the ability"
    );
}

/// Row 7.5: a live P/T change so that no axis equals the chosen number turns
/// the recheck false — the ability leaves the stack and no damage is dealt.
#[test]
fn pt_change_on_battlefield_removes_the_ability() {
    let mut fixture = squall_fixture(2, 0, 2);
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);
    assert_eq!(
        stack_condition_for_source(&fixture.runner, fixture.squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "reach guard: the retained condition is on the stack before the change"
    );

    // +1/+1 counter through a real layer pass: the live board becomes 1/3.
    fixture
        .runner
        .state_mut()
        .objects
        .get_mut(&fixture.attacker)
        .expect("attacker exists")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    fixture.runner.state_mut().layers_dirty.mark_full();
    layers::evaluate_layers(fixture.runner.state_mut());
    let object = &fixture.runner.state().objects[&fixture.attacker];
    assert_eq!(
        (object.power, object.toughness),
        (Some(1), Some(3)),
        "post-flush P/T must no longer equal the chosen number"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before,
        "the recheck must remove the ability: no damage"
    );
}

/// Row 7.6: a qualifying attacker that DIES is answered by its
/// last-battlefield P/T — the ability still resolves. A presence-only
/// departure guard would wrongly remove it.
#[test]
fn dead_qualifying_attacker_still_resolves() {
    let mut fixture = squall_fixture(2, 0, 2);
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);
    assert_eq!(
        stack_condition_for_source(&fixture.runner, fixture.squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "reach guard: the retained condition is on the stack before the death"
    );

    let mut events = Vec::new();
    move_to_zone(
        fixture.runner.state_mut(),
        fixture.attacker,
        Zone::Graveyard,
        &mut events,
    );
    assert!(
        fixture
            .runner
            .state()
            .lki_cache
            .contains_key(&fixture.attacker),
        "the zone exit must have captured the last-battlefield snapshot"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before - 3,
        "a departed but still-qualifying attacker keeps the ability"
    );
}

/// Row 7.7: an attacker that was 0/2 at fire time becomes 1/3 (a +1/+1
/// counter) and then dies. Its last-battlefield P/T (1/3) no longer qualifies,
/// while the reverted printed base (0/2) would — the LKI read decides.
#[test]
fn shrunk_then_dead_attacker_is_removed() {
    let mut fixture = squall_fixture(2, 0, 2);
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);
    assert_eq!(
        stack_condition_for_source(&fixture.runner, fixture.squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "reach guard: the retained condition is on the stack before the change"
    );

    fixture
        .runner
        .state_mut()
        .objects
        .get_mut(&fixture.attacker)
        .expect("attacker exists")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    fixture.runner.state_mut().layers_dirty.mark_full();
    layers::evaluate_layers(fixture.runner.state_mut());
    let object = &fixture.runner.state().objects[&fixture.attacker];
    assert_eq!(
        (object.power, object.toughness),
        (Some(1), Some(3)),
        "post-flush P/T must no longer equal the chosen number"
    );

    let mut events = Vec::new();
    move_to_zone(
        fixture.runner.state_mut(),
        fixture.attacker,
        Zone::Graveyard,
        &mut events,
    );
    assert!(
        fixture
            .runner
            .state()
            .lki_cache
            .contains_key(&fixture.attacker),
        "the zone exit must have captured the last-battlefield snapshot"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.attacker].power,
        Some(0),
        "the live card is reverted to its printed base power"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before,
        "the last-battlefield P/T (1/3) does not qualify: no damage"
    );
}

/// Row 7.8: a ceased token (purged from the object map) is still answered by
/// its LKI snapshot. Without the LKI branch the purged object fails closed.
#[test]
fn ceased_token_qualifier_reads_lki() {
    let mut fixture = squall_fixture(2, 0, 2);
    fixture
        .runner
        .state_mut()
        .objects
        .get_mut(&fixture.attacker)
        .expect("attacker exists")
        .is_token = true;
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);
    assert_eq!(
        stack_condition_for_source(&fixture.runner, fixture.squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "reach guard: the retained condition is on the stack before the purge"
    );

    let mut events = Vec::new();
    move_to_zone(
        fixture.runner.state_mut(),
        fixture.attacker,
        Zone::Graveyard,
        &mut events,
    );
    sba::check_state_based_actions(fixture.runner.state_mut(), &mut events);
    assert!(
        !fixture
            .runner
            .state()
            .objects
            .contains_key(&fixture.attacker),
        "CR 111.7: the token must have ceased to exist"
    );
    assert!(
        fixture
            .runner
            .state()
            .lki_cache
            .contains_key(&fixture.attacker),
        "the exit-time snapshot must still answer for the purged token"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before - 3,
        "the purged token's LKI qualifies: the ability resolves"
    );
}

/// Row F1(a): a matching-at-fire attacker blinks between stacking and
/// resolution. The recheck is answered by the attacking incarnation's
/// last-battlefield P/T (0/2), so the ability resolves even though the returned
/// incarnation is 1/3; without the pin the live 1/3 removes it.
#[test]
fn blinked_attacker_resolves_from_the_attacking_incarnation() {
    let mut fixture = squall_fixture(2, 0, 2);
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);
    assert_eq!(
        stack_condition_for_source(&fixture.runner, fixture.squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "reach guard: the retained condition is on the stack before the blink"
    );

    let pin = attacking_pin(&fixture.runner, fixture.attacker);
    let incarnation_before = pin.incarnation;
    blink_attacker(&mut fixture.runner, fixture.attacker);

    assert_eq!(
        fixture.runner.state().objects[&fixture.attacker].zone,
        Zone::Battlefield,
        "the blink must return the attacker to the battlefield"
    );
    assert_ne!(
        fixture.runner.state().objects[&fixture.attacker].incarnation,
        incarnation_before,
        "CR 400.7: the returned object is a new incarnation"
    );
    assert!(
        fixture
            .runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attacking_incarnations_this_combat.contains(&pin)),
        "the declaration ledger keeps pinning the attacking incarnation"
    );
    assert!(
        fixture
            .runner
            .state()
            .lki_by_incarnation
            .get(&fixture.attacker)
            .is_some_and(|history| history.contains_key(&incarnation_before)),
        "the exit must have captured the attacking incarnation's snapshot"
    );

    fixture
        .runner
        .state_mut()
        .objects
        .get_mut(&fixture.attacker)
        .expect("attacker exists")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    fixture.runner.state_mut().layers_dirty.mark_full();
    layers::evaluate_layers(fixture.runner.state_mut());
    let object = &fixture.runner.state().objects[&fixture.attacker];
    assert_eq!(
        (object.power, object.toughness),
        (Some(1), Some(3)),
        "the returned incarnation no longer matches the chosen number"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before - 3,
        "the pinned attacking incarnation (0/2) still qualifies: damage resolves"
    );
}

/// Row F1(b): a matching-at-fire attacker becomes 1/3 (no longer matching) and
/// then blinks. The pinned attacking incarnation's last-battlefield P/T (1/3)
/// does not qualify, so the ability is removed; without the pin the returned
/// live 0/2 would wrongly resolve it.
#[test]
fn blinked_attacker_is_removed_by_the_attacking_incarnations_lki() {
    let mut fixture = squall_fixture(2, 0, 2);
    let life_before = fixture.runner.life(P1);
    declare_squall_attack(&mut fixture);
    assert_eq!(
        stack_condition_for_source(&fixture.runner, fixture.squall),
        Some(chosen_number_condition(ControllerRef::Opponent)),
        "reach guard: the retained condition is on the stack before the change"
    );

    let pin = attacking_pin(&fixture.runner, fixture.attacker);
    let incarnation_before = pin.incarnation;

    fixture
        .runner
        .state_mut()
        .objects
        .get_mut(&fixture.attacker)
        .expect("attacker exists")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    fixture.runner.state_mut().layers_dirty.mark_full();
    layers::evaluate_layers(fixture.runner.state_mut());
    let object = &fixture.runner.state().objects[&fixture.attacker];
    assert_eq!(
        (object.power, object.toughness),
        (Some(1), Some(3)),
        "the attacking incarnation no longer matches before it leaves"
    );

    blink_attacker(&mut fixture.runner, fixture.attacker);

    assert_eq!(
        fixture.runner.state().objects[&fixture.attacker].zone,
        Zone::Battlefield,
        "the blink must return the attacker to the battlefield"
    );
    assert_ne!(
        fixture.runner.state().objects[&fixture.attacker].incarnation,
        incarnation_before,
        "CR 400.7: the returned object is a new incarnation"
    );
    let returned = &fixture.runner.state().objects[&fixture.attacker];
    assert_eq!(
        (returned.power, returned.toughness),
        (Some(0), Some(2)),
        "the returned incarnation is back to its printed 0/2 with no counters"
    );
    let pinned_lki = fixture
        .runner
        .state()
        .lki_by_incarnation
        .get(&fixture.attacker)
        .and_then(|history| history.get(&incarnation_before))
        .expect("the attacking incarnation's exit-time snapshot exists");
    assert_eq!(
        (pinned_lki.power, pinned_lki.toughness),
        (Some(1), Some(3)),
        "the attacking incarnation's last-battlefield P/T no longer qualifies"
    );

    fixture.runner.advance_until_stack_empty();
    assert_eq!(
        fixture.runner.life(P1),
        life_before,
        "the pinned attacking incarnation does not qualify: no damage"
    );
}
