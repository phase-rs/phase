//! Ordinary and synthetic duration consumers of separately printed token definitions.
//! Full Broken Visage's exact-token delayed cleanup belongs to the next consumer.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ShieldKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CONSTRUCTION: &str = "Destroy target nonartifact attacking creature. It can't be regenerated. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness. Target creature gets +1/+1 until end of turn.";
const REGENERABLE: &str = "Destroy target nonartifact attacking creature. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness. Target creature gets +1/+1 until end of turn.";
const DOUBLING: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
// Explicitly synthetic isolation: exactly Broken Visage's first four sentences.
pub(super) const ORDINARY_CONSTRUCTION: &str = "Destroy target nonartifact attacking creature. It can't be regenerated. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness.";

pub(super) struct Board {
    pub(super) runner: GameRunner,
    pub(super) spell: ObjectId,
    pub(super) b: ObjectId,
    c: ObjectId,
    a: ObjectId,
    response: ObjectId,
    pub(super) regen: ObjectId,
    modifier: ObjectId,
}

pub(super) fn board(
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

pub(super) fn assert_created(
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
        assert_eq!(token.color, vec![ManaColor::Black]);
        assert_eq!(
            token.card_types.core_types,
            vec![engine::types::card_type::CoreType::Creature]
        );
        assert_eq!(token.card_types.subtypes, vec!["Spirit"]);
        assert_eq!((token.power, token.toughness), (Some(pt.0), Some(pt.1)));
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
    // CR 117.3b + CR 117.3d: resolution returns priority to active P1; pass to P0.
    assert!(matches!(
        b.runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    b.runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        b.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
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
        // CR 117.3b + CR 117.3d: P0 casts the next spell after P1 passes priority.
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        b.runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
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
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected attack trigger target prompt: {:?}",
            runner.state().waiting_for
        );
    };
    let chosen = TargetRef::Object(target);
    assert!(target_slots[selection.current_slot]
        .legal_targets
        .contains(&chosen));
    let trigger_entry = runner
        .state()
        .pending_trigger_entry
        .expect("trigger construction");
    // CR 603.3d: choose the real trigger's legal target before receiving priority.
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(chosen.clone()),
            })
            .unwrap()
            .events,
    );
    assert!(runner.state().pending_trigger_entry.is_none());
    assert!(runner.state().pending_trigger.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == trigger_entry)
        .expect("completed trigger on stack");
    assert_eq!(entry.source_id, source);
    assert!(entry
        .ability()
        .expect("trigger body")
        .targets
        .contains(&chosen));
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(runner.state().stack.is_empty());
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
            // CR 117.3b + CR 117.3d: return the next construction's casting seat to P0.
            assert!(matches!(
                b.runner.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            ));
            b.runner.act(GameAction::PassPriority).unwrap();
            assert!(matches!(
                b.runner.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ));
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

#[test]
fn duration_graveyard_card_definition_uses_last_graveyard_pt() {
    // Synthetic duration consumer: full Ritual text plus an independent tail.
    let oracle = "Exile target creature card from your graveyard. Create a black Zombie creature token. Its power is equal to that card's power and its toughness is equal to that card's toughness. Target creature gets +1/+1 until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
    let card = scenario
        .add_creature(P0, "Graveyard characteristic source", 2, 5)
        .id();
    let independent = scenario
        .add_creature(P0, "Independent duration target", 11, 13)
        .id();
    let modifier = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic departure modifier",
            true,
            "Target creature gets +3/+1 until end of turn.",
        )
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic graveyard preparation",
            true,
            "Destroy target creature.",
        )
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic duration Ritual", true, oracle)
        .id();
    let mut runner = scenario.build();
    runner.cast(modifier).target_object(card).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        (
            runner.state().objects[&card].power,
            runner.state().objects[&card].toughness
        ),
        (Some(5), Some(6))
    );
    runner
        .cast(destroy)
        .target_object(card)
        .resolve()
        .assert_zone(&[card], Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // CR 400.7 + CR 608.2h: the graveyard card has its own characteristics,
    // distinct from the real 5/6 battlefield departure snapshot.
    let stale = &runner.state().lki_cache[&card];
    assert_eq!((stale.power, stale.toughness), (Some(5), Some(6)));
    assert_eq!(
        (
            runner.state().objects[&card].power,
            runner.state().objects[&card].toughness
        ),
        (Some(2), Some(5))
    );
    let graveyard_incarnation = runner.state().objects[&card].incarnation;
    let outcome = runner
        .cast(spell)
        .target_objects(&[card, independent])
        .resolve();
    outcome.assert_zone(&[card], Zone::Exile);
    assert_ne!(
        outcome.state().objects[&card].incarnation,
        graveyard_incarnation
    );
    let tokens: Vec<_> = outcome
        .state()
        .objects
        .values()
        .filter(|object| object.is_token)
        .collect();
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    // CR 111.3 + CR 608.2h: creation uses the last graveyard 2/5, not old 5/6 LKI.
    assert_eq!((token.base_power, token.base_toughness), (Some(2), Some(5)));
    assert_eq!((token.owner, token.controller), (P0, P0));
    assert_eq!(token.name, "Zombie");
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::TokenCreated { object_id, source_id, .. } if *object_id == token.id && *source_id == spell)).count(), 1);
    assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: None, to: Zone::Battlefield, .. } if *object_id == token.id)).count(), 1);
    assert_eq!(
        (
            outcome.state().objects[&independent].power,
            outcome.state().objects[&independent].toughness
        ),
        (Some(12), Some(14))
    );
}

fn return_priority_to_caster(runner: &mut GameRunner) {
    // CR 117.3b + CR 117.3d: active P1 receives priority after preparation resolves.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

fn assert_committed_occurrences(
    state: &GameState,
    spell: ObjectId,
    expected: &[ObjectId],
) -> Vec<ObjectIncarnationRef> {
    // CR 400.7 + CR 601.2c: capture both coordinates at announcement, before
    // responses or resolution can move any selected object.
    let announced: Vec<_> = expected
        .iter()
        .map(|id| ObjectIncarnationRef::from_object(&state.objects[id]))
        .collect();
    assert_committed_pins(state, spell, &announced);
    announced
}

fn assert_committed_pins(state: &GameState, spell: ObjectId, announced: &[ObjectIncarnationRef]) {
    let entry = state
        .stack
        .iter()
        .find(|e| e.source_id == spell)
        .expect("committed construction");
    let ability = entry.ability().unwrap();
    // The existing declaration authority excludes inherited mirrors from the
    // printed count, while retaining the separate A-then-B announcement order.
    assert_eq!(
        engine::game::ability_utils::declared_targets_in_chain(ability),
        announced
            .iter()
            .map(|pin| TargetRef::Object(pin.object_id))
            .collect::<Vec<_>>()
    );
    for node in std::iter::successors(Some(ability), |d| d.sub_ability.as_deref()) {
        for (index, target) in node.targets.iter().enumerate() {
            if let TargetRef::Object(id) = target {
                let expected = announced
                    .iter()
                    .find(|pin| pin.object_id == *id)
                    .expect("no unprinted object occurrence");
                assert_eq!(
                    node.target_pin_at(index),
                    Some(*expected),
                    "exact declared or inherited occurrence {index}: {node:#?}"
                );
            }
        }
        if matches!(*node.effect, engine::types::ability::Effect::Token { .. }) {
            assert_eq!(
                node.target_reads,
                engine::types::ability::TargetReadOrigin::ParentAnnouncement
            );
        }
    }
    assert_eq!(state.objects[&spell].zone, Zone::Stack);
}

#[test]
fn target_defined_spirit_uses_departure_pt() {
    let mut b = board(
        ORDINARY_CONSTRUCTION,
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
    return_priority_to_caster(&mut b.runner);
    assert_eq!(
        (
            b.runner.state().objects[&b.b].power,
            b.runner.state().objects[&b.b].toughness
        ),
        (Some(7), Some(8))
    );
    let mana_before = b.runner.state().players[0].mana_pool.total();
    let committed = b.runner.cast(b.spell).target_object(b.b).commit();
    let _announced = assert_committed_occurrences(committed.state(), b.spell, &[b.b]);
    assert_eq!(
        committed.state().players[0].mana_pool.total(),
        mana_before - 1
    );
    let outcome = committed.resolve();
    outcome.assert_zone(&[b.b], Zone::Graveyard);
    // CR 608.2h + CR 613.4c: two counters and +3/+1 yield departure 7/8.
    let lki = &outcome.state().lki_cache[&b.b];
    assert_eq!((lki.power, lki.toughness), (Some(7), Some(8)));
    let tokens = assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
    assert_eq!(outcome.state().objects[&b.c].power, Some(11));
    b.runner.advance_to_end_step();
    b.runner.advance_to_upkeep();
    // CR 111.3: copied values are frozen even after the modifier expires.
    assert_eq!(
        (
            b.runner.state().objects[&tokens[0]].power,
            b.runner.state().objects[&tokens[0]].toughness
        ),
        (Some(7), Some(8))
    );
}

#[test]
fn target_defined_spirit_reads_live_when_destroy_skips() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
    let victim = scenario
        .add_creature(P1, "Live returned B", 2, 5)
        .indestructible()
        .flash()
        .haste()
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic cycle",
            true,
            "Return target creature to its owner's hand.",
        )
        .id();
    let modifier = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic live modifier",
            true,
            "Target creature gets +3/+1 until end of turn.",
        )
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic ordinary live Spirit",
            true,
            ORDINARY_CONSTRUCTION,
        )
        .id();
    let mut runner = scenario.build();
    let old_incarnation = runner.state().objects[&victim].incarnation;
    runner
        .cast(bounce)
        .target_object(victim)
        .resolve()
        .assert_zone(&[victim], Zone::Hand);
    assert_eq!(
        (
            runner.state().lki_cache[&victim].power,
            runner.state().lki_cache[&victim].toughness
        ),
        (Some(2), Some(5))
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner
        .cast(victim)
        .resolve()
        .assert_zone(&[victim], Zone::Battlefield);
    assert_ne!(runner.state().objects[&victim].incarnation, old_incarnation);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .unwrap()
        .counters
        .insert(engine::types::counter::CounterType::Plus1Plus1, 2);
    runner.cast(modifier).target_object(victim).resolve();
    assert_eq!(
        (
            runner.state().objects[&victim].power,
            runner.state().objects[&victim].toughness
        ),
        (Some(7), Some(8))
    );
    // Real prior battlefield LKI remains distinct from this live incarnation.
    assert_eq!(
        (
            runner.state().lki_cache[&victim].power,
            runner.state().lki_cache[&victim].toughness
        ),
        (Some(2), Some(5))
    );
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(victim, AttackTarget::Player(P0))])
        .unwrap();
    runner.advance_to_priority_window();
    if runner.state().priority_player == P1 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let outcome = runner.cast(spell).target_object(victim).resolve();
    // CR 702.12b + CR 609.3: indestructible stops destruction, not creation.
    outcome.assert_zone(&[victim], Zone::Battlefield);
    assert_created(outcome.state(), outcome.events(), spell, 1, (7, 8));
}

#[test]
fn ordinary_construction_zero_power_is_real_information() {
    let mut b = board(
        ORDINARY_CONSTRUCTION,
        (0, 5),
        false,
        false,
        "Return target creature to its owner's hand.",
    );
    let outcome = b.runner.cast(b.spell).target_object(b.b).resolve();
    outcome.assert_zone(&[b.b], Zone::Graveyard);
    assert_created(outcome.state(), outcome.events(), b.spell, 1, (0, 5));
}

#[test]
fn token_definition_reads_nearest_announced_occurrence() {
    let text = format!("Tap target creature. {ORDINARY_CONSTRUCTION}");
    let mut b = board(
        &text,
        (7, 8),
        false,
        false,
        "Create a 11/13 white Soldier creature token. Reveal the top card of your library.",
    );
    let tracked_card = b.runner.state().players[0].library[0];
    let prior = b.runner.cast(b.response).resolve();
    let prior_id = prior
        .events()
        .iter()
        .find_map(|e| match e {
            GameEvent::TokenCreated {
                object_id,
                source_id,
                ..
            } if *source_id == b.response => Some(*object_id),
            _ => None,
        })
        .expect("distinct prior creation");
    assert!(prior.state().last_created_token_ids.contains(&prior_id));
    assert_eq!(prior.state().last_revealed_ids, vec![tracked_card]);
    assert!(prior
        .state()
        .tracked_object_sets
        .values()
        .any(|members| members == &vec![tracked_card]));
    assert!(prior.events().iter().any(|event| matches!(event, GameEvent::CardsRevealed { card_ids, .. } if card_ids == &vec![tracked_card])));
    assert_eq!(
        (
            prior.state().objects[&prior_id].power,
            prior.state().objects[&prior_id].toughness
        ),
        (Some(11), Some(13))
    );
    return_priority_to_caster(&mut b.runner);
    // An unrelated shield spell stays on the stack beneath the construction.
    let mut other = b.runner.cast(b.regen).target_object(b.a).commit();
    let committed = other.cast(b.spell).target_objects(&[b.a, b.b]).commit();
    assert_eq!(committed.state().stack.len(), 2);
    assert!(committed
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == b.regen));
    let _announced = assert_committed_occurrences(committed.state(), b.spell, &[b.a, b.b]);
    let outcome = committed.resolve();
    assert!(outcome.state().objects[&b.a].tapped);
    outcome.assert_zone(&[b.b], Zone::Graveyard);
    assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
    assert_eq!(
        (
            outcome.state().objects[&prior_id].base_power,
            outcome.state().objects[&prior_id].base_toughness
        ),
        (Some(11), Some(13))
    );
    assert_eq!(outcome.state().objects[&b.c].power, Some(11));
}

fn ordinary_interrupted(two_targets: bool) {
    let text = if two_targets {
        format!("Tap target creature. {ORDINARY_CONSTRUCTION}")
    } else {
        ORDINARY_CONSTRUCTION.into()
    };
    let mut b = board(
        &text,
        (7, 8),
        false,
        false,
        "Return target creature to its owner's hand.",
    );
    let targets = if two_targets {
        vec![b.a, b.b]
    } else {
        vec![b.b]
    };
    let mut committed = b.runner.cast(b.spell).target_objects(&targets).commit();
    let _announced = assert_committed_occurrences(committed.state(), b.spell, &targets);
    let outcome = committed.cast(b.response).target_object(b.b).resolve();
    outcome.assert_zone(&[b.b], Zone::Hand);
    outcome.assert_zone(&[b.spell], Zone::Graveyard);
    assert!(outcome.state().stack.is_empty());
    assert_created(outcome.state(), outcome.events(), b.spell, 0, (0, 0));
    assert!(outcome.state().objects.values().all(|o| !o.is_token));
    if two_targets {
        // CR 608.2b: legal A executes; missing B cannot supply dependent P/T.
        assert!(outcome.state().objects[&b.a].tapped);
    }
    // Positive same-seam sister prevents a failed parse/fizzle from proving no creation.
    let mut legal = board(
        &text,
        (7, 8),
        false,
        false,
        "Return target creature to its owner's hand.",
    );
    let targets = if two_targets {
        vec![legal.a, legal.b]
    } else {
        vec![legal.b]
    };
    let outcome = legal
        .runner
        .cast(legal.spell)
        .target_objects(&targets)
        .resolve();
    assert_created(outcome.state(), outcome.events(), legal.spell, 1, (7, 8));
    if two_targets {
        assert!(outcome.state().objects[&legal.a].tapped);
    }
}

#[test]
fn construction_all_targets_illegal_creates_nothing() {
    ordinary_interrupted(false);
}

#[test]
fn ordinary_prior_target_illegal_other_target_legal() {
    ordinary_interrupted(true);
}

#[test]
fn ordinary_target_qualifiers_are_checked_before_initial_resolution() {
    for (response, shield) in [
        (
            "Target creature becomes an artifact in addition to its other types until end of turn.",
            false,
        ),
        ("Destroy target creature.", true),
    ] {
        let mut b = board(ORDINARY_CONSTRUCTION, (7, 8), false, false, response);
        if shield {
            b.runner.cast(b.regen).target_object(b.b).resolve();
            return_priority_to_caster(&mut b.runner);
            assert!(b.runner.state().objects[&b.b]
                .replacement_definitions
                .iter()
                .any(|r| r.shield_kind == ShieldKind::Regeneration));
        }
        let mut committed = b.runner.cast(b.spell).target_object(b.b).commit();
        let _announced = assert_committed_occurrences(committed.state(), b.spell, &[b.b]);
        let outcome = committed.cast(b.response).target_object(b.b).resolve();
        outcome.assert_zone(&[b.b], Zone::Battlefield);
        outcome.assert_zone(&[b.spell], Zone::Graveyard);
        assert_created(outcome.state(), outcome.events(), b.spell, 0, (0, 0));
        assert!(outcome.state().objects.values().all(|o| !o.is_token));
        if shield {
            assert!(outcome.state().objects[&b.b].tapped);
            assert!(outcome
                .state()
                .combat
                .as_ref()
                .is_none_or(|c| c.attackers.iter().all(|a| a.object_id != b.b)));
        } else {
            assert!(outcome.state().objects[&b.b]
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Artifact));
        }
        let mut legal = board(ORDINARY_CONSTRUCTION, (7, 8), false, false, response);
        let outcome = legal
            .runner
            .cast(legal.spell)
            .target_object(legal.b)
            .resolve();
        assert_created(outcome.state(), outcome.events(), legal.spell, 1, (7, 8));
    }
}

#[test]
fn token_definition_existing_wire_values_roundtrip() {
    for legacy in [false, true] {
        let mut b = board(
            ORDINARY_CONSTRUCTION,
            (7, 8),
            false,
            false,
            "Return target creature to its owner's hand.",
        );
        let committed = b.runner.cast(b.spell).target_object(b.b).commit();
        let announced = assert_committed_occurrences(committed.state(), b.spell, &[b.b]);
        let mut json = serde_json::to_value(committed.state()).unwrap();
        if legacy {
            remove_pins(&mut json);
        }
        let state: GameState = serde_json::from_value(json).unwrap();
        assert_eq!(state.objects[&b.spell].zone, Zone::Stack);
        if !legacy {
            assert_committed_pins(&state, b.spell, &announced);
        } else {
            let ability = state
                .stack
                .iter()
                .find(|entry| entry.source_id == b.spell)
                .unwrap()
                .ability()
                .unwrap();
            assert_eq!(
                engine::game::ability_utils::declared_targets_in_chain(ability),
                vec![TargetRef::Object(b.b)]
            );
            assert!(
                std::iter::successors(Some(ability), |node| node.sub_ability.as_deref())
                    .flat_map(|node| node.aligned_target_pins())
                    .all(|pin| pin.is_none())
            );
        }
        let mut resumed = GameRunner::from_state(state);
        let mut events = Vec::new();
        for _ in 0..16 {
            if resumed.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                resumed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            events.extend(resumed.act(GameAction::PassPriority).unwrap().events);
        }
        assert!(resumed.state().stack.is_empty());
        assert_eq!(resumed.state().objects[&b.b].zone, Zone::Graveyard);
        assert_created(resumed.state(), &events, b.spell, 1, (7, 8));
    }
}

#[test]
fn ordinary_ritual_definition_uses_last_graveyard_pt() {
    // Complete pinned Ritual of the Returned through the public cast pipeline.
    let oracle = "Exile target creature card from your graveyard. Create a black Zombie creature token. Its power is equal to that card's power and its toughness is equal to that card's toughness.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
    let card = scenario
        .add_creature(P0, "Graveyard characteristic source", 2, 5)
        .id();
    let independent = scenario
        .add_creature(P0, "Independent duration target", 11, 13)
        .id();
    let modifier = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic departure modifier",
            true,
            "Target creature gets +3/+1 until end of turn.",
        )
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic graveyard preparation",
            true,
            "Destroy target creature.",
        )
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ritual of the Returned", true, oracle)
        .id();
    let mut runner = scenario.build();
    runner.cast(modifier).target_object(card).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        (
            runner.state().objects[&card].power,
            runner.state().objects[&card].toughness
        ),
        (Some(5), Some(6))
    );
    runner
        .cast(destroy)
        .target_object(card)
        .resolve()
        .assert_zone(&[card], Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // CR 400.7 + CR 608.2h: the graveyard card has its own characteristics,
    // distinct from the real 5/6 battlefield departure snapshot.
    let stale = &runner.state().lki_cache[&card];
    assert_eq!((stale.power, stale.toughness), (Some(5), Some(6)));
    assert_eq!(
        (
            runner.state().objects[&card].power,
            runner.state().objects[&card].toughness
        ),
        (Some(2), Some(5))
    );
    let graveyard_incarnation = runner.state().objects[&card].incarnation;
    let outcome = runner.cast(spell).target_object(card).resolve();
    outcome.assert_zone(&[card], Zone::Exile);
    assert_ne!(
        outcome.state().objects[&card].incarnation,
        graveyard_incarnation
    );
    let tokens: Vec<_> = outcome
        .state()
        .objects
        .values()
        .filter(|object| object.is_token)
        .collect();
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    // CR 111.3 + CR 608.2h: creation uses the last graveyard 2/5, not old 5/6 LKI.
    assert_eq!((token.base_power, token.base_toughness), (Some(2), Some(5)));
    assert_eq!((token.owner, token.controller), (P0, P0));
    assert_eq!(token.name, "Zombie");
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::TokenCreated { object_id, source_id, .. } if *object_id == token.id && *source_id == spell)).count(), 1);
    assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: None, to: Zone::Battlefield, .. } if *object_id == token.id)).count(), 1);
    assert_eq!(
        (
            outcome.state().objects[&independent].power,
            outcome.state().objects[&independent].toughness
        ),
        (Some(11), Some(13))
    );
}
