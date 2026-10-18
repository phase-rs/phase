//! Player-scoped loyalty triggers through fresh parsing and shipped card data.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, ControllerRef, Effect, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{ActivatedAbilityKind, GameEvent};
use engine::types::format::FormatConfig;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::support;

const GIDEON: &str = "Ward—Discard a card.\nWhenever a creature an opponent controls enters, Gideon deals 1 damage to that player.\nWhenever an opponent activates a loyalty ability, Gideon deals 1 damage to that player.";
const CHANDRA: &str = "[+1]: Elementals you control get +2/+0 until end of turn.\n[−1]: Add {R}{R}.\n[−2]: Chandra deals 2 damage to any target.";
const KERAL: &str = "Whenever you activate a loyalty ability of a Chandra planeswalker, this creature deals 1 damage to each opponent.";
const JACE: &str = "[+2]: Each player draws a card.\n[−1]: Target player draws a card.\n[−10]: Target player mills twenty cards.";
const BRAND: &str = "Gain control of all permanents you own. (This effect lasts indefinitely.)\nCycling {2} ({2}, Discard this card: Draw a card.)";

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    scenario
}

fn add_gideon(scenario: &mut GameScenario, generated: bool) -> ObjectId {
    if generated {
        let db = support::shared_card_db().expect("generated integration fixture is required");
        assert_eq!(
            db.get_face_by_name("Gideon the Oathless")
                .expect("generated Gideon record")
                .oracle_text
                .as_deref(),
            Some(GIDEON)
        );
        scenario.add_real_card(P0, "Gideon the Oathless", Zone::Battlefield, db)
    } else {
        scenario
            .add_creature(P0, "Gideon the Oathless", 3, 3)
            .from_oracle_text(GIDEON)
            .id()
    }
}

fn add_chandra(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, "Chandra, Novice Pyromancer", 0, 0)
        .as_planeswalker_with_loyalty("Chandra", 5)
        .from_oracle_text(CHANDRA)
        .id()
}

fn set_turn(runner: &mut GameRunner, player: PlayerId) {
    assert!(runner.state().stack.is_empty());
    // CR 606.3: a legal loyalty activation needs the activator's main phase.
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.phase = Phase::PreCombatMain;
    state.waiting_for = WaitingFor::Priority { player };
}

fn assert_full_gideon(runner: &GameRunner, source: ObjectId) {
    let object = &runner.state().objects[&source];
    assert!(object
        .keywords
        .iter()
        .any(|kw| matches!(kw, Keyword::Ward(WardCost::DiscardCard))));
    for mode in [
        TriggerMode::ChangesZone,
        TriggerMode::LoyaltyAbilityActivated,
    ] {
        let trigger = object
            .base_trigger_definitions
            .iter()
            .find(|trigger| trigger.mode == mode)
            .expect("both printed Gideon triggers must be recognized");
        if mode == TriggerMode::LoyaltyAbilityActivated {
            assert_eq!(
                trigger.valid_target,
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent)
                ))
            );
        }
        let execute = trigger.execute.as_deref().expect("printed effect");
        for ability in
            std::iter::successors(Some(execute), |ability| ability.sub_ability.as_deref())
        {
            assert!(!matches!(
                ability.effect.as_ref(),
                Effect::Unimplemented { .. }
            ));
        }
    }
    assert_eq!(
        object
            .base_trigger_definitions
            .iter()
            .filter(|trigger| matches!(
                trigger.mode,
                TriggerMode::ChangesZone | TriggerMode::LoyaltyAbilityActivated
            ))
            .count(),
        2
    );
}

fn assert_activation(
    events: &[GameEvent],
    player: PlayerId,
    source: ObjectId,
    kind: ActivatedAbilityKind,
) {
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event,
        GameEvent::AbilityActivated { player_id, source_id, kind: actual, .. }
        if *player_id == player && *source_id == source && *actual == kind))
            .count(),
        1
    );
}

fn assert_damage(events: &[GameEvent], source: ObjectId, player: PlayerId, count: usize) {
    // CR 120.2b + CR 120.3a: Gideon is the damage source; the event player loses 1.
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event,
        GameEvent::DamageDealt { source_id, target: TargetRef::Player(actual), amount: 1, .. }
        if *source_id == source && *actual == player))
            .count(),
        count
    );
}

fn resolve_stack(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..48 {
        if runner.state().stack.is_empty() {
            return events;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority")
                .events,
        );
    }
    panic!("stack did not empty");
}

fn activation_triggers_on_stack(
    runner: &GameRunner,
    source: ObjectId,
    player: PlayerId,
    walker: ObjectId,
) -> usize {
    // CR 603.3a + CR 602.2a: recipient is the triggering activation's player.
    runner.state().stack.iter().filter(|entry| entry.source_id == source
        && matches!(&entry.kind, StackEntryKind::TriggeredAbility {
            trigger_event: Some(GameEvent::AbilityActivated { player_id, source_id, kind: ActivatedAbilityKind::Loyalty, .. }), ..
        } if *player_id == player && *source_id == walker)).count()
}

fn opponent_loyalty(generated: bool) {
    let mut scenario = main_phase();
    let source = add_gideon(&mut scenario, generated);
    let walker = add_chandra(&mut scenario, P1);
    let mut runner = scenario.build();
    assert_full_gideon(&runner, source);
    set_turn(&mut runner, P1);
    let before: Vec<_> = runner
        .state()
        .players
        .iter()
        .map(|player| player.life)
        .collect();
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
        })
        .expect("legal Chandra +1 activation")
        .events;
    assert_activation(&events, P1, walker, ActivatedAbilityKind::Loyalty);
    assert_eq!(
        runner.state().objects[&walker].counters[&CounterType::Loyalty],
        6
    );
    let matching_trigger_count = activation_triggers_on_stack(&runner, source, P1, walker);
    events.extend(resolve_stack(&mut runner));
    assert_eq!(
        runner.state().players[1].life,
        before[1] - 1,
        "opponent loyalty activation must deal exactly one damage to its activator"
    );
    assert_eq!(
        matching_trigger_count, 1,
        "trigger must carry the actual activation event"
    );
    assert_eq!(runner.state().players[0].life, before[0]);
    assert_damage(&events, source, P1, 1);
    assert_damage(&events, source, P0, 0);
}

// CR 606.2 + CR 603.2: a legal opponent loyalty activation triggers automatically.
#[test]
fn gideon_opponent_loyalty_activation_deals_one_to_activator() {
    opponent_loyalty(false);
}

#[test]
fn gideon_generated_fixture_loyalty_trigger_runs() {
    opponent_loyalty(true);
}

// CR 109.5: Gideon's opponent scope is relative to its controller.
#[test]
fn gideon_own_loyalty_activation_does_not_trigger() {
    opponent_loyalty(false);
    let mut scenario = main_phase();
    let source = add_gideon(&mut scenario, false);
    let walker = add_chandra(&mut scenario, P0);
    let mut runner = scenario.build();
    assert_full_gideon(&runner, source);
    let outcome = runner.activate(walker, 0).resolve();
    assert_activation(outcome.events(), P0, walker, ActivatedAbilityKind::Loyalty);
    outcome.assert_counters(walker, CounterType::Loyalty, 6);
    outcome.assert_life_delta(P0, 0);
    outcome.assert_life_delta(P1, 0);
    assert_damage(outcome.events(), source, P0, 0);
    assert_damage(outcome.events(), source, P1, 0);
}

// CR 606.2: an ordinary or mana ability with no loyalty symbol is not loyalty.
#[test]
fn gideon_ordinary_and_mana_activations_do_not_trigger() {
    opponent_loyalty(false);
    let mut scenario = main_phase();
    let source = add_gideon(&mut scenario, false);
    let pyromancer = scenario
        .add_creature(P1, "Prodigal Pyromancer", 1, 1)
        .from_oracle_text("{T}: This creature deals 1 damage to any target.")
        .id();
    let elves = scenario
        .add_creature(P1, "Llanowar Elves", 1, 1)
        .from_oracle_text("{T}: Add {G}.")
        .id();
    let mut runner = scenario.build();
    assert_full_gideon(&runner, source);
    set_turn(&mut runner, P1);
    let normal = runner.activate(pyromancer, 0).target_player(P0).resolve();
    assert_activation(
        normal.events(),
        P1,
        pyromancer,
        ActivatedAbilityKind::Normal,
    );
    normal.assert_life_delta(P0, -1);
    normal.assert_life_delta(P1, 0);
    assert_damage(normal.events(), pyromancer, P0, 1);
    let mana = runner.activate(elves, 0).resolve();
    assert_activation(mana.events(), P1, elves, ActivatedAbilityKind::Mana);
    assert_eq!(mana.mana_pool_color(P1, ManaType::Green), 1);
    mana.assert_life_delta(P0, 0);
    mana.assert_life_delta(P1, 0);
    for outcome in [&normal, &mana] {
        assert!(!outcome.events().iter().any(|event| matches!(event,
            GameEvent::DamageDealt { source_id, .. } if *source_id == source)));
        assert!(!outcome
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == source));
    }
}

fn keral_activation(legacy: bool, player: PlayerId, chandra: bool) -> Outcome {
    let mut scenario = main_phase();
    let source = scenario
        .add_creature(P0, "Keral Keep Disciples", 4, 3)
        .from_oracle_text(KERAL)
        .id();
    let walker = if chandra {
        add_chandra(&mut scenario, player)
    } else {
        scenario
            .add_creature(player, "Jace Beleren", 0, 0)
            .as_planeswalker_with_loyalty("Jace", 3)
            .from_oracle_text(JACE)
            .id()
    };
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&source].base_trigger_definitions[0].valid_target,
        Some(TargetFilter::Controller)
    );
    if legacy {
        let object = runner.state_mut().objects.get_mut(&source).unwrap();
        let mut definitions = object.base_trigger_definitions.as_ref().clone();
        definitions[0].valid_target = None;
        object
            .install_trigger_base_definitions(Arc::new(definitions))
            .expect("legacy actor encoding");
        engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), source);
    }
    set_turn(&mut runner, player);
    let outcome = runner.activate(walker, 0).resolve();
    assert_activation(
        outcome.events(),
        player,
        walker,
        ActivatedAbilityKind::Loyalty,
    );
    outcome.assert_counters(walker, CounterType::Loyalty, if chandra { 6 } else { 5 });
    if !chandra {
        outcome.assert_hand_drawn(P0, 1);
        outcome.assert_hand_drawn(P1, 1);
    }
    outcome
}

#[test]
fn loyalty_you_and_legacy_none_scopes_follow_controller() {
    // CR 109.5: Keral's "you" means its controller, for either encoding.
    for legacy in [false, true] {
        let own = keral_activation(legacy, P0, true);
        own.assert_life_delta(P1, -1);
        own.assert_life_delta(P0, 0);
        for (player, chandra) in [(P1, true), (P0, false)] {
            let refused = keral_activation(legacy, player, chandra);
            refused.assert_life_delta(P0, 0);
            refused.assert_life_delta(P1, 0);
        }
    }
}

#[test]
fn loyalty_any_player_subject_runs_for_both_players() {
    // Synthetic grammar fixture, not the Oracle of a printed card.
    for player in [P0, P1] {
        let mut scenario = main_phase();
        scenario
            .add_creature(P0, "Synthetic loyalty observer", 1, 3)
            .from_oracle_text("Whenever a player activates a loyalty ability, draw a card.");
        let walker = add_chandra(&mut scenario, player);
        let mut runner = scenario.build();
        set_turn(&mut runner, player);
        let outcome = runner.activate(walker, 0).resolve();
        assert_activation(
            outcome.events(),
            player,
            walker,
            ActivatedAbilityKind::Loyalty,
        );
        outcome.assert_counters(walker, CounterType::Loyalty, 6);
        outcome.assert_hand_drawn(P0, 1);
        outcome.assert_hand_drawn(P1, 0);
    }
}

#[test]
fn gideon_controller_and_event_player_authority() {
    let mut scenario = main_phase();
    let source = scenario
        .add_creature(P0, "Gideon the Oathless", 3, 3)
        .from_oracle_text(GIDEON)
        .controlled_by(P1)
        .id();
    let walker = add_chandra(&mut scenario, P0);
    let brand = scenario
        .add_spell_to_hand_from_oracle(P0, "Brand", true, BRAND)
        .id();
    let mut runner = scenario.build();
    assert_full_gideon(&runner, source);
    let events = runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
        })
        .expect("legal own Chandra activation")
        .events;
    assert_activation(&events, P0, walker, ActivatedAbilityKind::Loyalty);
    assert_eq!(activation_triggers_on_stack(&runner, source, P0, walker), 1);
    assert_eq!(
        runner.state().objects[&walker].counters[&CounterType::Loyalty],
        6
    );
    // CR 603.3a + CR 113.7a: Brand changes the source's controller after firing.
    // The trigger remains independent; its stored event still names P0.
    let outcome = runner.cast(brand).resolve();
    outcome.assert_controls(P0, source);
    outcome.assert_life_delta(P0, -1);
    outcome.assert_life_delta(P1, 0);
    assert_damage(outcome.events(), source, P0, 1);
}

#[test]
fn gideon_multiplayer_damage_follows_each_event_activator() {
    for player in [P1, PlayerId(2)] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = add_gideon(&mut scenario, false);
        let walker = add_chandra(&mut scenario, player);
        let mut runner = scenario.build();
        assert_full_gideon(&runner, source);
        set_turn(&mut runner, player);
        let outcome = runner.activate(walker, 0).resolve();
        assert_activation(
            outcome.events(),
            player,
            walker,
            ActivatedAbilityKind::Loyalty,
        );
        outcome.assert_counters(walker, CounterType::Loyalty, 6);
        for seat in [P0, P1, PlayerId(2)] {
            outcome.assert_life_delta(seat, if seat == player { -1 } else { 0 });
            assert_damage(outcome.events(), source, seat, usize::from(seat == player));
        }
    }
}

#[test]
fn gideon_teammate_excluded_enemy_activation_triggers() {
    // CR 102.3: different seats on the same team are not opponents.
    for (player, expected) in [(P1, 0), (PlayerId(2), 1)] {
        let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = add_gideon(&mut scenario, false);
        let walker = add_chandra(&mut scenario, player);
        let mut runner = scenario.build();
        set_turn(&mut runner, player);
        let outcome = runner.activate(walker, 0).resolve();
        assert_activation(
            outcome.events(),
            player,
            walker,
            ActivatedAbilityKind::Loyalty,
        );
        outcome.assert_counters(walker, CounterType::Loyalty, 6);
        assert_damage(outcome.events(), source, player, expected);
    }
}

#[test]
fn gideon_creature_entry_deals_one_to_entrant_controller() {
    // CR 603.6a: the first printed trigger observes an opponent's creature entry.
    for generated in [false, true] {
        for player in [P1, P0] {
            let mut scenario = main_phase();
            let source = add_gideon(&mut scenario, generated);
            let db = support::shared_card_db().expect("real Grizzly Bears fixture is required");
            let bears = scenario.add_real_card(player, "Grizzly Bears", Zone::Hand, db);
            scenario.with_mana_pool(
                player,
                (0..2)
                    .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
                    .collect(),
            );
            let mut runner = scenario.build();
            assert_full_gideon(&runner, source);
            set_turn(&mut runner, player);
            let outcome = runner.cast(bears).resolve();
            outcome.assert_zone(&[bears], Zone::Battlefield);
            outcome.assert_life_delta(P0, 0);
            outcome.assert_life_delta(P1, if player == P1 { -1 } else { 0 });
            assert_damage(outcome.events(), source, player, usize::from(player == P1));
        }
    }
}

#[test]
fn gideon_ward_discard_payment() {
    // CR 702.21a: paid Ward allows the spell; declined Ward counters it.
    for generated in [false, true] {
        for pay in [true, false] {
            let mut scenario = main_phase();
            let source = add_gideon(&mut scenario, generated);
            let bolt = scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Lightning Bolt",
                    true,
                    "Lightning Bolt deals 3 damage to any target.",
                )
                .id();
            let discard = scenario.add_card_to_hand(P1, "Forest");
            let mut runner = scenario.build();
            assert_full_gideon(&runner, source);
            set_turn(&mut runner, P1);
            runner.cast(bolt).target_objects(&[source]).commit();
            assert!(runner.state().stack.iter().any(|entry| entry.id == bolt));
            assert!(runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == source
                    && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })));
            runner.advance_until_stack_empty();
            assert!(
                matches!(&runner.state().waiting_for, WaitingFor::UnlessPayment {
                player, cost: AbilityCost::Discard { .. }, .. } if *player == P1)
            );
            runner
                .act(GameAction::PayUnlessCost { pay })
                .expect("Ward payment decision");
            if pay {
                assert!(matches!(&runner.state().waiting_for,
                    WaitingFor::WardDiscardChoice { player, cards, .. }
                    if *player == P1 && cards.contains(&discard)));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discard],
                    })
                    .expect("pay Ward by discarding");
                assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
            }
            let events = resolve_stack(&mut runner);
            assert_eq!(
                runner.state().objects[&source].zone,
                if pay {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
            assert_eq!(runner.state().objects[&bolt].zone, Zone::Graveyard);
            if !pay {
                assert_eq!(runner.state().objects[&discard].zone, Zone::Hand);
            }
            if pay {
                assert!(events.iter().any(|event| matches!(event,
                GameEvent::DamageDealt { source_id, target: TargetRef::Object(target), amount: 3, .. }
                if *source_id == bolt && *target == source)));
            }
        }
    }
}

// SHAPE: the exact full card parses independently of the generated database.
#[test]
fn gideon_fresh_parse_keeps_all_printed_clauses() {
    let parsed = parse_oracle_text(
        GIDEON,
        "Gideon the Oathless",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Mercenary".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 2);
    assert_eq!(
        parsed.triggers[1].mode,
        TriggerMode::LoyaltyAbilityActivated
    );
    for trigger in &parsed.triggers {
        assert!(matches!(
            trigger
                .execute
                .as_deref()
                .expect("printed effect")
                .effect
                .as_ref(),
            Effect::DealDamage { .. }
        ));
    }
}
