//! Ordinary and synthetic duration consumers of separately printed token definitions.
//! Completed creation results feed ordinary delayed instructions.

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
const FULL_BROKEN: &str = "Destroy target nonartifact attacking creature. It can't be regenerated. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness. Sacrifice the token at the beginning of the next end step.";
const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";
const JINNIE: &str = "If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.";
const DIVINE: &str = "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.";
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";
// Explicitly synthetic isolation: exactly Broken Visage's first four sentences.
pub(super) const ORDINARY_CONSTRUCTION: &str = "Destroy target nonartifact attacking creature. It can't be regenerated. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness.";

pub(super) struct Board {
    pub(super) runner: GameRunner,
    pub(super) spell: ObjectId,
    pub(super) b: ObjectId,
    c: ObjectId,
    a: ObjectId,
    response: ObjectId,
    opponent_response: ObjectId,
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
    board_with_replacements(oracle, pt, indestructible, multiplier, response, &[])
}

fn board_with_replacements(
    oracle: &str,
    pt: (i32, i32),
    indestructible: bool,
    multiplier: bool,
    response: &str,
    replacements: &[(&str, &str)],
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
    for (name, text) in replacements {
        if *name == "Chatterfang, Squirrel General" {
            scenario.add_creature_from_oracle(P0, name, 3, 3, text);
        } else if *name == "Jinnie Fay, Jetmir's Second" {
            scenario.add_creature_from_oracle(P0, name, 3, 3, text);
        } else {
            scenario.add_enchantment_from_oracle(P0, name, text);
        }
    }
    let full = oracle.ends_with(FULL_BROKEN);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            if full {
                "Broken Visage"
            } else {
                "Synthetic duration Spirit"
            },
            true,
            oracle,
        )
        .with_mana_cost(if full {
            ManaCost::Cost {
                generic: 4,
                shards: vec![engine::types::mana::ManaCostShard::Black],
            }
        } else {
            ManaCost::generic(1)
        })
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic response", true, response)
        .id();
    let opponent_response = scenario.add_spell_to_hand_from_oracle(P1,"Ray of Command",true,"Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.").with_mana_cost(ManaCost::Cost {generic:3,shards:vec![engine::types::mana::ManaCostShard::Blue]}).id();
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
    if full {
        for color in [ManaType::Black, ManaType::Blue, ManaType::White] {
            for _ in 0..4 {
                let _ = runner
                    .state_mut()
                    .add_mana_to_pool(P0, ManaUnit::new(color, ObjectId(0), false, vec![]));
            }
        }
    }
    Board {
        runner,
        spell,
        b,
        c,
        a,
        response,
        opponent_response,
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

pub(super) fn creation_priority_to_caster(runner: &mut GameRunner) {
    for _ in 0..runner.state().players.len() {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ) {
            return;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

pub(super) fn pass_creation_window(runner: &mut GameRunner) -> Vec<GameEvent> {
    match runner.state().waiting_for.clone() {
        WaitingFor::DeclareBlockers { .. } => {
            runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .unwrap()
                .events
        }
        WaitingFor::DeclareAttackers {
            valid_attack_targets,
            valid_attack_targets_by_attacker,
            attacker_constraints,
            ..
        } => {
            let attacks = attacker_constraints
                .iter()
                .filter_map(|(id, requirement)| match requirement {
                    engine::game::combat::CombatRequirement::MustAttack { defenders, .. } => {
                        defenders
                            .first()
                            .copied()
                            .or_else(|| {
                                valid_attack_targets_by_attacker
                                    .as_ref()
                                    .and_then(|map| map.get(id))
                                    .and_then(|targets| targets.first())
                                    .copied()
                            })
                            .or_else(|| valid_attack_targets.first().copied())
                            .map(|target| (*id, target))
                    }
                    _ => None,
                })
                .collect();
            runner
                .act(GameAction::DeclareAttackers {
                    attacks,
                    bands: vec![],
                })
                .unwrap()
                .events
        }
        WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority).unwrap().events,
        other => panic!("unexpected public creation window: {other:?}"),
    }
}

fn cleanup_stack_entry(runner: &GameRunner, source: ObjectId) -> Option<ObjectId> {
    runner
        .state()
        .stack
        .iter()
        .find(|entry| {
            entry.source_id == source
                && entry.ability().is_some_and(|ability| {
                    matches!(
                        ability.effect,
                        engine::types::ability::Effect::Sacrifice { .. }
                    )
                })
        })
        .map(|entry| entry.id)
}

pub(super) fn reach_cleanup_stack(runner: &mut GameRunner, source: ObjectId) -> ObjectId {
    for _ in 0..160 {
        if let Some(id) = cleanup_stack_entry(runner, source) {
            return id;
        }
        pass_creation_window(runner);
    }
    panic!(
        "normal delayed cleanup never reached the stack: {:?}",
        runner.state().waiting_for
    )
}

fn full_broken_result(b: &mut Board, expected: usize) -> Vec<ObjectIncarnationRef> {
    let before = b
        .runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .unwrap()
        .mana_pool
        .total() as u32;
    let outcome = b.runner.cast(b.spell).target_object(b.b).resolve();
    outcome.assert_zone(&[b.spell, b.b], Zone::Graveyard);
    assert_eq!(
        before - outcome.mana_pool_total(P0),
        5,
        "actual printed 4B payment"
    );
    let entries: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(entries.len(), expected);
    assert!(entries.iter().all(|id| outcome.events().iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: None, to: Zone::Battlefield, .. } if object_id == id))));
    let delayed = outcome
        .state()
        .delayed_triggers
        .iter()
        .find(|delayed| {
            delayed.source_id == b.spell
                && matches!(
                    delayed.ability.effect,
                    engine::types::ability::Effect::Sacrifice { .. }
                )
        })
        .expect("normal independent sacrifice installed");
    assert_eq!(delayed.controller, P0);
    assert!(delayed.one_shot);
    assert_eq!(delayed.ability.target_incarnations.len(), expected);
    assert_eq!(
        delayed
            .ability
            .targets
            .iter()
            .filter_map(|target| if let TargetRef::Object(id) = target {
                Some(*id)
            } else {
                None
            })
            .collect::<std::collections::HashSet<_>>(),
        entries.into_iter().collect()
    );
    assert!(delayed.ability.context.token_creation_parent.is_none());
    assert!(outcome.state().resolution_stack.is_empty());
    delayed.ability.target_incarnations.clone()
}

fn assert_original_births_left(runner: &GameRunner, births: &[ObjectIncarnationRef]) {
    for member in births {
        assert!(!runner
            .state()
            .objects
            .get(&member.object_id)
            .is_some_and(|object| object.zone == Zone::Battlefield
                && ObjectIncarnationRef::from_object(object) == *member));
    }
}

#[test]
fn broken_visage_creates_frozen_spirit_and_installs_cleanup() {
    let mut b = board(
        FULL_BROKEN,
        (2, 5),
        false,
        false,
        "Create two 1/1 white Soldier creature tokens.",
    );
    b.runner
        .state_mut()
        .objects
        .get_mut(&b.b)
        .unwrap()
        .counters
        .insert(engine::types::counter::CounterType::Plus1Plus1, 2);
    b.runner.cast(b.modifier).target_object(b.b).resolve();
    creation_priority_to_caster(&mut b.runner);
    let births = full_broken_result(&mut b, 1);
    let spirit = &b.runner.state().objects[&births[0].object_id];
    assert_eq!(
        (
            spirit.base_power,
            spirit.base_toughness,
            spirit.power,
            spirit.toughness
        ),
        (Some(7), Some(8), Some(7), Some(8))
    );
    assert_eq!(spirit.color, vec![ManaColor::Black]);
    assert_eq!(spirit.card_types.subtypes, vec!["Spirit"]);
    assert_eq!((spirit.owner, spirit.controller), (P0, P0));
    reach_cleanup_stack(&mut b.runner, b.spell);
    assert_eq!(b.runner.state().phase, Phase::End);
    assert_eq!(
        ObjectIncarnationRef::from_object(&b.runner.state().objects[&births[0].object_id]),
        births[0]
    );
    b.runner.advance_until_stack_empty();
    assert_original_births_left(&b.runner, &births);
    assert_eq!(b.runner.state().objects[&b.c].zone, Zone::Battlefield);
}

#[test]
fn broken_visage_multiplied_births_all_sacrifice_and_later_response_survives() {
    let mut b = board(
        FULL_BROKEN,
        (7, 8),
        false,
        true,
        "Create two 1/1 white Soldier creature tokens.",
    );
    let births = full_broken_result(&mut b, 2);
    for member in &births {
        assert_eq!(
            (
                b.runner.state().objects[&member.object_id].base_power,
                b.runner.state().objects[&member.object_id].base_toughness
            ),
            (Some(7), Some(8))
        );
    }
    reach_cleanup_stack(&mut b.runner, b.spell);
    creation_priority_to_caster(&mut b.runner);
    let response = b.runner.cast(b.response).resolve();
    let later: Vec<_> = response
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        later.len(),
        4,
        "the independent real token instruction is doubled too"
    );
    assert_original_births_left(&b.runner, &births);
    assert!(later
        .iter()
        .all(|id| b.runner.state().objects[id].zone == Zone::Battlefield));
}

#[test]
fn broken_visage_chatterfang_cleans_complete_replaced_result() {
    for doubled in [false, true] {
        let mut b = board_with_replacements(
            FULL_BROKEN,
            (7, 8),
            false,
            doubled,
            "Create two 1/1 white Soldier creature tokens.",
            &[("Chatterfang, Squirrel General", CHATTERFANG)],
        );
        let births = full_broken_result(&mut b, if doubled { 4 } else { 2 });
        let bodies: Vec<_> = births
            .iter()
            .map(|member| &b.runner.state().objects[&member.object_id])
            .collect();
        assert_eq!(
            bodies
                .iter()
                .filter(|object| object.name == "Spirit"
                    && object.base_power == Some(7)
                    && object.base_toughness == Some(8))
                .count(),
            if doubled { 2 } else { 1 }
        );
        assert_eq!(
            bodies
                .iter()
                .filter(
                    |object| object.card_types.subtypes.contains(&"Squirrel".into())
                        && object.base_power == Some(1)
                        && object.base_toughness == Some(1)
                        && object.color == vec![ManaColor::Green]
                )
                .count(),
            if doubled { 2 } else { 1 }
        );
        reach_cleanup_stack(&mut b.runner, b.spell);
        b.runner.advance_until_stack_empty();
        assert_original_births_left(&b.runner, &births);
    }
}

#[test]
fn broken_visage_divine_creates_angel_and_cleans_only_its_birth() {
    let mut b = board_with_replacements(
        FULL_BROKEN,
        (7, 8),
        false,
        false,
        STIFLE,
        &[("Divine Visitation", DIVINE)],
    );
    let births = full_broken_result(&mut b, 1);
    let angel = &b.runner.state().objects[&births[0].object_id];
    assert_eq!((angel.base_power, angel.base_toughness), (Some(4), Some(4)));
    assert_eq!(angel.card_types.subtypes, vec!["Angel"]);
    assert_eq!(angel.color, vec![ManaColor::White]);
    assert!(angel
        .keywords
        .contains(&engine::types::keywords::Keyword::Flying));
    assert!(angel
        .keywords
        .contains(&engine::types::keywords::Keyword::Vigilance));
    reach_cleanup_stack(&mut b.runner, b.spell);
    b.runner.advance_until_stack_empty();
    assert_original_births_left(&b.runner, &births);
}

#[test]
fn broken_visage_cleanup_is_a_counterable_spent_trigger() {
    let mut b = board(FULL_BROKEN, (7, 8), false, false, STIFLE);
    let births = full_broken_result(&mut b, 1);
    let trigger = reach_cleanup_stack(&mut b.runner, b.spell);
    creation_priority_to_caster(&mut b.runner);
    let answer = b.runner.cast(b.response).target_object(trigger).resolve();
    assert!(answer.events().iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == trigger)));
    assert_eq!(
        b.runner.state().objects[&births[0].object_id].zone,
        Zone::Battlefield
    );
    assert!(b
        .runner
        .state()
        .delayed_triggers
        .iter()
        .all(|delayed| delayed.source_id != b.spell));
    let fired_turn = b.runner.state().turn_number;
    for _ in 0..300 {
        if b.runner.state().turn_number > fired_turn && b.runner.state().phase == Phase::End {
            break;
        }
        pass_creation_window(&mut b.runner);
    }
    assert!(b.runner.state().turn_number > fired_turn);
    assert_eq!(b.runner.state().phase, Phase::End);
    assert!(cleanup_stack_entry(&b.runner, b.spell).is_none());
    assert_eq!(
        ObjectIncarnationRef::from_object(&b.runner.state().objects[&births[0].object_id]),
        births[0]
    );
}

#[test]
fn broken_visage_partial_and_all_departed_members_do_not_widen_cleanup() {
    for all in [false, true] {
        let response = if all {
            "Exile all creatures with power 7 or greater."
        } else {
            "Exile target creature you control, then return that card to the battlefield under your control."
        };
        let mut b = board(FULL_BROKEN, (7, 8), false, true, response);
        let births = full_broken_result(&mut b, 2);
        // Positive complete saved set, followed by real public removal BEFORE
        // the independent delayed trigger is collected.
        creation_priority_to_caster(&mut b.runner);
        if all {
            b.runner.cast(b.response).resolve();
        } else {
            b.runner
                .cast(b.response)
                .target_object(births[0].object_id)
                .resolve();
        }
        assert_ne!(
            b.runner.state().objects[&births[0].object_id].zone,
            Zone::Battlefield
        );
        if !all {
            assert_eq!(
                b.runner.state().objects[&births[1].object_id].zone,
                Zone::Battlefield
            );
        }
        let delayed = b
            .runner
            .state()
            .delayed_triggers
            .iter()
            .find(|trigger| trigger.source_id == b.spell)
            .unwrap();
        assert_eq!(delayed.ability.target_incarnations, births);
        reach_cleanup_stack(&mut b.runner, b.spell);
        b.runner.advance_until_stack_empty();
        assert_original_births_left(&b.runner, &births);
        assert!(!b
            .runner
            .state()
            .delayed_triggers
            .iter()
            .any(|trigger| trigger.source_id == b.spell));
        if !all {
            assert_eq!(b.runner.state().objects[&b.c].zone, Zone::Battlefield);
        }
    }
}

#[test]
fn broken_visage_current_persistence_keeps_captured_birth_refs() {
    let mut b = board(FULL_BROKEN, (7, 8), false, true, STIFLE);
    let births = full_broken_result(&mut b, 2);
    for mode in 0..4 {
        let state = b.runner.state().clone();
        let loaded = match mode {
            0 => {
                serde_json::from_value::<GameState>(serde_json::to_value(&state).unwrap()).unwrap()
            }
            1 => serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                serde_json::to_value(
                    engine::types::resolution::ResolutionStateWire::from_game_state(state),
                )
                .unwrap(),
            )
            .unwrap()
            .into_game_state(),
            2 => serde_json::from_value::<engine::types::game_state::TrustedGameStateEnvelope>(
                serde_json::to_value(
                    engine::types::game_state::TrustedGameStateEnvelope::capture(state),
                )
                .unwrap(),
            )
            .unwrap()
            .into_game_state(),
            _ => state,
        };
        let delayed = loaded
            .delayed_triggers
            .iter()
            .find(|trigger| trigger.source_id == b.spell)
            .unwrap();
        assert_eq!(delayed.ability.target_incarnations, births);
        let mut resumed = GameRunner::from_state(loaded);
        reach_cleanup_stack(&mut resumed, b.spell);
        resumed.advance_until_stack_empty();
        assert_original_births_left(&resumed, &births);
        assert_eq!(resumed.state().objects[&b.c].zone, Zone::Battlefield);
    }
}

#[test]
fn broken_visage_initial_illegal_and_completed_empty_have_distinct_policies() {
    for partial in [false, true] {
        let text = if partial {
            format!("Tap target creature. {FULL_BROKEN}")
        } else {
            FULL_BROKEN.into()
        };
        let mut b = board(
            &text,
            (7, 8),
            false,
            false,
            "Return target creature to its owner's hand.",
        );
        let targets = if partial { vec![b.a, b.b] } else { vec![b.b] };
        let mut committed = b.runner.cast(b.spell).target_objects(&targets).commit();
        let outcome = committed.cast(b.response).target_object(b.b).resolve();
        assert_eq!(outcome.state().objects[&b.b].zone, Zone::Hand);
        assert!(!outcome.events().iter().any(|event| matches!(event, GameEvent::TokenCreated { source_id, .. } if *source_id == b.spell)));
        let cleanup = outcome
            .state()
            .delayed_triggers
            .iter()
            .find(|trigger| trigger.source_id == b.spell);
        assert_eq!(cleanup.is_some(), partial);
        if partial {
            assert!(outcome.state().objects[&b.a].tapped);
            let cleanup = cleanup.unwrap();
            assert!(matches!(
                cleanup.ability.effect,
                engine::types::ability::Effect::Sacrifice {
                    target: engine::types::ability::TargetFilter::None,
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    ..
                }
            ));
            reach_cleanup_stack(&mut b.runner, b.spell);
            b.runner.advance_until_stack_empty();
            assert_eq!(b.runner.state().objects[&b.c].zone, Zone::Battlefield);
        }
    }
    let mut positive = board(FULL_BROKEN, (0, 5), false, false, STIFLE);
    let births = full_broken_result(&mut positive, 1);
    assert_eq!(
        (
            positive.runner.state().objects[&births[0].object_id].base_power,
            positive.runner.state().objects[&births[0].object_id].base_toughness
        ),
        (Some(0), Some(5))
    );
}

const MISHRA: &str = "At the beginning of combat on your turn, create a token that's a copy of target noncreature artifact you control, except its name is Mishra's Warform and it's a 4/4 Construct artifact creature in addition to its other types. It gains haste until end of turn. Sacrifice it at the beginning of the next end step.";
const RELIC: &str = "Kicker {3}\nIndestructible\nWhen this artifact enters, if it was kicked, create two tapped tokens that are copies of this artifact.\n{T}: Add one mana of any color.";
const SOUL: &str = "Soul of Eternity's power and toughness are each equal to your life total.\nEncore {7}{W}{W} ({7}{W}{W}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";
const QUASIDUPLICATE: &str = "Create a token that's a copy of target creature you control.\nJump-start (You may cast this card from your graveyard by discarding a card in addition to paying its other costs. Then exile this card.)";

pub(super) fn fund_creation_response(
    runner: &mut GameRunner,
    player: engine::types::player::PlayerId,
) {
    for color in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ] {
        for _ in 0..12 {
            let _ = runner
                .state_mut()
                .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
        }
    }
}

#[test]
fn mishra_divine_substitution_discards_copy_body_and_keeps_later_instructions() {
    for divine in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mishra = scenario
            .add_creature_from_oracle(P0, "Mishra, Eminent One", 2, 4, MISHRA)
            .id();
        let relic = scenario
            .add_artifact_from_oracle(P0, "Skyclave Relic", RELIC)
            .id();
        if divine {
            scenario.add_enchantment_from_oracle(P0, "Divine Visitation", DIVINE);
        }
        for player in [P0, P1] {
            scenario.with_library_top(player, &["Plains", "Plains", "Plains"]);
        }
        let mut runner = scenario.build();
        assert!(runner.state().objects[&relic]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Artifact));
        assert!(!runner.state().objects[&relic]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature));
        runner.advance_to_phase(Phase::BeginCombat);
        for _ in 0..20 {
            match runner.state().waiting_for.clone() {
                WaitingFor::TriggerTargetSelection { .. } => {
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(relic)),
                        })
                        .unwrap();
                }
                WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                _ => break,
            }
        }
        engine::game::layers::flush_layers(runner.state_mut());
        let delayed = runner
            .state()
            .delayed_triggers
            .iter()
            .find(|trigger| {
                trigger.source_id == mishra
                    && matches!(
                        trigger.ability.effect,
                        engine::types::ability::Effect::Sacrifice { .. }
                    )
            })
            .expect("Mishra's ordinary delayed sacrifice");
        let births = delayed.ability.target_incarnations.clone();
        assert_eq!(births.len(), 1);
        let token = &runner.state().objects[&births[0].object_id];
        assert_eq!((token.base_power, token.base_toughness), (Some(4), Some(4)));
        assert!(token
            .keywords
            .contains(&engine::types::keywords::Keyword::Haste));
        if divine {
            assert_eq!(token.name, "Angel");
            assert_eq!(token.color, vec![ManaColor::White]);
            assert_eq!(
                token.card_types.core_types,
                vec![engine::types::card_type::CoreType::Creature]
            );
            assert_eq!(token.card_types.subtypes, vec!["Angel"]);
            assert!(token
                .keywords
                .contains(&engine::types::keywords::Keyword::Flying));
            assert!(token
                .keywords
                .contains(&engine::types::keywords::Keyword::Vigilance));
            assert!(!token
                .keywords
                .contains(&engine::types::keywords::Keyword::Indestructible));
            assert!(
                token.abilities.is_empty(),
                "full substitution discards copied mana ability"
            );
        } else {
            assert_eq!(token.name, "Mishra's Warform");
            assert!(token
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Artifact));
            assert!(token.card_types.subtypes.contains(&"Construct".into()));
            assert!(token
                .keywords
                .contains(&engine::types::keywords::Keyword::Indestructible));
            assert!(!token.abilities.is_empty());
        }
        reach_cleanup_stack(&mut runner, mishra);
        runner.advance_until_stack_empty();
        assert_original_births_left(&runner, &births);
        assert_eq!(runner.state().objects[&relic].zone, Zone::Battlefield);
    }
}

#[test]
fn kicked_relic_divine_noncreature_control_still_copies_the_relic() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Divine Visitation", DIVINE);
    let relic = scenario
        .add_artifact_to_hand_from_oracle(P0, "Skyclave Relic", RELIC)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut runner = scenario.build();
    fund_creation_response(&mut runner, P0);
    let before = runner.state().players[0].mana_pool.total();
    let outcome = runner.cast(relic).accept_optional().resolve();
    assert_eq!(
        before - outcome.mana_pool_total(P0) as usize,
        6,
        "printed cost plus one kicker"
    );
    assert!(!outcome.state().objects[&relic].kickers_paid.is_empty());
    let copies: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(copies.len(), 2);
    for id in copies {
        let object = &outcome.state().objects[&id];
        assert_eq!(object.name, "Skyclave Relic");
        assert!(object.tapped);
        assert!(object.kickers_paid.is_empty());
        assert!(object
            .keywords
            .contains(&engine::types::keywords::Keyword::Indestructible));
        assert!(!object
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature));
        assert!(!object.abilities.is_empty());
    }
    assert!(outcome.state().delayed_triggers.is_empty());
}

// Labelled entry/queued-child isolation attached to a real printed Encore
// activation. The full Jinnie replacement is independently public above.
pub(super) fn install_creation_pause_isolation(runner: &mut GameRunner, mode: usize) {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityModification,
        ReplacementCondition, ReplacementDefinition, TargetFilter,
    };
    use engine::types::replacements::ReplacementEvent;
    let mut definitions = Vec::new();
    if mode == 1 {
        let parsed = engine::parser::oracle::parse_oracle_text(
            JINNIE,
            "Jinnie Fay, Jetmir's Second",
            &[],
            &["Creature".into()],
            &[],
        );
        definitions.push(parsed.replacements[0].clone().condition(
            ReplacementCondition::TokenSubtypeMatches {
                subtypes: vec!["Squirrel".into()],
            },
        ));
    }
    if mode == 2 {
        definitions.push(
            ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                .valid_card(TargetFilter::Any)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: engine::types::counter::CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::SelfRef,
                    },
                )),
        );
        definitions.push(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::DOUBLE),
        );
        definitions.push(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::Plus { value: 1 }),
        );
    }
    for definition in definitions {
        let id = engine::game::zones::create_object(
            runner.state_mut(),
            engine::types::identifiers::CardId(900),
            P0,
            "Entry isolation".into(),
            Zone::Battlefield,
        );
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.base_replacement_definitions = std::sync::Arc::new(vec![definition.clone()]);
        object.replacement_definitions = vec![definition].into();
    }
}

fn public_encore_survivor(
    substitute: Option<usize>,
    reload: bool,
    later_copy: bool,
    pause_mode: usize,
) {
    let mut scenario = GameScenario::new_n_player(3, 904);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, engine::types::player::PlayerId(2)] {
        scenario.with_life(player, 100);
        scenario.with_library_top(
            player,
            &["Plains", "Plains", "Plains", "Plains", "Plains", "Plains"],
        );
    }
    scenario.with_life(P0, 20);
    let soul = scenario
        .add_creature_to_graveyard(P0, "Soul of Eternity", 0, 0)
        .from_oracle_text_with_keywords(&["Encore"], SOUL)
        .id();
    if substitute.is_some() {
        scenario.add_creature_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 3, JINNIE);
    }
    let stifle = scenario
        .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![engine::types::mana::ManaCostShard::Blue],
        })
        .id();
    let quasi = scenario
        .add_spell_to_hand_from_oracle(P0, "Quasiduplicate", false, QUASIDUPLICATE)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![engine::types::mana::ManaCostShard::Blue; 2],
        })
        .id();
    let thief=scenario.add_spell_to_hand_from_oracle(P1,"Ray of Command",true,"Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.").with_mana_cost(ManaCost::Cost {generic:3,shards:vec![engine::types::mana::ManaCostShard::Blue]}).id();
    if pause_mode == 1 {
        scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    }
    let mut runner = scenario.build();
    install_creation_pause_isolation(&mut runner, pause_mode);
    fund_creation_response(&mut runner, P0);
    let index = runner.state().objects[&soul]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                ability.effect.as_ref(),
                engine::types::ability::Effect::Encore
            )
        })
        .expect("normal synthesized Encore activation");
    let outcome = runner.activate(soul, index).resolve();
    assert_eq!(
        outcome.state().objects[&soul].zone,
        Zone::Exile,
        "real activation cost exiles the card"
    );
    if substitute.is_some() || matches!(pause_mode, 1 | 2) {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(runner.state().delayed_triggers.is_empty());
    }
    if reload {
        runner = GameRunner::from_state(
            serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                serde_json::to_value(
                    engine::types::resolution::ResolutionStateWire::from_game_state(
                        runner.state().clone(),
                    ),
                )
                .unwrap(),
            )
            .unwrap()
            .into_game_state(),
        );
    }
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .unwrap();
            }
            WaitingFor::ChooseOneOfBranch { .. } => {
                runner
                    .act(GameAction::ChooseBranch {
                        index: substitute.unwrap_or(1),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } if runner.state().resolution_stack.is_empty() => break,
            other => panic!("unexpected Encore child: {other:?}"),
        }
    }
    engine::game::layers::flush_layers(runner.state_mut());
    let delayed = runner
        .state()
        .delayed_triggers
        .iter()
        .find(|trigger| trigger.source_id == soul)
        .unwrap();
    let births = delayed.ability.target_incarnations.clone();
    assert_eq!(births.len(), if pause_mode == 1 { 4 } else { 2 });
    for member in &births {
        let object = &runner.state().objects[&member.object_id];
        assert!(object
            .keywords
            .contains(&engine::types::keywords::Keyword::Haste));
        if substitute == Some(1) {
            assert_eq!(object.name, "Dog");
            assert!(!object
                .base_keywords
                .contains(&engine::types::keywords::Keyword::Haste));
        } else if substitute.is_none() {
            assert!(!object
                .base_keywords
                .contains(&engine::types::keywords::Keyword::Haste));
            if object.name == "Soul of Eternity" {
                assert_eq!(object.power, Some(if pause_mode == 2 { 23 } else { 20 }));
            } else {
                assert_eq!(
                    (
                        object.name.as_str(),
                        object.base_power,
                        object.base_toughness
                    ),
                    ("Dog", Some(3), Some(1))
                );
            }
        }
        assert!(engine::game::combat::creature_must_attack(
            runner.state(),
            member.object_id
        ));
    }
    if pause_mode == 3 {
        for _ in 0..3 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            ) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        fund_creation_response(&mut runner, P1);
        runner
            .cast(thief)
            .target_object(births[0].object_id)
            .resolve();
        engine::game::layers::flush_layers(runner.state_mut());
        let stolen = &runner.state().objects[&births[0].object_id];
        assert_eq!(stolen.controller, P1);
        assert_eq!(ObjectIncarnationRef::from_object(stolen), births[0]);
        assert!(stolen
            .keywords
            .contains(&engine::types::keywords::Keyword::Haste));
        assert_eq!(
            runner
                .state()
                .delayed_triggers
                .iter()
                .find(|trigger| trigger.source_id == soul)
                .unwrap()
                .controller,
            P0
        );
    }
    let creating_turn = runner.state().turn_number;
    let trigger = reach_cleanup_stack(&mut runner, soul);
    creation_priority_to_caster(&mut runner);
    fund_creation_response(&mut runner, P0);
    let counter = runner.cast(stifle).target_object(trigger).resolve();
    assert!(counter.events().iter().any(
        |event| matches!(event,GameEvent::SpellCountered { object_id,.. } if *object_id==trigger)
    ));
    for _ in 0..300 {
        if runner.state().turn_number > creating_turn
            && runner.state().active_player == P0
            && runner.state().phase == Phase::PreCombatMain
        {
            break;
        }
        pass_creation_window(&mut runner);
    }
    assert!(runner.state().turn_number > creating_turn);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    engine::game::layers::flush_layers(runner.state_mut());
    for member in &births {
        let object = &runner.state().objects[&member.object_id];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(ObjectIncarnationRef::from_object(object), *member);
        assert!(
            object
                .keywords
                .contains(&engine::types::keywords::Keyword::Haste),
            "no stated duration: survives real cleanup"
        );
        assert!(
            !engine::game::combat::creature_must_attack(runner.state(), member.object_id),
            "only this-turn requirement expires"
        );
    }
    assert!(runner
        .state()
        .delayed_triggers
        .iter()
        .all(|trigger| trigger.source_id != soul));
    if later_copy {
        fund_creation_response(&mut runner, P0);
        let outcome = runner
            .cast(quasi)
            .target_object(births[0].object_id)
            .replacement_choice(1)
            .resolve();
        let new: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(new.len(), if pause_mode == 1 { 2 } else { 1 });
        let copy = new
            .iter()
            .map(|id| &outcome.state().objects[id])
            .find(|object| object.name == outcome.state().objects[&births[0].object_id].name)
            .unwrap();
        assert_eq!(
            copy.name,
            outcome.state().objects[&births[0].object_id].name
        );
        assert_eq!(
            copy.keywords
                .contains(&engine::types::keywords::Keyword::Haste),
            substitute == Some(0),
            "only intrinsic Cat haste is copiable"
        );
        assert!(outcome.state().objects[&births[0].object_id]
            .keywords
            .contains(&engine::types::keywords::Keyword::Haste));
    }
}

#[test]
fn encore_survivor_keeps_granted_haste_after_cleanup() {
    public_encore_survivor(None, false, false, 0);
}
#[test]
fn encore_substitute_survivor_has_permanent_noncopiable_haste() {
    public_encore_survivor(Some(1), false, true, 0);
}
#[test]
fn encore_survivor_pause_reload_keeps_distinct_grant_durations() {
    public_encore_survivor(Some(1), true, true, 0);
}
#[test]
fn encore_survivor_later_copy_excludes_granted_haste() {
    public_encore_survivor(None, false, true, 0);
    public_encore_survivor(Some(0), false, true, 0);
}

#[test]
fn encore_queued_and_counter_children_resume_into_real_cleanup_survivor_and_copy() {
    public_encore_survivor(None, true, true, 1);
    public_encore_survivor(None, true, true, 2);
}

#[test]
fn encore_permanent_haste_survives_public_control_response_and_creating_cleanup() {
    public_encore_survivor(None, false, false, 3);
}

// A complete parsed creation/cleanup instruction deliberately isolates the
// replacement from Broken's opponent-turn attack restriction.
fn public_replaced_creation(
    replacements: &[(&str, &str)],
    branch: Option<usize>,
    copy_choice: bool,
    reload: bool,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Plains", "Plains", "Plains", "Plains"]);
    }
    let chosen = scenario.add_creature(P0, "Chosen seven eight", 7, 8).id();
    let irrelevant = scenario
        .add_creature(P0, "Independent eleven thirteen", 11, 13)
        .id();
    for (name, text) in replacements {
        scenario.add_creature_from_oracle(P0, name, 3, 3, text);
    }
    let spell=scenario.add_spell_to_hand_from_oracle(P0,"Synthetic replacement consumer",false,"Create two 1/1 white Soldier creature tokens. Sacrifice those tokens at the beginning of the next end step.").with_mana_cost(ManaCost::generic(1)).id();
    let mut runner = scenario.build();
    fund_creation_response(&mut runner, P0);
    let outcome = runner.cast(spell).resolve();
    let mut events = outcome.events().to_vec();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(runner.state().delayed_triggers.is_empty());
    let mut saved = false;
    let mut source_choices = 0;
    let mut branch_choices = 0;
    for _ in 0..80 {
        if reload && !saved {
            runner = GameRunner::from_state(
                serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                    serde_json::to_value(
                        engine::types::resolution::ResolutionStateWire::from_game_state(
                            runner.state().clone(),
                        ),
                    )
                    .unwrap(),
                )
                .unwrap()
                .into_game_state(),
            );
            saved = true;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement {
                index: if branch.is_none() && !copy_choice {
                    1
                } else {
                    0
                },
            },
            WaitingFor::ChooseOneOfBranch { .. } => {
                branch_choices += 1;
                GameAction::ChooseBranch {
                    index: branch.unwrap(),
                }
            }
            WaitingFor::CopyTargetChoice {
                purpose:
                    engine::types::ability::CopyTargetPurpose::CopyTokenSource {
                        continuation: Some(ref tail),
                    },
                ..
            } => {
                source_choices += 1;
                assert!(tail.context.token_creation_parent.is_some());
                assert_eq!(
                    tail.context
                        .token_creation_parent
                        .as_ref()
                        .unwrap()
                        .substitution_count,
                    2
                );
                // Save the actual authority-bearing prompt as well as the
                // optional replacement prompt, before answering it publicly.
                if reload {
                    runner = GameRunner::from_state(
                        serde_json::from_value::<GameState>(
                            serde_json::to_value(runner.state()).unwrap(),
                        )
                        .unwrap(),
                    );
                }
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(chosen)),
                }
            }
            WaitingFor::Priority { .. } if runner.state().resolution_stack.is_empty() => break,
            other => panic!("unresolved replacement consumer: {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    let births = events
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(births.len(), 2);
    assert_eq!(source_choices, usize::from(copy_choice));
    assert_eq!(branch_choices, usize::from(branch.is_some()));
    for id in &births {
        let object = &runner.state().objects[id];
        assert_eq!(object.zone, Zone::Battlefield);
        if copy_choice {
            assert_eq!(object.name, "Chosen seven eight");
            assert_eq!(
                (object.base_power, object.base_toughness),
                (Some(7), Some(8))
            );
        } else if let Some(index) = branch {
            assert_eq!(
                (
                    object.name.as_str(),
                    object.base_power,
                    object.base_toughness
                ),
                if index == 0 {
                    ("Cat", Some(2), Some(2))
                } else {
                    ("Dog", Some(3), Some(1))
                }
            );
        } else {
            assert_eq!(object.name, "Soldier");
            assert_eq!(
                (object.base_power, object.base_toughness),
                (Some(1), Some(1))
            );
        }
    }
    let delayed = runner
        .state()
        .delayed_triggers
        .iter()
        .find(|trigger| trigger.source_id == spell)
        .unwrap();
    assert_eq!(delayed.controller, P0);
    assert_eq!(
        delayed
            .ability
            .target_incarnations
            .iter()
            .map(|member| member.object_id)
            .collect::<std::collections::HashSet<_>>(),
        births.iter().copied().collect()
    );
    reach_cleanup_stack(&mut runner, spell);
    runner.advance_until_stack_empty();
    for id in births {
        assert_ne!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&irrelevant].zone, Zone::Battlefield);
}

#[test]
fn parsed_jinnie_accept_cat_dog_decline_and_paused_reload_capture_complete_result() {
    for branch in [None, Some(0), Some(1)] {
        public_replaced_creation(
            &[("Jinnie Fay, Jetmir's Second", JINNIE)],
            branch,
            false,
            false,
        );
    }
    public_replaced_creation(
        &[("Jinnie Fay, Jetmir's Second", JINNIE)],
        Some(1),
        false,
        true,
    );
}

#[test]
fn full_esix_chosen_source_count_and_saved_tail_complete_public_cleanup() {
    let esix="Flying\nThe first time you would create one or more tokens during each of your turns, you may instead choose a creature other than Esix and create that many tokens that are copies of that creature.";
    public_replaced_creation(&[("Esix, Fractal Bloom", esix)], None, true, false);
    public_replaced_creation(&[("Esix, Fractal Bloom", esix)], None, true, true);
}

#[test]
fn public_manufactor_treasure_cleanup_uses_all_predefined_births() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Academy Manufactor",
        1,
        3,
        "If you would create a Clue, Food, or Treasure token, instead create one of each.",
    );
    let old = scenario.add_creature(P0, "Prior survivor", 3, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Treasure cleanup",
            false,
            "Create a Treasure token. Sacrifice that token at the beginning of the next end step.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let prior = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic prior Treasure",
            false,
            "Create a Treasure token.",
        )
        .id();
    let later = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic later Treasure",
            true,
            "Create a Treasure token.",
        )
        .id();
    let mut runner = scenario.build();
    fund_creation_response(&mut runner, P0);
    let prior_result = runner.cast(prior).resolve();
    let mut unrelated = prior_result
        .events()
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(unrelated.len(), 3);
    creation_priority_to_caster(&mut runner);

    let outcome = runner.cast(spell).resolve();
    let births = outcome
        .events()
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(births.len(), 3);
    for name in ["Clue", "Food", "Treasure"] {
        let body = outcome
            .state()
            .objects
            .values()
            .find(|object| births.contains(&object.id) && object.name == name)
            .unwrap();
        assert_eq!(
            body.card_types.core_types,
            vec![engine::types::card_type::CoreType::Artifact]
        );
        assert!(!body.abilities.is_empty());
        assert_eq!((body.power, body.toughness), (None, None));
    }
    let delayed = runner
        .state()
        .delayed_triggers
        .iter()
        .find(|trigger| trigger.source_id == spell)
        .unwrap();
    assert_eq!(delayed.ability.target_incarnations.len(), 3);
    creation_priority_to_caster(&mut runner);
    let later_result = runner.cast(later).resolve();
    let later_births = later_result
        .events()
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(later_births.len(), 3);
    unrelated.extend(later_births);
    reach_cleanup_stack(&mut runner, spell);
    runner.advance_until_stack_empty();
    for id in births {
        assert_ne!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert_eq!(runner.state().objects[&old].zone, Zone::Battlefield);
    for id in unrelated {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
}

#[test]
fn full_thraben_inspector_manufactor_investigates_once_for_three_complete_entries() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Academy Manufactor",
        1,
        3,
        "If you would create a Clue, Food, or Treasure token, instead create one of each.",
    );
    let inspector=scenario.add_creature_to_hand(P0,"Thraben Inspector",1,2).from_oracle_text("When this creature enters, investigate. (Create a Clue token. It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")").with_mana_cost(ManaCost::Cost {generic:0,shards:vec![engine::types::mana::ManaCostShard::White]}).id();
    let mut runner = scenario.build();
    fund_creation_response(&mut runner, P0);
    let outcome = runner.cast(inspector).resolve();
    assert_eq!(outcome.state().objects[&inspector].zone, Zone::Battlefield);
    assert_eq!(
        outcome
            .events()
            .iter()
            .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
            .count(),
        3
    );
    assert_eq!(
        outcome
            .events()
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: engine::types::events::PlayerActionKind::Investigate,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(outcome.state().resolution_stack.is_empty());
}

#[test]
fn late_primary_etb_tap_override_does_not_seed_added_squirrel() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source=scenario.add_planeswalker_from_oracle(P0,"Saheeli Rai","Saheeli",3,"[+1]: Scry 1. Saheeli Rai deals 1 damage to each opponent.\n[−2]: Create a token that's a copy of target artifact or creature you control, except it's an artifact in addition to its other types. That token gains haste. Exile it at the beginning of the next end step.\n[−7]: Search your library for up to three artifact cards with different names, put them onto the battlefield, then shuffle.").id();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Skyclave Relic", RELIC)
        .id();
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    scenario.add_enchantment_from_oracle(P0, "Root Maze", "Artifacts and lands enter tapped.");
    let mut runner = scenario.build();
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                ability.effect.as_ref(),
                engine::types::ability::Effect::CopyTokenOf { .. }
            )
        })
        .unwrap();
    let outcome = runner
        .activate(source, index)
        .target_object(relic)
        .resolve();
    let ids = outcome
        .events()
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    let primary = ids
        .iter()
        .map(|id| &outcome.state().objects[id])
        .find(|object| object.name == "Skyclave Relic")
        .unwrap();
    assert!(primary.tapped);
    assert!(primary
        .keywords
        .contains(&engine::types::keywords::Keyword::Indestructible));
    let squirrel = ids
        .iter()
        .map(|id| &outcome.state().objects[id])
        .find(|object| object.name == "Squirrel")
        .unwrap();
    assert!(!squirrel.tapped);
    assert_eq!(
        (squirrel.base_power, squirrel.base_toughness),
        (Some(1), Some(1))
    );
    assert_eq!(
        squirrel.card_types.core_types,
        vec![engine::types::card_type::CoreType::Creature]
    );
    assert!(!squirrel
        .keywords
        .contains(&engine::types::keywords::Keyword::Indestructible));
    for id in &ids {
        assert!(outcome.state().objects[id]
            .keywords
            .contains(&engine::types::keywords::Keyword::Haste));
    }
    let delayed = outcome
        .state()
        .delayed_triggers
        .iter()
        .find(|trigger| trigger.source_id == source)
        .unwrap();
    assert_eq!(delayed.ability.target_incarnations.len(), 2);
    for _ in 0..150 {
        if runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == source)
            && runner.state().phase == Phase::End
        {
            break;
        }
        pass_creation_window(&mut runner);
    }
    assert_eq!(runner.state().phase, Phase::End);
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == source));
    runner.advance_until_stack_empty();
    for id in ids {
        assert_ne!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert_eq!(runner.state().objects[&relic].zone, Zone::Battlefield);
}

#[test]
fn full_determined_iteration_multi_pick_populate_captures_only_its_complete_births() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    let source=scenario.add_enchantment_from_oracle(P0,"Determined Iteration","At the beginning of combat on your turn, populate. The token created this way gains haste. Sacrifice it at the beginning of the next end step. (To populate, create a token that's a copy of a creature token you control.)").id();
    let maker = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic two distinct token pool",
            false,
            "Create a 2/3 blue Drake creature token. Create a 4/5 red Giant creature token.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    fund_creation_response(&mut runner, P0);
    let creation = runner.cast(maker).resolve();
    let old = creation
        .events()
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(old.len(), 4);
    let chosen = old
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Drake")
        .unwrap();
    runner.advance_to_phase(Phase::BeginCombat);
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::PopulateChoice { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PopulateChoice { .. }
    ));
    assert!(!runner
        .state()
        .delayed_triggers
        .iter()
        .any(|trigger| trigger.source_id == source));
    let result = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        })
        .unwrap();
    let births = result
        .events
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(births.len(), 2);
    assert!(births.iter().all(|id| !old.contains(id)));
    let delayed = runner
        .state()
        .delayed_triggers
        .iter()
        .find(|trigger| trigger.source_id == source)
        .unwrap();
    assert_eq!(delayed.ability.target_incarnations.len(), 2);
    for id in &births {
        assert!(runner.state().objects[id]
            .keywords
            .contains(&engine::types::keywords::Keyword::Haste));
    }
    reach_cleanup_stack(&mut runner, source);
    runner.advance_until_stack_empty();
    for id in births {
        assert_ne!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    for id in old {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
}

#[test]
fn full_mirror_match_uses_blocking_completion_and_complete_end_combat_exile() {
    let text="Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";
    let mut b = board_with_replacements(
        text,
        (2, 3),
        false,
        false,
        "Draw a card.",
        &[("Chatterfang, Squirrel General", CHATTERFANG)],
    );
    for _ in 0..30 {
        if b.runner.state().phase == Phase::DeclareBlockers {
            break;
        }
        pass_creation_window(&mut b.runner);
    }
    if matches!(
        b.runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        b.runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .unwrap();
    }
    creation_priority_to_caster(&mut b.runner);
    let result = b.runner.cast(b.spell).resolve();
    let births = result
        .events()
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(births.len(), 2);
    let attacker = result
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .find(|attacker| attacker.object_id == b.b)
        .unwrap();
    for id in &births {
        assert!(attacker.blocked);
        assert!(result.state().combat.as_ref().unwrap().blocker_assignments[&b.b].contains(id));
    }
    let delayed = result
        .state()
        .delayed_triggers
        .iter()
        .find(|trigger| trigger.source_id == b.spell)
        .unwrap();
    assert_eq!(delayed.ability.target_incarnations.len(), 2);
    for _ in 0..80 {
        if b.runner.state().phase == Phase::EndCombat && !b.runner.state().stack.is_empty() {
            break;
        }
        pass_creation_window(&mut b.runner);
    }
    assert_eq!(b.runner.state().phase, Phase::EndCombat);
    assert!(!b.runner.state().stack.is_empty());
    b.runner.advance_until_stack_empty();
    for id in births {
        assert_ne!(b.runner.state().objects[&id].zone, Zone::Battlefield);
    }
}

#[test]
fn full_banshee_myriad_replaced_segment_attacks_assigned_player_and_exiles_complete_result() {
    let p2 = engine::types::player::PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 908);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, p2] {
        scenario.with_life(player, 100);
        scenario.with_library_top(player, &["Plains", "Plains", "Plains"]);
    }
    let banshee=scenario.add_creature(P0,"Banshee of the Dread Choir",4,4).from_oracle_text_with_keywords(&["Myriad"],"Myriad (Whenever this creature attacks, for each opponent other than defending player, you may create a token copy that's tapped and attacking that player or a planeswalker they control. Exile the tokens at end of combat.)\nWhenever this creature deals combat damage to a player, that player discards a card.").id();
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(banshee, AttackTarget::Player(P1))])
        .unwrap();
    let mut events = Vec::new();
    for _ in 0..40 {
        if runner
            .state()
            .delayed_triggers
            .iter()
            .any(|trigger| trigger.source_id == banshee)
        {
            break;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected Myriad prompt: {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    let delayed = runner
        .state()
        .delayed_triggers
        .iter()
        .find(|trigger| trigger.source_id == banshee)
        .unwrap();
    let births = delayed.ability.target_incarnations.clone();
    assert_eq!(births.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
            .count(),
        2
    );
    for member in &births {
        assert!(runner.state().objects[&member.object_id].tapped);
        let attacking = runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .find(|attacker| attacker.object_id == member.object_id)
            .unwrap();
        assert_eq!(attacking.defending_player, p2);
    }
    for _ in 0..120 {
        if runner.state().phase == Phase::EndCombat && !runner.state().stack.is_empty() {
            break;
        }
        pass_creation_window(&mut runner);
    }
    assert_eq!(runner.state().phase, Phase::EndCombat);
    runner.advance_until_stack_empty();
    assert_original_births_left(&runner, &births);
    assert_eq!(runner.state().objects[&banshee].zone, Zone::Battlefield);
}

#[test]
fn broken_cleanup_keeps_installing_controller_for_theft_regain_and_split_control() {
    const RAY:&str="Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.";
    for regain in [false, true] {
        let mut b = board(FULL_BROKEN, (7, 8), false, true, RAY);
        let births = full_broken_result(&mut b, 2);
        let captured = births.clone();
        // The opponent acts at normal priority and resolves the full printed
        // steal instruction; controller differs without changing incarnation.
        for _ in 0..2 {
            if matches!(
                b.runner.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            ) {
                break;
            }
            b.runner.act(GameAction::PassPriority).unwrap();
        }
        fund_creation_response(&mut b.runner, P1);
        b.runner
            .cast(b.opponent_response)
            .target_object(births[0].object_id)
            .resolve();
        assert_eq!(
            b.runner.state().objects[&births[0].object_id].controller,
            P1
        );
        assert_eq!(
            ObjectIncarnationRef::from_object(&b.runner.state().objects[&births[0].object_id]),
            births[0]
        );
        if regain {
            creation_priority_to_caster(&mut b.runner);
            fund_creation_response(&mut b.runner, P0);
            b.runner
                .cast(b.response)
                .target_object(births[0].object_id)
                .resolve();
            assert_eq!(
                b.runner.state().objects[&births[0].object_id].controller,
                P0
            );
        }
        let delayed = b
            .runner
            .state()
            .delayed_triggers
            .iter()
            .find(|trigger| trigger.source_id == b.spell)
            .unwrap();
        assert_eq!(delayed.controller, P0);
        assert_eq!(delayed.ability.controller, P0);
        assert_eq!(delayed.ability.target_incarnations, captured);
        reach_cleanup_stack(&mut b.runner, b.spell);
        b.runner.advance_until_stack_empty();
        assert_ne!(
            b.runner.state().objects[&births[1].object_id].zone,
            Zone::Battlefield
        );
        assert_eq!(
            b.runner.state().objects[&births[0].object_id].zone == Zone::Battlefield,
            !regain
        );
        assert_eq!(b.runner.state().objects[&b.c].zone, Zone::Battlefield);
        assert!(b
            .runner
            .state()
            .delayed_triggers
            .iter()
            .all(|trigger| trigger.source_id != b.spell));
    }
}

#[test]
fn installed_cleanup_storage_pin_never_repins_a_reused_object_id() {
    let mut b = board(FULL_BROKEN, (7, 8), false, true, STIFLE);
    let births = full_broken_result(&mut b, 2);
    // Explicit storage isolation, not token blink: a stale saved birth and a
    // distinct epoch deliberately share the stable object storage identifier.
    b.runner
        .state_mut()
        .objects
        .get_mut(&births[0].object_id)
        .unwrap()
        .bump_incarnation();
    let changed =
        ObjectIncarnationRef::from_object(&b.runner.state().objects[&births[0].object_id]);
    assert_ne!(changed, births[0]);
    let state =
        serde_json::from_value::<GameState>(serde_json::to_value(b.runner.state()).unwrap())
            .unwrap();
    assert_eq!(
        state
            .delayed_triggers
            .iter()
            .find(|trigger| trigger.source_id == b.spell)
            .unwrap()
            .ability
            .target_incarnations,
        births
    );
    b.runner = GameRunner::from_state(state);
    reach_cleanup_stack(&mut b.runner, b.spell);
    b.runner.advance_until_stack_empty();
    assert_eq!(
        b.runner.state().objects[&changed.object_id].zone,
        Zone::Battlefield
    );
    assert_ne!(
        b.runner.state().objects[&births[1].object_id].zone,
        Zone::Battlefield
    );
}

#[test]
fn installed_full_broken_cleanup_survives_public_exile_of_its_source() {
    let mut b = board(
        FULL_BROKEN,
        (7, 8),
        false,
        false,
        "Exile target card from a graveyard.",
    );
    let births = full_broken_result(&mut b, 1);
    creation_priority_to_caster(&mut b.runner);
    b.runner.cast(b.response).target_object(b.spell).resolve();
    assert_eq!(b.runner.state().objects[&b.spell].zone, Zone::Exile);
    reach_cleanup_stack(&mut b.runner, b.spell);
    b.runner.advance_until_stack_empty();
    assert_original_births_left(&b.runner, &births);
    assert_eq!(b.runner.state().objects[&b.c].zone, Zone::Battlefield);
}

#[test]
fn full_broken_jinnie_cat_dog_decline_and_current_choice_reload_bind_only_actual_births() {
    for branch in [None, Some(0), Some(1)] {
        for reload in [false, true] {
            let mut b = board_with_replacements(
                FULL_BROKEN,
                (7, 8),
                false,
                false,
                STIFLE,
                &[("Jinnie Fay, Jetmir's Second", JINNIE)],
            );
            let before = b.runner.state().players[0].mana_pool.total();
            let outcome = b.runner.cast(b.spell).target_object(b.b).resolve();
            let mut events = outcome.events().to_vec();
            assert_eq!(before - outcome.mana_pool_total(P0) as usize, 5);
            assert_eq!(b.runner.state().objects[&b.b].zone, Zone::Graveyard);
            assert!(matches!(
                b.runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            assert!(b.runner.state().delayed_triggers.is_empty());
            if reload {
                b.runner = GameRunner::from_state(
                    serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                        serde_json::to_value(
                            engine::types::resolution::ResolutionStateWire::from_game_state(
                                b.runner.state().clone(),
                            ),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                    .into_game_state(),
                );
            }
            events.extend(
                b.runner
                    .act(GameAction::ChooseReplacement {
                        index: if branch.is_some() { 0 } else { 1 },
                    })
                    .unwrap()
                    .events,
            );
            if let Some(branch) = branch {
                assert!(matches!(
                    b.runner.state().waiting_for,
                    WaitingFor::ChooseOneOfBranch { .. }
                ));
                if reload {
                    b.runner = GameRunner::from_state(
                        serde_json::from_value::<GameState>(
                            serde_json::to_value(b.runner.state()).unwrap(),
                        )
                        .unwrap(),
                    );
                }
                events.extend(
                    b.runner
                        .act(GameAction::ChooseBranch { index: branch })
                        .unwrap()
                        .events,
                );
            }
            assert_eq!(b.runner.state().objects[&b.spell].zone, Zone::Graveyard);
            assert!(b.runner.state().resolution_stack.is_empty());
            let delayed = b
                .runner
                .state()
                .delayed_triggers
                .iter()
                .find(|trigger| trigger.source_id == b.spell)
                .unwrap();
            let births = delayed.ability.target_incarnations.clone();
            assert_eq!(births.len(), 1);
            let token = &b.runner.state().objects[&births[0].object_id];
            assert_eq!(
                (token.name.as_str(), token.base_power, token.base_toughness),
                match branch {
                    None => ("Spirit", Some(7), Some(8)),
                    Some(0) => ("Cat", Some(2), Some(2)),
                    _ => ("Dog", Some(3), Some(1)),
                }
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                    .count(),
                1
            );
            reach_cleanup_stack(&mut b.runner, b.spell);
            b.runner.advance_until_stack_empty();
            assert_original_births_left(&b.runner, &births);
            assert_eq!(b.runner.state().objects[&b.c].zone, Zone::Battlefield);
        }
    }
}
