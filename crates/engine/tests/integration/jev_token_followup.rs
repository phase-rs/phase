//! Synthetic duration consumers of separately printed token definitions.
//! Full Broken Visage's exact-token delayed cleanup belongs to the next consumer.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ShieldKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CONSTRUCTION: &str = "Destroy target nonartifact attacking creature. It can't be regenerated. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness. Target creature gets +1/+1 until end of turn.";
const REGENERABLE: &str = "Destroy target nonartifact attacking creature. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness. Target creature gets +1/+1 until end of turn.";
const DOUBLING: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";

struct Board {
    runner: GameRunner,
    spell: ObjectId,
    b: ObjectId,
    c: ObjectId,
    a: ObjectId,
    response: ObjectId,
    regen: ObjectId,
    modifier: ObjectId,
}

fn board(
    oracle: &str,
    pt: (i32, i32),
    indestructible: bool,
    multiplier: bool,
    response: &str,
) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
    let b = {
        let mut card = scenario.add_creature(P1, "Announced B", pt.0, pt.1);
        if indestructible {
            card.indestructible();
        }
        card.id()
    };
    let c = scenario.add_creature(P0, "Independent C", 11, 13).id();
    let a = scenario.add_creature(P0, "Earlier A", 17, 19).id();
    scenario.add_creature(P0, "Unrelated source-sized object", 17, 19);
    if multiplier {
        scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic duration Spirit", true, oracle)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic response", true, response)
        .id();
    let regen = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic shield", true, "Regenerate target creature.")
        .id();
    let modifier = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic unequal modifier",
            true,
            "Target creature gets +3/+1 until end of turn.",
        )
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(b, AttackTarget::Player(P0))])
        .unwrap();
    runner.advance_to_priority_window();
    if runner.state().priority_player != P0 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    for _ in 0..10 {
        let _ = runner.state_mut().add_mana_to_pool(
            P0,
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        );
    }
    Board {
        runner,
        spell,
        b,
        c,
        a,
        response,
        regen,
        modifier,
    }
}

fn assert_created(
    state: &GameState,
    events: &[GameEvent],
    source: ObjectId,
    count: usize,
    pt: (i32, i32),
) -> Vec<ObjectId> {
    let ids: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated {
                object_id,
                source_id,
                ..
            } if *source_id == source => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), count, "{events:#?}");
    let entries: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: None,
                to: Zone::Battlefield,
                ..
            } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        entries.len(),
        count,
        "no creation-entry/SBA counterfeit: {events:#?}"
    );
    for id in &ids {
        assert!(entries.contains(id));
        let token = &state.objects[id];
        assert!(token.is_token);
        assert_eq!(token.zone, Zone::Battlefield);
        assert_eq!(
            (token.base_power, token.base_toughness),
            (Some(pt.0), Some(pt.1))
        );
        // CR 111.2: the creating player owns and controls the token.
        assert_eq!((token.owner, token.controller), (P0, P0));
        assert_eq!(token.name, "Spirit");
    }
    ids
}

#[test]
fn duration_target_defined_spirit_uses_departure_pt() {
    let mut b = board(
        CONSTRUCTION,
        (2, 5),
        false,
        false,
        "Return target creature to its owner's hand.",
    );
    b.runner
        .state_mut()
        .objects
        .get_mut(&b.b)
        .unwrap()
        .counters
        .insert(engine::types::counter::CounterType::Plus1Plus1, 2);
    b.runner.cast(b.modifier).target_object(b.b).resolve();
    assert_eq!(
        (
            b.runner.state().objects[&b.b].power,
            b.runner.state().objects[&b.b].toughness
        ),
        (Some(7), Some(8))
    );
    let outcome = b.runner.cast(b.spell).target_objects(&[b.b, b.c]).resolve();
    outcome.assert_zone(&[b.b], Zone::Graveyard);
    // CR 608.2h + CR 613.4c: read the modified departure characteristics.
    let departure = &outcome.state().lki_cache[&b.b];
    assert_eq!((departure.power, departure.toughness), (Some(7), Some(8)));
    let tokens = assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
    assert_eq!(
        (
            outcome.state().objects[&b.c].power,
            outcome.state().objects[&b.c].toughness
        ),
        (Some(12), Some(14))
    );
    b.runner.advance_to_end_step();
    b.runner.advance_to_upkeep();
    assert_eq!(
        (
            b.runner.state().objects[&tokens[0]].power,
            b.runner.state().objects[&tokens[0]].toughness
        ),
        (Some(7), Some(8))
    );
}

#[test]
fn duration_target_defined_spirit_reads_live_when_destroy_skips() {
    let mut b = board(
        CONSTRUCTION,
        (7, 8),
        true,
        false,
        "Return target creature to its owner's hand.",
    );
    let outcome = b.runner.cast(b.spell).target_objects(&[b.b, b.c]).resolve();
    // CR 702.12b: unsuccessful destruction does not suppress independent creation.
    outcome.assert_zone(&[b.b], Zone::Battlefield);
    assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
    assert_eq!(outcome.state().objects[&b.c].power, Some(12));
}

#[test]
fn duration_construction_zero_power_is_real_information() {
    let mut b = board(
        CONSTRUCTION,
        (0, 5),
        false,
        false,
        "Return target creature to its owner's hand.",
    );
    let outcome = b.runner.cast(b.spell).target_objects(&[b.b, b.c]).resolve();
    outcome.assert_zone(&[b.b], Zone::Graveyard);
    assert_created(outcome.state(), outcome.events(), b.spell, 1, (0, 5));
    assert_eq!(outcome.state().objects[&b.c].power, Some(12));
}

fn interrupted(all: bool, multiplier: bool) {
    let response = if all {
        "Return all creatures to their owners' hands."
    } else {
        "Return target creature to its owner's hand."
    };
    let mut b = board(CONSTRUCTION, (7, 8), false, multiplier, response);
    let mut committed = b.runner.cast(b.spell).target_objects(&[b.b, b.c]).commit();
    let ability = committed.state().stack.last().unwrap().ability().unwrap();
    let selected: Vec<_> = std::iter::successors(Some(ability), |a| a.sub_ability.as_deref())
        .flat_map(|a| a.targets.iter())
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .collect();
    assert!(selected.contains(&b.b) && selected.contains(&b.c));
    assert!(!ability.aligned_target_pins().is_empty());
    assert_eq!(committed.state().objects[&b.spell].zone, Zone::Stack);
    let outcome = if all {
        committed.cast(b.response).resolve()
    } else {
        committed.cast(b.response).target_object(b.b).resolve()
    };
    outcome.assert_zone(&[b.b], Zone::Hand);
    assert_created(outcome.state(), outcome.events(), b.spell, 0, (0, 0));
    assert!(outcome.state().objects.values().all(|o| !o.is_token));
    // CR 608.2b/c + CR 111.3: C proves resolution despite B's missing information.
    if all {
        outcome.assert_zone(&[b.c], Zone::Hand);
    } else {
        assert_eq!(outcome.state().objects[&b.c].zone, Zone::Battlefield);
        assert_eq!(
            (
                outcome.state().objects[&b.c].power,
                outcome.state().objects[&b.c].toughness
            ),
            (Some(12), Some(14))
        );
    }
}

#[test]
fn duration_construction_prior_target_illegal_later_target_legal() {
    interrupted(false, false);
}

#[test]
fn duration_construction_all_targets_illegal_creates_nothing() {
    interrupted(true, false);
}

#[test]
fn duration_target_definition_replacement_uses_same_availability() {
    interrupted(false, true);
    let mut b = board(
        CONSTRUCTION,
        (0, 5),
        false,
        true,
        "Return target creature to its owner's hand.",
    );
    let outcome = b.runner.cast(b.spell).target_objects(&[b.b, b.c]).resolve();
    assert_created(outcome.state(), outcome.events(), b.spell, 2, (0, 5));
    assert_eq!(outcome.state().objects[&b.c].power, Some(12));
}

#[test]
fn duration_construction_no_regeneration_rider_reaches_destroy() {
    for (oracle, expected_zone) in [
        (CONSTRUCTION, Zone::Graveyard),
        (REGENERABLE, Zone::Battlefield),
    ] {
        let mut b = board(
            oracle,
            (7, 8),
            false,
            false,
            "Return target creature to its owner's hand.",
        );
        b.runner.cast(b.regen).target_object(b.b).resolve();
        assert!(b.runner.state().objects[&b.b]
            .replacement_definitions
            .iter()
            .any(|r| r.shield_kind == ShieldKind::Regeneration));
        let outcome = b.runner.cast(b.spell).target_objects(&[b.b, b.c]).resolve();
        outcome.assert_zone(&[b.b], expected_zone);
        assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
        assert_eq!(outcome.state().objects[&b.c].power, Some(12));
        if expected_zone == Zone::Battlefield {
            assert!(outcome.state().objects[&b.b].tapped);
        }
    }
}

#[test]
fn duration_token_definition_reads_nearest_announced_occurrence() {
    let oracle = format!("Tap target creature. {CONSTRUCTION}");
    let mut b = board(
        &oracle,
        (7, 8),
        false,
        false,
        "Return target creature to its owner's hand.",
    );
    let outcome = b
        .runner
        .cast(b.spell)
        .target_objects(&[b.a, b.b, b.c])
        .resolve();
    assert!(outcome.state().objects[&b.a].tapped);
    outcome.assert_zone(&[b.b], Zone::Graveyard);
    assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
    assert_eq!(outcome.state().objects[&b.c].power, Some(12));
}

#[test]
fn duration_token_definition_existing_wire_values_roundtrip() {
    for legacy in [false, true] {
        let mut b = board(
            CONSTRUCTION,
            (7, 8),
            false,
            false,
            "Return target creature to its owner's hand.",
        );
        let committed = b.runner.cast(b.spell).target_objects(&[b.b, b.c]).commit();
        let mut json = serde_json::to_value(committed.state()).unwrap();
        if legacy {
            remove_pins(&mut json);
        }
        let state: GameState = serde_json::from_value(json).unwrap();
        assert_eq!(state.objects[&b.spell].zone, Zone::Stack);
        let mut resumed = GameRunner::from_state(state);
        let mut events = Vec::new();
        for _ in 0..16 {
            if resumed.state().stack.is_empty() {
                break;
            }
            events.extend(resumed.act(GameAction::PassPriority).unwrap().events);
        }
        assert!(resumed.state().stack.is_empty());
        assert_created(resumed.state(), &events, b.spell, 1, (7, 8));
        assert_eq!(resumed.state().objects[&b.c].power, Some(12));
    }
}

fn remove_pins(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            fields.remove("target_pins");
            for value in fields.values_mut() {
                remove_pins(value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                remove_pins(value);
            }
        }
        _ => {}
    }
}

#[test]
fn source_defined_twin_preserves_source_despite_prior_target() {
    // Explicitly synthetic source sibling; full Gemini's separate delayed
    // cleanup is not asserted as this regression's new behavior.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Synthetic source Twin", 17, 19).from_oracle_text("Whenever this creature attacks, tap target creature. Create a colorless Construct artifact creature token named Twin that's attacking. Its power is equal to this creature's power and its toughness is equal to this creature's toughness.").id();
    let target = scenario
        .add_creature(P1, "Unrelated selected creature", 7, 8)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let mut events = runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .unwrap()
        .events;
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::TargetSelection { .. }
        ) {
            events.extend(
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .unwrap()
                    .events,
            );
        } else if !runner.state().stack.is_empty() {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        } else {
            break;
        }
    }
    assert!(runner.state().objects[&target].tapped);
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token)
        .collect();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].name, "Twin");
    assert_eq!(
        (tokens[0].base_power, tokens[0].base_toughness),
        (Some(17), Some(19))
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::TokenCreated { source_id, .. } if *source_id == source)));
}

#[test]
fn duration_target_qualifiers_are_checked_before_initial_resolution() {
    for (response, needs_shield) in [
        (
            "Target creature becomes an artifact in addition to its other types until end of turn.",
            false,
        ),
        ("Destroy target creature.", true),
    ] {
        let mut b = board(CONSTRUCTION, (7, 8), false, false, response);
        if needs_shield {
            b.runner.cast(b.regen).target_object(b.b).resolve();
        }
        let mut committed = b.runner.cast(b.spell).target_objects(&[b.b, b.c]).commit();
        let outcome = committed.cast(b.response).target_object(b.b).resolve();
        outcome.assert_zone(&[b.b, b.c], Zone::Battlefield);
        assert_created(outcome.state(), outcome.events(), b.spell, 0, (0, 0));
        assert_eq!(outcome.state().objects[&b.c].power, Some(12));
        if needs_shield {
            assert!(outcome.state().objects[&b.b].tapped);
        } else {
            assert!(outcome.state().objects[&b.b]
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Artifact));
        }
    }
}
