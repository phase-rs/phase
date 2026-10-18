//! Fixed-output damage thresholds use the current proposed event's amount.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, Comparator, DamageModification, DamageTargetFilter, DamageTargetPlayerScope,
    Effect, GameRestriction, PreventionAmount, QuantityExpr, QuantityRef, ReplacementCondition,
    ReplacementDefinition, RestrictionExpiry, TargetFilter, TargetRef, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use super::rules::run_combat;

// Verbatim MTGJSON Oracle text, pinned 2026-10-04. Flame Javelin includes its
// printed reminder text so the ordinary public parser handles that boundary too.
const PRESENCE: &str = "If a source would deal 4 or more damage to a permanent or player, that source deals 3 damage to that permanent or player instead.";
const AMULET: &str = "At the beginning of your upkeep, sacrifice this artifact unless you pay {3}.\nIf an instant or sorcery source would deal 3 or more damage to you, it deals 2 damage to you instead.";
const FURNACE: &str = "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.";
const JAVELIN: &str = "({2/R} can be paid with any two mana or with {R}. This card's mana value is 6.)\nFlame Javelin deals 4 damage to any target.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const SPIKE: &str = "Lava Spike deals 3 damage to target player or planeswalker.";
const SHOCK: &str = "Shock deals 2 damage to any target.";

fn presence(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(controller, "Divine Presence", PRESENCE)
        .id()
}

fn amulet(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_artifact_from_oracle(controller, "Forethought Amulet", AMULET)
        .id()
}

fn spell(scenario: &mut GameScenario, name: &str, oracle: &str, instant: bool) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, oracle)
        .id()
}

fn assert_damage(events: &[GameEvent], source: ObjectId, target: TargetRef, amount: u32) {
    assert!(
        events.iter().any(|event| matches!(event,
            GameEvent::DamageDealt { source_id, target: actual, amount: dealt, .. }
            if *source_id == source && *actual == target && *dealt == amount
        )),
        "expected unchanged damage source/recipient and amount {amount}: {events:?}"
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::DamagePrevented { .. })));
}

#[test]
fn shape_printed_thresholds_keep_source_recipient_and_upkeep() {
    for (name, oracle, kind, threshold, output) in [
        ("Divine Presence", PRESENCE, "Enchantment", 4, 3),
        ("Forethought Amulet", AMULET, "Artifact", 3, 2),
    ] {
        let parsed = parse_oracle_text(oracle, name, &[], &[kind.to_string()], &[]);
        assert!(
            parsed.abilities.is_empty(),
            "supported printed statics must not leave a failure marker"
        );
        assert_eq!(parsed.replacements.len(), 1);
        let definition = &parsed.replacements[0];
        assert_eq!(definition.event, ReplacementEvent::DamageDone);
        assert_eq!(
            definition.damage_modification,
            Some(DamageModification::SetTo { value: output })
        );
        assert_eq!(
            definition.condition,
            Some(ReplacementCondition::OnlyIfQuantity {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: threshold },
                active_player_req: None,
            })
        );
        assert!(definition.execute.is_none());
        assert!(definition.expiry.is_none());
        if name == "Forethought Amulet" {
            assert_eq!(
                definition.damage_target_filter,
                Some(DamageTargetFilter::Player {
                    player: DamageTargetPlayerScope::Controller,
                })
            );
            let Some(TargetFilter::Typed(filter)) = &definition.damage_source_filter else {
                panic!("an instant or sorcery source must have a typed disjunction");
            };
            assert_eq!(
                filter.type_filters,
                vec![TypeFilter::AnyOf(vec![
                    TypeFilter::Instant,
                    TypeFilter::Sorcery
                ])]
            );
            assert_eq!(parsed.triggers.len(), 1);
            let upkeep = &parsed.triggers[0];
            assert!(matches!(
                *upkeep.execute.as_ref().expect("upkeep effect").effect,
                Effect::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ));
            assert!(matches!(&upkeep.unless_pay.as_ref().unwrap().cost,
                AbilityCost::Mana { cost } if cost.mana_value() == 3));
        } else {
            assert!(definition.damage_source_filter.is_none());
            assert!(matches!(
                definition.damage_target_filter,
                Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                    player: DamageTargetPlayerScope::Any,
                    permanent_type: None,
                    ..
                })
            ));
        }
    }
}

#[test]
fn shape_unsupported_threshold_clauses_fail_closed_in_public_parser() {
    // Positive reach guard uses the same public route as every rejected sibling.
    let positive = parse_oracle_text(
        PRESENCE,
        "Divine Presence",
        &[],
        &["Enchantment".into()],
        &[],
    );
    assert_eq!(
        positive.replacements[0].damage_modification,
        Some(DamageModification::SetTo { value: 3 })
    );
    assert!(positive.abilities.is_empty());
    for text in [
        "If a source would deal 4 or more damage to a permanent or player, that source deals 3 damage to you instead.",
        "If a source would deal 4 or more damage to a permanent or player, that source deals 3 damage to a permanent or player instead.",
        "If a source would deal 4 or more damage to a permanent or player, that source deals 3 damage to that permanent or player instead, then draw a card.",
        "If a source would deal X or more damage to a permanent or player, that source deals 3 damage to that permanent or player instead.",
        "If a source would deal 4 or more damage to a permanent or player, that source deals X damage to that permanent or player instead.",
        "If an unknown source would deal 4 or more damage to you, it deals 3 damage to you instead.",
        "If a source would deal 4 or more damage to a permanent or player this turn, that source deals 3 damage to that permanent or player instead.",
        "If a source would deal 4 or more combat damage to you, it deals 3 damage to you instead.",
        "If a source would deal 2147483648 or more damage to you, it deals 3 damage to you instead.",
        "If a source would deal 4 or more damage to you, it deals 3 damage to you instead if you would draw a card.",
    ] {
        let parsed = parse_oracle_text(text, "Threshold Probe", &[], &["Enchantment".into()], &[]);
        assert!(parsed.replacements.is_empty(), "unsupported threshold must not become a supported stub: {text}");
        assert!(parsed.abilities.iter().any(|ability| matches!(*ability.effect, Effect::Unimplemented { .. })),
            "unsupported threshold must remain explicitly unsupported: {text}");
    }
}

#[test]
fn presence_caps_real_spell_damage_at_threshold_and_preserves_identity() {
    // CR 120.4b: damage is dealt at the modified amount, with its original source.
    for (with_presence, name, oracle, expected) in [
        (true, "Flame Javelin", JAVELIN, 3),
        (true, "Lightning Bolt", BOLT, 3),
        (false, "Flame Javelin", JAVELIN, 4),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if with_presence {
            presence(&mut scenario, P1);
        }
        let source = spell(&mut scenario, name, oracle, true);
        let mut runner = scenario.build();
        let outcome = runner.cast(source).target_player(P1).resolve();
        outcome.assert_life_delta(P1, -(expected as i32));
        assert_damage(outcome.events(), source, TargetRef::Player(P1), expected);
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    presence(&mut scenario, P1);
    let victim = scenario.add_creature(P1, "Large Creature", 0, 10).id();
    let source = spell(&mut scenario, "Flame Javelin", JAVELIN, true);
    let mut runner = scenario.build();
    let outcome = runner.cast(source).target_object(victim).resolve();
    assert_eq!(outcome.state().objects[&victim].damage_marked, 3);
    assert_damage(outcome.events(), source, TargetRef::Object(victim), 3);
}

#[test]
fn presence_persists_between_events_and_stops_when_its_host_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = presence(&mut scenario, P1);
    let sources: Vec<_> = (0..3)
        .map(|_| spell(&mut scenario, "Flame Javelin", JAVELIN, true))
        .collect();
    let mut runner = scenario.build();
    // CR 614.5 + CR 611.3b: once per event, available again for the next event
    // while its generating permanent remains on the battlefield.
    for source in &sources[..2] {
        runner
            .cast(*source)
            .target_player(P1)
            .resolve()
            .assert_life_delta(P1, -3);
    }
    move_to_zone(runner.state_mut(), host, Zone::Graveyard, &mut Vec::new());
    runner
        .cast(sources[2])
        .target_player(P1)
        .resolve()
        .assert_life_delta(P1, -4);
}

#[test]
fn amulet_filters_instant_sorcery_controller_and_threshold() {
    // CR 609.7c + CR 109.5: the spell's type and the host's current controller
    // determine applicability, rather than the damage source's controller.
    for (name, oracle, instant, target, expected) in [
        ("Lightning Bolt", BOLT, true, P1, 2),
        ("Lava Spike", SPIKE, false, P1, 2),
        ("Shock", SHOCK, true, P1, 2),
        ("Lightning Bolt", BOLT, true, P0, 3),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        amulet(&mut scenario, P1);
        let source = spell(&mut scenario, name, oracle, instant);
        let mut runner = scenario.build();
        let outcome = runner.cast(source).target_player(target).resolve();
        outcome.assert_life_delta(target, -expected);
        assert_damage(
            outcome.events(),
            source,
            TargetRef::Player(target),
            expected as u32,
        );
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    amulet(&mut scenario, P1);
    let attacker = scenario.add_creature(P0, "Creature Source", 3, 3).id();
    let mut runner = scenario.build();
    let before = runner.life(P1);
    run_combat(&mut runner, vec![attacker], vec![]);
    assert_eq!(runner.life(P1), before - 3);
}

fn choose_source(runner: &mut GameRunner, source: ObjectId) -> Vec<GameEvent> {
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("replacement ordering is pending");
    let index = pending
        .candidates
        .iter()
        .position(|id| id.source == source)
        .expect("the selected source is offered");
    assert_eq!(pending.candidates[index].index, 0);
    runner
        .act(GameAction::ChooseReplacement { index })
        .unwrap()
        .events
}

#[test]
fn current_amount_is_rechecked_after_replacement_ordering() {
    // CR 616.1f + CR 614.5: recheck after each selected replacement, and never
    // apply that exact replacement identity twice to the same damage event.
    for (presence_first, expected) in [(true, 6), (false, 3)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cap = presence(&mut scenario, P1);
        let furnace = scenario
            .add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE)
            .id();
        let source = spell(&mut scenario, "Flame Javelin", JAVELIN, true);
        let mut runner = scenario.build();
        let before = runner.life(P1);
        let outcome = runner.cast(source).target_player(P1).resolve();
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::ReplacementChoice { player, candidate_count: 2, .. } if *player == P1)
        );
        let events = choose_source(&mut runner, if presence_first { cap } else { furnace });
        runner.advance_until_stack_empty();
        assert_eq!(runner.life(P1), before - expected);
        assert_damage(&events, source, TargetRef::Player(P1), expected as u32);
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    presence(&mut scenario, P1);
    scenario.add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE);
    let source = spell(&mut scenario, "Shock", SHOCK, true);
    let mut runner = scenario.build();
    // Initial 2 does not qualify; after doubling to 4 the cap becomes applicable.
    runner
        .cast(source)
        .target_player(P1)
        .resolve()
        .assert_life_delta(P1, -3);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    presence(&mut scenario, P1);
    let subtract = scenario
        .add_creature(P0, "Subtraction Control", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .damage_modification(DamageModification::Minus { value: 2 }),
        )
        .id();
    let source = spell(&mut scenario, "Flame Javelin", JAVELIN, true);
    let mut runner = scenario.build();
    let before = runner.life(P1);
    runner.cast(source).target_player(P1).resolve();
    let events = choose_source(&mut runner, subtract);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P1),
        before - 2,
        "the cap must stop applying after subtraction"
    );
    assert_damage(&events, source, TargetRef::Player(P1), 2);
}

#[test]
fn fixed_amount_replacements_apply_to_unpreventable_damage() {
    // CR 615.1a + CR 615.12: ordinary amount-setting is not prevention.
    for (use_amulet, name, oracle, expected) in [
        (false, "Flame Javelin", JAVELIN, 3),
        (true, "Lightning Bolt", BOLT, 2),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = if use_amulet {
            amulet(&mut scenario, P1)
        } else {
            presence(&mut scenario, P1)
        };
        let source = spell(&mut scenario, name, oracle, true);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: host,
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });
        let outcome = runner.cast(source).target_player(P1).resolve();
        outcome.assert_life_delta(P1, -expected);
        assert_damage(
            outcome.events(),
            source,
            TargetRef::Player(P1),
            expected as u32,
        );
    }
    for (disabled, expected) in [(false, 0), (true, 4)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario
            .add_creature(P1, "Prevention Control", 1, 1)
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All),
            )
            .id();
        let source = spell(&mut scenario, "Flame Javelin", JAVELIN, true);
        let mut runner = scenario.build();
        if disabled {
            runner
                .state_mut()
                .restrictions
                .push(GameRestriction::DamagePreventionDisabled {
                    source: host,
                    expiry: RestrictionExpiry::EndOfTurn,
                    scope: None,
                });
        }
        runner
            .cast(source)
            .target_player(P1)
            .resolve()
            .assert_life_delta(P1, -expected);
    }
}

#[test]
fn amulet_authority_follows_current_controller_among_multiple_hosts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = amulet(&mut scenario, P0);
    let second = amulet(&mut scenario, P1);
    let sources: Vec<_> = (0..3)
        .map(|_| spell(&mut scenario, "Lightning Bolt", BOLT, true))
        .collect();
    let mut runner = scenario.build();
    let outcome = runner.cast(sources[0]).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -2);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "only the damaged player's Amulet is applicable"
    );
    move_to_zone(runner.state_mut(), first, Zone::Graveyard, &mut Vec::new());
    // CR 109.5 + CR 611.3a: a printed static's 'you' follows live controller.
    let object = runner.state_mut().objects.get_mut(&second).unwrap();
    object.base_controller = Some(P0);
    object.controller = P0;
    assert_eq!(object.owner, P1);
    runner
        .cast(sources[1])
        .target_player(P1)
        .resolve()
        .assert_life_delta(P1, -3);
    runner
        .cast(sources[2])
        .target_player(P0)
        .resolve()
        .assert_life_delta(P0, -2);
}
