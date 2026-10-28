//! Coordinated card populations through the production casting pipeline.
use super::jev_spell_history_context::printed_db;
use super::support::shared_card_export_json;
use engine::database::card_db::CardDatabase;
use engine::game::game_object::BackFaceData;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, ContinuousModification, ControllerRef, Duration, Effect, FilterProp, QuantityExpr,
    QuantityRef, StaticCondition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, MorphCost};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;
use serde_json::{to_string, Map, Value};
use std::path::Path;

const SAILORS: &str = "This spell costs {1} less to cast for each card you own in exile and in your graveyard that's an instant card, a sorcery card, or a card that has an Adventure.\nWard {4} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {4}.)";

const LOCKET: &str = "Spells you cast cost {1} less to cast for each card with the same name as that spell in your graveyard.";

/// CR 601.2f + CR 700.6: overlapping colors/historic qualities cause one
/// application of each printed reducer, with the colored component retained.
#[test]
fn retained_color_and_historic_cost_subjects_apply_once_per_source() {
    let db = printed_db();
    for source in ["Nightscape Familiar", "Jhoira's Familiar"] {
        for matching in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_real_card(P0, source, Zone::Battlefield, &db);
            let spell = if source == "Nightscape Familiar" {
                scenario
                    .add_spell_to_hand_from_oracle(
                        P0,
                        "Synthetic overlapping color instant",
                        true,
                        "You gain 1 life.",
                    )
                    .with_mana_cost(if matching {
                        ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
                        }
                    } else {
                        ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                        }
                    })
                    .id()
            } else if matching {
                scenario
                    .add_artifact_to_hand_from_oracle(P0, "Synthetic legendary artifact", "")
                    .as_legendary()
                    .with_mana_cost(blue_cost(1, 2))
                    .id()
            } else {
                scenario
                    .add_creature_to_hand_from_oracle(
                        P0,
                        "Synthetic nonhistoric creature",
                        1,
                        1,
                        "",
                    )
                    .with_mana_cost(blue_cost(1, 2))
                    .id()
            };
            scenario.with_mana_pool(
                P0,
                [ManaType::Blue, ManaType::Red, ManaType::Green]
                    .into_iter()
                    .flat_map(|kind| {
                        (0..5).map(move |_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
                    })
                    .collect(),
            );
            let mut runner = scenario.build();
            runner.cast(spell).resolve();
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                if matching { 13 } else { 12 }
            );
            assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                runner.state().stack.is_empty()
                    && runner.state().pending_trigger.is_none()
                    && runner.state().deferred_triggers.is_empty()
                    && runner.state().pending_trigger_order.is_none()
            );
        }
    }
}

/// CR 601.2f: the retained noncreature subject reduces only generic mana;
/// CR 611.3a: Lyse's live threshold reads the same turn's completed casts.
#[test]
fn retained_lyse_discount_paid_casts_prowess_and_next_turn_history_reset() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lyse = scenario.add_real_card(P0, "Lyse Hext", Zone::Battlefield, &db);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Synthetic cost creature", 1, 1, "")
        .with_mana_cost(blue_cost(1, 1))
        .id();
    let spells: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic cost instant",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(blue_cost(1, 1))
                .id()
        })
        .collect();
    scenario.with_library_top(P0, &["P0 reserve", "P0 second"]);
    scenario.with_library_top(P1, &["P1 reserve", "P1 second"]);
    mana(&mut scenario, 8);
    let mut runner = scenario.build();
    runner.cast(creature).resolve();
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    assert!(!runner.state().objects[&lyse]
        .keywords
        .contains(&Keyword::DoubleStrike));
    runner.cast(spells[0]).resolve();
    assert_eq!(runner.state().players[0].mana_pool.total(), 5);
    assert_eq!(runner.state().objects[&lyse].power, Some(3));
    assert!(!runner.state().objects[&lyse]
        .keywords
        .contains(&Keyword::DoubleStrike));
    runner.cast(spells[1]).resolve();
    assert_eq!(runner.state().players[0].mana_pool.total(), 4);
    assert_eq!(runner.state().objects[&lyse].power, Some(4));
    assert!(runner.state().objects[&lyse]
        .keywords
        .contains(&Keyword::DoubleStrike));
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 3);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(
        runner.state().stack.is_empty()
            && runner.state().pending_trigger.is_none()
            && runner.state().deferred_triggers.is_empty()
            && runner.state().pending_trigger_order.is_none()
    );
    let old_turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(runner.state().turn_number > old_turn);
    assert!(runner
        .state()
        .spells_cast_this_turn_by_player
        .values()
        .all(|records| records.is_empty()));
    assert_eq!(runner.state().objects[&lyse].controller, P0);
    assert_eq!(runner.state().objects[&lyse].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&lyse].power, Some(2));
    assert!(!runner.state().objects[&lyse]
        .keywords
        .contains(&Keyword::DoubleStrike));
    assert!(
        runner.state().stack.is_empty()
            && runner.state().pending_trigger.is_none()
            && runner.state().deferred_triggers.is_empty()
            && runner.state().pending_trigger_order.is_none()
    );
}

/// CR 601.2f + CR 601.2i: the in-flight Guardian cannot count itself as an
/// earlier noncreature. A prior creature is the paid negative twin.
#[test]
fn retained_academy_guardian_uses_actual_prior_noncreature_and_pays_full_colored_cost() {
    let db = printed_db();
    for noncreature in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let guardian = scenario.add_real_card(P0, "Traxos, Academy Guardian", Zone::Hand, &db);
        let prior = if noncreature {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic prior instant",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(blue_cost(0, 1))
                .id()
        } else {
            scenario
                .add_creature_to_hand_from_oracle(P0, "Synthetic prior creature", 1, 1, "")
                .with_mana_cost(blue_cost(0, 1))
                .id()
        };
        mana(&mut scenario, 8);
        let mut runner = scenario.build();
        runner.cast(prior).resolve();
        assert_eq!(runner.state().players[0].mana_pool.total(), 7);
        let committed = runner.cast(guardian).commit();
        assert_eq!(
            committed.state().players[0].mana_pool.total(),
            if noncreature { 5 } else { 3 }
        );
        assert_eq!(
            committed.state().spells_cast_this_turn_by_player[&P0].len(),
            2
        );
        let outcome = committed.resolve();
        outcome.assert_zone(&[guardian], Zone::Battlefield);
        for keyword in [Keyword::Flying, Keyword::Vigilance, Keyword::Prowess] {
            assert!(outcome.state().objects[&guardian]
                .keywords
                .contains(&keyword));
        }
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(
            outcome.state().stack.is_empty()
                && outcome.state().pending_trigger.is_none()
                && outcome.state().deferred_triggers.is_empty()
                && outcome.state().pending_trigger_order.is_none()
        );
    }
}

/// CR 611.3a + CR 601.2f: Lyse reads completed casts immediately on entry;
/// its live reducer stops functioning when a real bounce removes its source.
#[test]
fn retained_lyse_late_entry_and_departure_use_existing_history_and_live_source() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lyse = scenario.add_real_card(P0, "Lyse Hext", Zone::Hand, &db);
    let prior: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic earlier noncreature",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(blue_cost(0, 1))
                .id()
        })
        .collect();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic reducer bounce",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(blue_cost(1, 1))
        .id();
    let after = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic post-departure instant",
            true,
            "You gain 1 life.",
        )
        .with_mana_cost(blue_cost(1, 1))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..10)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .chain(std::iter::once(ManaUnit::new(
                ManaType::White,
                ObjectId(0),
                false,
                vec![],
            )))
            .collect(),
    );
    let mut runner = scenario.build();
    for spell in prior {
        runner.cast(spell).resolve();
    }
    runner.cast(lyse).resolve();
    assert!(runner.state().objects[&lyse]
        .keywords
        .contains(&Keyword::DoubleStrike));
    assert_eq!(runner.state().objects[&lyse].power, Some(2));
    let before = runner.state().players[0].mana_pool.total();
    runner.cast(bounce).target_object(lyse).resolve();
    assert_eq!(runner.state().players[0].mana_pool.total(), before - 1);
    assert_eq!(runner.state().objects[&lyse].zone, Zone::Hand);
    let before = runner.state().players[0].mana_pool.total();
    runner.cast(after).resolve();
    assert_eq!(runner.state().players[0].mana_pool.total(), before - 2);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 5);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(
        runner.state().stack.is_empty()
            && runner.state().pending_trigger.is_none()
            && runner.state().deferred_triggers.is_empty()
            && runner.state().pending_trigger_order.is_none()
    );
}

/// CR 601.2f: two distinct reducers apply once each, the generic floor is
/// zero, and neither reducer can pay an absent colored shard.
#[test]
fn retained_lyse_cost_floor_colored_failure_and_foreign_source() {
    let db = printed_db();
    for (source_controller, reducers, generic, blue, allowed, paid) in [
        (P0, 1, 0, 1, true, 1),
        (P0, 2, 2, 1, true, 1),
        (P0, 1, 1, 0, false, 0),
        (P0, 1, 2, 1, false, 0),
        (P1, 1, 1, 2, true, 2),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(source_controller, "Lyse Hext", Zone::Battlefield, &db);
        if reducers == 2 {
            scenario.add_real_card(P0, "Nightscape Familiar", Zone::Battlefield, &db);
        }
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic generic floor instant",
                true,
                "You gain 1 life.",
            )
            .with_mana_cost(blue_cost(generic, 1))
            .id();
        mana(&mut scenario, blue);
        let mut runner = scenario.build();
        if blue == 0 {
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::White,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        let before = runner.state().players[0].mana_pool.total();
        let result = runner.cast(spell).try_resolve();
        if allowed {
            result.unwrap();
            assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
        } else {
            assert!(result.is_err());
            assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
            assert!(runner
                .state()
                .spells_cast_this_turn_by_player
                .get(&P0)
                .is_none_or(|records| records.is_empty()));
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), before - paid);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            runner.state().stack.is_empty()
                && runner.state().pending_trigger.is_none()
                && runner.state().deferred_triggers.is_empty()
                && runner.state().pending_trigger_order.is_none()
        );
    }
}

/// CR 514.1 + CR 514.2 + CR 514.3a: a real cleanup discard trigger
/// opens the lawful same-turn priority window after prowess has expired.
#[test]
fn retained_lyse_cleanup_priority_keeps_history_based_double_strike() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lyse = scenario.add_real_card(P0, "Lyse Hext", Zone::Battlefield, &db);
    scenario.add_enchantment_from_oracle(
        P0,
        "Synthetic cleanup discard witness",
        "Whenever you discard a card, you gain 1 life.",
    );
    let fillers: Vec<_> = (0..8)
        .map(|index| scenario.add_card_to_hand(P0, &format!("Cleanup card {index}")))
        .collect();
    let spells: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic cleanup history instant",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(blue_cost(1, 1))
                .id()
        })
        .collect();
    scenario.with_library_top(P1, &["Next draw", "Reserve"]);
    mana(&mut scenario, 8);
    let mut runner = scenario.build();
    for spell in spells {
        runner.cast(spell).resolve();
    }
    assert_eq!(runner.state().objects[&lyse].power, Some(4));
    assert!(runner.state().objects[&lyse]
        .keywords
        .contains(&Keyword::DoubleStrike));
    let turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::Cleanup);
    let WaitingFor::DiscardChoice {
        player,
        cards,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("real cleanup discard: {:?}", runner.state().waiting_for)
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(
        cards
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        fillers.iter().copied().collect()
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![fillers[7]],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().phase, Phase::Cleanup);
    assert_eq!(runner.state().turn_number, turn);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 2);
    assert_eq!(runner.state().objects[&lyse].power, Some(2));
    assert!(runner.state().objects[&lyse]
        .keywords
        .contains(&Keyword::DoubleStrike));
    assert_eq!(runner.life(P0), 23);
    assert_eq!(runner.state().objects[&fillers[7]].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(
        runner.state().stack.is_empty()
            && runner.state().pending_trigger.is_none()
            && runner.state().deferred_triggers.is_empty()
            && runner.state().pending_trigger_order.is_none()
    );
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(runner.state().turn_number > turn);
    assert!(!runner.state().objects[&lyse]
        .keywords
        .contains(&Keyword::DoubleStrike));
    assert!(runner
        .state()
        .spells_cast_this_turn_by_player
        .values()
        .all(|records| records.is_empty()));
    assert_eq!(runner.state().spells_cast_this_game_by_player[&P0].len(), 2);
}

#[test]
#[cfg(feature = "cli")]
fn jev_all_nine_complete_current_export_string_loader() {
    use engine::types::ability::{
        AbilityCondition, CardTypeSetSource, ChoiceType, CounterAdjustment,
        DelayedTriggerCondition, EffectScope, ObjectScope, ParitySource, PerpetualModification,
        PlayerFilter, PtValue, ReturnResultReadSpec, SharedQuality, SharedQualityRelation,
        SubAbilityLink, TapStateChange, TargetReadOrigin,
    };
    use engine::types::card_type::{CardType, CoreType, Supertype};
    use engine::types::counter::CounterMatch;
    use engine::types::mana::ManaColor;
    use engine::types::replacements::ReplacementEvent;
    use std::collections::{BTreeMap, BTreeSet};
    const CIRCADIAN: &str = "Vivid — Seek X cards that each share a color with one or more permanents you control, where X is the number of colors among permanents you control. For each color among permanents you control, those cards perpetually gain \"This spell costs {1} less to cast.\"";
    const BROKEN: &str = "Destroy target nonartifact attacking creature. It can't be regenerated. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness. Sacrifice the token at the beginning of the next end step.";
    const CRATEROUS: &str = "Craterous Stomp deals 3 damage to target creature an opponent controls. Each other creature that player controls becomes a Coward in addition to its other types and gains \"This creature can't block Giants or Warriors.\"";
    const PRESERVE: &str = "At the beginning of your upkeep, each player reveals the top card of their library. If all cards revealed this way are creature cards, put those cards onto the battlefield under their owners' control.";
    const IMMARD: &str = "Whenever Immard, the Stormcleaver enters or attacks, put a charge counter on it or remove one from it. When you remove a counter this way, choose one —\n• Immard deals 4 damage to any target.\n• Immard gains lifelink and indestructible until end of turn.";
    const ASHLING: &str = "As this enchantment enters, choose odd or even. (Zero is even.)\nEach creature with mana value of the chosen quality has haste.\nEach creature without mana value of the chosen quality enters tapped.";
    // allow-full-card-db: the cli-qualified final-input regression exercises the complete production String through the actual browser loader.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    let serialized = std::fs::read_to_string(&path)
        .expect("fresh complete current production export is required; this test cannot skip");
    let raw: Map<String, Value> =
        serde_json::from_str(&serialized).expect("complete raw export map");
    assert!(
        raw.len() > 35_000,
        "production face-scale population required"
    );
    let by_storage: BTreeMap<_, _> = raw
        .iter()
        .map(|(key, entry)| (key.to_lowercase(), entry))
        .collect();
    assert_eq!(
        by_storage.len(),
        raw.len(),
        "no lowercase storage-key collision"
    );
    // The unchanged complete String is the browser's actual production loader input.
    let db = CardDatabase::from_json_str(&serialized).expect("whole production string loads");
    let server_db = CardDatabase::from_export(&path).expect("same complete server export loads");
    let expected_keys: BTreeSet<_> = by_storage.keys().map(String::as_str).collect();
    let loaded_keys: BTreeSet<_> = db.face_iter().map(|(key, _)| key).collect();
    assert_eq!(db.face_iter().count(), expected_keys.len());
    assert_eq!(loaded_keys, expected_keys);
    assert_eq!(
        server_db
            .face_iter()
            .map(|(key, _)| key)
            .collect::<BTreeSet<_>>(),
        expected_keys
    );
    for (key, face) in db.face_iter() {
        let entry = by_storage[key];
        assert_eq!(face.name, entry["name"].as_str().unwrap(), "{key}");
        assert_eq!(
            serde_json::to_value(&face.mana_cost).unwrap(),
            entry["mana_cost"],
            "{key}"
        );
        assert_eq!(
            serde_json::to_value(&face.card_type).unwrap(),
            entry["card_type"],
            "{key}"
        );
        assert_eq!(
            face.oracle_text.as_deref(),
            entry["oracle_text"].as_str(),
            "{key}"
        );
        assert_eq!(
            face.scryfall_oracle_id.as_deref(),
            entry["scryfall_oracle_id"].as_str(),
            "{key}"
        );
    }
    use ManaCostShard::*;
    for (name, oracle, id, generic, shards, types, subtypes, supertypes) in [
        (
            "Sailors' Bane",
            SAILORS,
            "5d22ab00-b454-4407-93d7-65b47f56cbaa",
            7,
            vec![Blue, Blue],
            vec![CoreType::Creature],
            vec!["Dragon", "Turtle"],
            vec![],
        ),
        (
            "Machinist's Dismissal",
            DISMISSAL,
            "2f2a6a57-6919-405b-93df-a1d4c077da0f",
            4,
            vec![Blue],
            vec![CoreType::Instant],
            vec![],
            vec![],
        ),
        (
            "Locket of Yesterdays",
            LOCKET,
            "bfbfa8c1-5b03-40bb-8a4c-aedb768cb023",
            1,
            vec![],
            vec![CoreType::Artifact],
            vec![],
            vec![],
        ),
        (
            "Broken Visage",
            BROKEN,
            "8e1d750e-44a7-47b1-8e0a-5f8b15d48b41",
            4,
            vec![Black],
            vec![CoreType::Instant],
            vec![],
            vec![],
        ),
        (
            "Circadian Struggle",
            CIRCADIAN,
            "82cb6351-58db-462d-a49b-ad365f19f1ec",
            4,
            vec![GreenBlue, GreenBlue],
            vec![CoreType::Instant],
            vec![],
            vec![],
        ),
        (
            "Craterous Stomp",
            CRATEROUS,
            "e0aa676e-2a3a-4ed5-93f9-65e53a8fcb96",
            1,
            vec![Red],
            vec![CoreType::Kindred, CoreType::Instant],
            vec!["Giant"],
            vec![],
        ),
        (
            "Game Preserve",
            PRESERVE,
            "409e0a43-0e85-47ea-9d40-de0860b12952",
            2,
            vec![Green],
            vec![CoreType::Enchantment],
            vec![],
            vec![],
        ),
        (
            "Immard, the Stormcleaver",
            IMMARD,
            "104f65d3-bedb-4dc5-a5dd-8ab9fe796af4",
            1,
            vec![Blue, Red, White],
            vec![CoreType::Creature],
            vec!["Human", "Soldier"],
            vec![Supertype::Legendary],
        ),
        (
            "Ashling's Prerogative",
            ASHLING,
            "b68bc097-482a-4fe5-9fca-df5941f3dc5c",
            1,
            vec![Red],
            vec![CoreType::Enchantment],
            vec![],
            vec![],
        ),
    ] {
        let key = name.to_lowercase();
        let entry = by_storage[key.as_str()];
        let face = db.get_face_by_name(name).unwrap();
        let cost = ManaCost::Cost { generic, shards };
        assert_eq!(face.name, name);
        assert_eq!(face.oracle_text.as_deref(), Some(oracle));
        assert_eq!(entry["oracle_text"].as_str(), Some(oracle));
        assert_eq!(face.scryfall_oracle_id.as_deref(), Some(id));
        assert_eq!(entry["scryfall_oracle_id"].as_str(), Some(id));
        assert_eq!(face.mana_cost, cost);
        assert_eq!(entry["mana_cost"], serde_json::to_value(cost).unwrap());
        let expected_type = CardType {
            core_types: types,
            subtypes: subtypes.into_iter().map(String::from).collect(),
            supertypes,
        };
        assert_eq!(face.card_type, expected_type);
        assert_eq!(
            entry["card_type"],
            serde_json::to_value(expected_type).unwrap()
        );
        assert!(
            face.parse_warnings.is_empty(),
            "{name}: {:?}",
            face.parse_warnings
        );
        assert!(
            engine::game::coverage::card_face_gaps(face).is_empty(),
            "{name}: {:?}",
            engine::game::coverage::card_face_gaps(face)
        );
    }
    let face = |name| db.get_face_by_name(name).unwrap();
    let typed =
        |kind, properties| TargetFilter::Typed(TypedFilter::new(kind).properties(properties));
    let owned = FilterProp::Owned {
        controller: ControllerRef::You,
    };
    let sailors_count = QuantityRef::ObjectCount {
        filter: TargetFilter::And {
            filters: vec![
                typed(TypeFilter::Card, vec![owned.clone()]),
                TargetFilter::Or {
                    filters: vec![
                        typed(
                            TypeFilter::Card,
                            vec![FilterProp::InZone { zone: Zone::Exile }],
                        ),
                        typed(
                            TypeFilter::Card,
                            vec![
                                FilterProp::InZone {
                                    zone: Zone::Graveyard,
                                },
                                owned.clone(),
                            ],
                        ),
                    ],
                },
                TargetFilter::Or {
                    filters: vec![
                        typed(TypeFilter::Instant, vec![]),
                        typed(TypeFilter::Sorcery, vec![]),
                        typed(TypeFilter::Card, vec![FilterProp::HasAdventure]),
                    ],
                },
            ],
        },
    };
    let sailors = face("Sailors' Bane");
    assert_eq!(sailors.static_abilities.len(), 1);
    assert_eq!(
        sailors.static_abilities[0].affected,
        Some(TargetFilter::SelfRef)
    );
    assert!(
        matches!(&sailors.static_abilities[0].mode, StaticMode::ModifyCost { mode: CostModifyMode::Reduce, amount, dynamic_count: Some(count), .. } if *amount == ManaCost::generic(1) && *count == sailors_count)
    );
    assert!(sailors
        .keywords
        .contains(&Keyword::Ward(engine::types::keywords::WardCost::Mana(
            ManaCost::generic(4)
        ))));
    let dismissal = face("Machinist's Dismissal");
    assert_eq!(dismissal.static_abilities.len(), 1);
    let population = TargetFilter::Or {
        filters: vec![
            TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Battlefield,
                    }]),
            ),
            typed(
                TypeFilter::Artifact,
                vec![FilterProp::InZone { zone: Zone::Hand }, owned.clone()],
            ),
        ],
    };
    assert!(
        matches!(&dismissal.static_abilities[0].mode, StaticMode::ModifyCost { mode: CostModifyMode::Reduce, amount, dynamic_count: Some(QuantityRef::ObjectCount { filter }), .. } if *amount == ManaCost::generic(1) && *filter == population)
    );
    assert_eq!(dismissal.abilities.len(), 1);
    assert!(
        matches!(dismissal.abilities[0].effect.as_ref(), Effect::Counter { target: TargetFilter::Typed(tf), source_rider: None, countered_spell_zone: None } if tf.type_filters.contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))) && tf.properties.contains(&FilterProp::InZone { zone: Zone::Stack }))
    );
    let locket = face("Locket of Yesterdays");
    let locket_count = QuantityRef::ObjectCount {
        filter: TargetFilter::And {
            filters: vec![
                typed(TypeFilter::Card, vec![]),
                typed(
                    TypeFilter::Card,
                    vec![FilterProp::SharesQuality {
                        quality: SharedQuality::Name,
                        reference: Some(Box::new(TargetFilter::CastingSpell)),
                        relation: SharedQualityRelation::Shares,
                    }],
                ),
                typed(
                    TypeFilter::Card,
                    vec![
                        FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                        owned,
                    ],
                ),
            ],
        },
    };
    assert_eq!(locket.static_abilities.len(), 1);
    assert_eq!(
        locket.static_abilities[0].affected,
        Some(TargetFilter::Typed(
            TypedFilter::card().controller(ControllerRef::You)
        ))
    );
    assert!(
        matches!(&locket.static_abilities[0].mode, StaticMode::ModifyCost { mode: CostModifyMode::Reduce, amount, dynamic_count: Some(count), spell_filter: None, .. } if *amount == ManaCost::generic(1) && *count == locket_count)
    );
    let broken = face("Broken Visage");
    assert!(broken.static_abilities.is_empty());
    assert_eq!(broken.abilities.len(), 1);
    let destroy = &broken.abilities[0];
    assert!(
        matches!(destroy.effect.as_ref(), Effect::Destroy { target: TargetFilter::Typed(tf), cant_regenerate: true, .. } if tf.type_filters.contains(&TypeFilter::Creature) && tf.type_filters.contains(&TypeFilter::Non(Box::new(TypeFilter::Artifact))) && tf.properties.iter().any(|prop| matches!(prop, FilterProp::Attacking { .. })))
    );
    let token = destroy.sub_ability.as_deref().unwrap();
    assert_eq!(token.target_reads, TargetReadOrigin::ParentAnnouncement);
    assert!(
        matches!(token.effect.as_ref(), Effect::Token { power, toughness, colors, types, count: QuantityExpr::Fixed { value: 1 }, .. } if *power == PtValue::Quantity(QuantityExpr::Ref { qty: QuantityRef::Power { scope: ObjectScope::Target } }) && *toughness == PtValue::Quantity(QuantityExpr::Ref { qty: QuantityRef::Toughness { scope: ObjectScope::Target } }) && colors == &[ManaColor::Black] && types.contains(&"Spirit".into()))
    );
    let delayed = token.sub_ability.as_deref().unwrap();
    assert_eq!(delayed.sub_link, SubAbilityLink::SequentialSibling);
    assert!(
        matches!(delayed.effect.as_ref(), Effect::CreateDelayedTrigger { condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End }, effect, uses_tracked_set: true } if matches!(effect.effect.as_ref(), Effect::Sacrifice { .. }))
    );
    // The ordinary Token instruction declares the creation operation at runtime;
    // its resident TokenCreationFrameId publishes the completed incarnation set
    // through forwarded_result_context to this immediate tracked-set consumer.
    // It is not the distinct sought/revealed ReturnResultId AST family. The U9
    // broken_visage_* tests assert actual captured births, cleanup, foreign later
    // tokens, same-ID returned members and current/legacy restored authorities.
    assert!(token.declares_return_result.is_none());
    assert!(delayed.reads_return_result.is_none());
    assert!(delayed.sub_ability.is_none());
    let circadian = face("Circadian Struggle");
    assert!(circadian.static_abilities.is_empty());
    assert_eq!(circadian.abilities.len(), 1);
    let seek = &circadian.abilities[0];
    let population =
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent).controller(ControllerRef::You));
    let colors = QuantityExpr::Ref {
        qty: QuantityRef::DistinctColorsAmong {
            source: CardTypeSetSource::Objects {
                filter: population.clone(),
            },
        },
    };
    assert!(
        matches!(seek.effect.as_ref(), Effect::Seek { filter: TargetFilter::Typed(tf), count, destination: Zone::Hand, from_top: None, .. } if *count == colors && tf.properties.iter().any(|prop| matches!(prop, FilterProp::SharesQuality { quality: SharedQuality::Color, reference: Some(reference), relation: SharedQualityRelation::Shares } if **reference == population)))
    );
    let grant = seek.sub_ability.as_deref().unwrap();
    assert_eq!(grant.repeat_for, Some(colors));
    assert!(seek.declares_return_result.is_some());
    assert_eq!(
        grant.reads_return_result.as_ref().map(|(id, _)| *id),
        seek.declares_return_result
    );
    assert!(
        matches!(grant.effect.as_ref(), Effect::ApplyPerpetual { target: TargetFilter::None, modification: PerpetualModification::ModifyCost { mode: CostModifyMode::Reduce, amount } } if *amount == ManaCost::generic(1))
    );
    let stomp = face("Craterous Stomp");
    assert_eq!(stomp.abilities.len(), 1);
    let damage = &stomp.abilities[0];
    assert!(
        matches!(damage.effect.as_ref(), Effect::DealDamage { amount: QuantityExpr::Fixed { value: 3 }, target: TargetFilter::Typed(tf), .. } if tf.controller == Some(ControllerRef::Opponent) && tf.type_filters == [TypeFilter::Creature])
    );
    let rest = damage.sub_ability.as_deref().unwrap();
    assert_eq!(rest.target_reads, TargetReadOrigin::ParentAnnouncement);
    let Effect::GenericEffect {
        static_abilities,
        duration: Some(Duration::Permanent),
        ..
    } = rest.effect.as_ref()
    else {
        panic!("complete permanent Coward grant")
    };
    assert!(static_abilities.iter().any(|def| matches!(&def.affected, Some(TargetFilter::Typed(tf)) if tf.controller == Some(ControllerRef::ParentTargetController) && tf.properties.contains(&FilterProp::DistinctFrom { reference: Box::new(TargetFilter::ParentTarget) }))));
    assert!(static_abilities.iter().flat_map(|def| &def.modifications).any(|modification| matches!(modification, ContinuousModification::AddSubtype { subtype } if subtype == "Coward")));
    let restriction = StaticMode::BlockRestriction {
        filter: TargetFilter::Not {
            filter: Box::new(TargetFilter::Or {
                filters: vec![
                    typed(TypeFilter::Subtype("Giant".into()), vec![]),
                    typed(TypeFilter::Subtype("Warrior".into()), vec![]),
                ],
            }),
        },
    };
    assert!(
        static_abilities
            .iter()
            .flat_map(|def| &def.modifications)
            .any(|modification| match modification {
                ContinuousModification::GrantStaticAbility { definition } =>
                    definition.mode == restriction
                        && definition.affected == Some(TargetFilter::SelfRef),
                ContinuousModification::AddStaticMode { mode } => *mode == restriction,
                _ => false,
            })
    );
    let preserve = face("Game Preserve");
    assert_eq!(preserve.triggers.len(), 1);
    let reveal = preserve.triggers[0].execute.as_deref().unwrap();
    assert_eq!(reveal.player_scope, Some(PlayerFilter::All));
    assert!(matches!(
        reveal.effect.as_ref(),
        Effect::RevealTop { count: 1, .. }
    ));
    let owner_entry = reveal.sub_ability.as_deref().unwrap();
    assert!(reveal.declares_return_result.is_some());
    assert_eq!(
        owner_entry.reads_return_result.as_ref().map(|(id, _)| *id),
        reveal.declares_return_result
    );
    assert_eq!(
        owner_entry
            .reads_return_result
            .as_ref()
            .map(|(_, spec)| spec),
        Some(&ReturnResultReadSpec {
            noun: TargetFilter::Typed(TypedFilter::creature()),
            destination: Zone::Library,
            recipient: ControllerRef::ParentTargetOwner
        })
    );
    assert!(matches!(
        owner_entry.effect.as_ref(),
        Effect::ChangeZone {
            target: TargetFilter::None,
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            enters_under: None,
            ..
        }
    ));
    assert!(
        matches!(&owner_entry.condition, Some(AbilityCondition::QuantityCheck { lhs: QuantityExpr::Ref { qty: QuantityRef::FilteredTrackedSetSize { filter, caused_by: None } }, comparator: engine::types::ability::Comparator::EQ, rhs: QuantityExpr::Ref { qty: QuantityRef::TrackedSetSize } }) if **filter == TargetFilter::Typed(TypedFilter::creature()))
    );
    let immard = face("Immard, the Stormcleaver");
    assert_eq!(immard.triggers.len(), 2);
    assert!(immard
        .triggers
        .iter()
        .any(|trigger| trigger.mode == engine::types::triggers::TriggerMode::Attacks));
    assert!(immard
        .triggers
        .iter()
        .any(|trigger| trigger.mode == engine::types::triggers::TriggerMode::ChangesZone));
    for trigger in &immard.triggers {
        let parent = trigger.execute.as_deref().unwrap();
        assert!(
            matches!(parent.effect.as_ref(), Effect::ChooseCounterAdjustment { adjustment: CounterAdjustment::AddOrRemove, counters: CounterMatch::OfType(CounterType::Generic(kind)), count: QuantityExpr::Fixed { value: 1 }, target: TargetFilter::SelfRef } if kind == "charge")
        );
        assert!(!parent.optional);
        let reflexive = parent.sub_ability.as_deref().unwrap();
        let condition = reflexive.condition.as_ref().unwrap();
        assert!(condition.has_when_you_do_marker());
        let mut guard = Some(condition.clone());
        assert!(AbilityCondition::take_when_you_do_marker(&mut guard));
        let Some(AbilityCondition::CounterRemovedThisWay { result, counters }) = guard else {
            panic!("complete actual-removal guard")
        };
        assert_eq!(parent.declares_return_result, Some(result));
        assert_eq!(counters, CounterMatch::Any);
        assert_eq!(reflexive.mode_abilities.len(), 2);
        assert!(matches!(
            reflexive.mode_abilities[0].effect.as_ref(),
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::Any,
                damage_source: None,
                ..
            }
        ));
        let keywords = &reflexive.mode_abilities[1];
        assert_eq!(keywords.duration, Some(Duration::UntilEndOfTurn));
        let Effect::GenericEffect {
            static_abilities,
            duration: Some(Duration::UntilEndOfTurn),
            ..
        } = keywords.effect.as_ref()
        else {
            panic!("complete self keyword mode")
        };
        assert_eq!(static_abilities.len(), 1);
        assert_eq!(static_abilities[0].affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            static_abilities[0].modifications,
            vec![
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Lifelink
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Indestructible
                }
            ]
        );
    }
    let ashling = face("Ashling's Prerogative");
    assert_eq!(ashling.replacements.len(), 2);
    assert_eq!(ashling.static_abilities.len(), 1);
    assert!(ashling
        .replacements
        .iter()
        .any(|def| def.valid_card == Some(TargetFilter::SelfRef)
            && def.execute.as_deref().is_some_and(|ability| matches!(
                ability.effect.as_ref(),
                Effect::Choose {
                    choice_type: ChoiceType::OddOrEven,
                    persist: true,
                    ..
                }
            ))));
    let parity = FilterProp::ManaValueParity {
        parity: ParitySource::SourceChosen,
    };
    assert_eq!(
        ashling.static_abilities[0].affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![parity.clone()])
        ))
    );
    assert_eq!(
        ashling.static_abilities[0].modifications,
        vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Haste
        }]
    );
    let external = ashling
        .replacements
        .iter()
        .find(|def| def.valid_card != Some(TargetFilter::SelfRef))
        .unwrap();
    assert_eq!(external.event, ReplacementEvent::Moved);
    assert_eq!(external.destination_zone, Some(Zone::Battlefield));
    assert_eq!(
        external.mode,
        engine::types::ability::ReplacementMode::Mandatory
    );
    assert_eq!(
        external.valid_card,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::Not {
                prop: Box::new(parity)
            }]
        )))
    );
    assert!(matches!(
        external.execute.as_deref().unwrap().effect.as_ref(),
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap
        }
    ));
}
const BEHOLD: &str = "Scry 2, then draw two cards.\nForetell {1}{U} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";
const DREAM_DEVOURER: &str = "Each nonland card in your hand without foretell has foretell. Its foretell cost is equal to its mana cost reduced by {2}. (During your turn, you may pay {2} and exile it from your hand face down. Cast it on a later turn for its foretell cost.)\nWhenever you foretell a card, this creature gets +2/+0 until end of turn.";
const SEA_GATE_RESTORATION: &str = "Draw cards equal to the number of cards in your hand plus one. You have no maximum hand size for the rest of the game.";
const SEA_GATE_REBORN: &str =
    "As this land enters, you may pay 3 life. If you don't, it enters tapped.\n{T}: Add {U}.";

fn sea_gate_reborn_face() -> BackFaceData {
    let parsed = engine::parser::parse_oracle_text(
        SEA_GATE_REBORN,
        "Sea Gate, Reborn",
        &[],
        &["Land".into()],
        &[],
    );
    assert!(parsed.parse_warnings.is_empty());
    BackFaceData {
        name: "Sea Gate, Reborn".into(),
        layout_kind: Some(LayoutKind::Modal),
        mana_cost: ManaCost::NoCost,
        card_types: engine::types::card_type::CardType {
            core_types: vec![engine::types::card_type::CoreType::Land],
            ..Default::default()
        },
        abilities: parsed.abilities,
        trigger_definitions: parsed.triggers.into(),
        static_definitions: parsed.statics.into(),
        replacement_definitions: parsed.replacements.into(),
        ..Default::default()
    }
}

fn synthetic_name_face(
    name: &str,
    layout: LayoutKind,
    cost: ManaCost,
    oracle: &str,
) -> BackFaceData {
    let parsed = engine::parser::parse_oracle_text(oracle, name, &[], &["Instant".into()], &[]);
    assert!(parsed.parse_warnings.is_empty());
    assert!(parsed
        .abilities
        .iter()
        .all(|ability| !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. })));
    BackFaceData {
        name: name.into(),
        layout_kind: Some(layout),
        mana_cost: cost,
        card_types: engine::types::card_type::CardType {
            core_types: vec![engine::types::card_type::CoreType::Instant],
            ..Default::default()
        },
        abilities: parsed.abilities,
        ..Default::default()
    }
}

#[test]
fn locket_selected_face_names() {
    use engine::types::game_state::{CastingVariant, CastingVariantFace};
    // Synthetic split witnesses exercise selected half, two off-stack names,
    // and one contribution from a card sharing both fused names.
    for (variant, face, paid, gain, name) in [
        (
            CastingVariant::Normal,
            CastingVariantFace::Left,
            5,
            1,
            "Synthetic Alpha",
        ),
        (
            CastingVariant::Normal,
            CastingVariantFace::Right,
            1,
            2,
            "Synthetic Beta",
        ),
        (
            CastingVariant::Fuse,
            CastingVariantFace::Left,
            7,
            3,
            "Synthetic Alpha",
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, paid);
        add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic Alpha", true, "You gain 1 life.")
            .with_mana_cost(blue_cost(5, 1))
            .with_keyword(Keyword::Fuse)
            .id();
        let both = scenario
            .add_spell_to_graveyard(P0, "Synthetic Beta", true)
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic Beta", true);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&spell)
            .unwrap()
            .back_face = Some(synthetic_name_face(
            "Synthetic Beta",
            LayoutKind::Split,
            blue_cost(2, 1),
            "You gain 2 life.",
        ));
        runner.state_mut().objects.get_mut(&both).unwrap().back_face = Some(synthetic_name_face(
            "Synthetic Alpha",
            LayoutKind::Split,
            blue_cost(5, 1),
            "You gain 1 life.",
        ));
        assert_eq!(runner.state().objects[&both].effective_names().len(), 2);
        let committed = runner
            .cast(spell)
            .casting_variant_face(variant, face)
            .commit();
        assert_eq!(committed.state().objects[&spell].name, name);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        assert_eq!(
            committed.selected_casting_variant().unwrap().variant,
            variant
        );
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, gain);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
}

#[test]
fn locket_adventure_and_modal_names() {
    use engine::types::game_state::CastingVariant;
    for creature_face in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, if creature_face { 3 } else { 2 });
        add_locket(&mut scenario);
        let spell = scenario
            .add_creature_to_hand(P0, "Synthetic adventurer", 2, 2)
            .with_mana_cost(blue_cost(3, 1))
            .id();
        let ordinary = scenario
            .add_creature_to_graveyard(P0, "Synthetic adventurer", 2, 2)
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic Journey", true);
        let mut runner = scenario.build();
        for id in [spell, ordinary] {
            runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(synthetic_name_face(
                "Synthetic Journey",
                LayoutKind::Adventure,
                blue_cost(2, 1),
                "You gain 2 life.",
            ));
        }
        let committed = runner.cast(spell).adventure_face(creature_face).commit();
        assert_eq!(
            committed.state().objects[&spell].name,
            if creature_face {
                "Synthetic adventurer"
            } else {
                "Synthetic Journey"
            }
        );
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(
            &[spell],
            if creature_face {
                Zone::Battlefield
            } else {
                Zone::Exile
            },
        );
        outcome.assert_life_delta(P0, if creature_face { 0 } else { 2 });
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
    for back in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, if back { 2 } else { 4 });
        add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic modal front", true, "You gain 1 life.")
            .with_mana_cost(blue_cost(3, 1))
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic modal back", true);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&spell)
            .unwrap()
            .back_face = Some(synthetic_name_face(
            "Synthetic modal back",
            LayoutKind::Modal,
            blue_cost(2, 1),
            "You gain 2 life.",
        ));
        let committed = runner.cast(spell).modal_back_face(back).commit();
        assert_eq!(
            committed.state().objects[&spell].name,
            if back {
                "Synthetic modal back"
            } else {
                "Synthetic modal front"
            }
        );
        assert_eq!(
            committed.state().objects[&spell].spell_names_for(CastingVariant::Normal),
            vec![if back {
                "synthetic modal back"
            } else {
                "synthetic modal front"
            }]
        );
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, if back { 2 } else { 1 });
    }
}

#[test]
fn locket_unnamed_and_actual_face_down_nonmatch() {
    use engine::types::game_state::CastingVariant;
    for name in ["Synthetic named creature", ""] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, if name.is_empty() { 4 } else { 3 });
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_creature_to_hand(P0, name, 2, 2)
            .with_mana_cost(blue_cost(3, 1))
            .id();
        scenario.add_creature_to_graveyard(P0, name, 2, 2);
        let mut runner = scenario.build();
        assert_eq!(
            locket_count(&runner, source, spell),
            if name.is_empty() { 0 } else { 1 }
        );
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
    for face_down in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, 3);
        add_locket(&mut scenario);
        let spell = scenario
            .add_creature_to_hand(P0, "Synthetic morph creature", 4, 4)
            .with_keyword(Keyword::Morph(MorphCost::Mana(ManaCost::generic(2))))
            .with_mana_cost(ManaCost::generic(4))
            .id();
        scenario.add_creature_to_graveyard(P0, "Synthetic morph creature", 4, 4);
        scenario.add_creature_to_graveyard(P0, "", 2, 2);
        let mut runner = scenario.build();
        let variant = if face_down {
            CastingVariant::FaceDown
        } else {
            CastingVariant::Normal
        };
        let committed = runner.cast(spell).casting_variant(variant).commit();
        assert_eq!(
            committed.state().objects[&spell]
                .spell_names_for(variant)
                .is_empty(),
            face_down
        );
        assert_eq!(
            committed.state().players[0].mana_pool.total(),
            0,
            "named {{4}}-1 and unnamed face-down {{3}} are separate payable branches"
        );
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        assert_eq!(outcome.state().objects[&spell].face_down, face_down);
        assert_eq!(
            outcome.state().objects[&spell].power,
            Some(if face_down { 2 } else { 4 })
        );
    }
}

#[test]
fn locket_reference_serde_and_loaded_cast() {
    use engine::types::ability::StaticDefinition;
    // Literal legacy tags are compatibility fixtures, not derived expected encodings.
    for (json, filter) in [
        (r#"{"type":"SelfRef"}"#, TargetFilter::SelfRef),
        (r#"{"type":"ParentTarget"}"#, TargetFilter::ParentTarget),
        (r#"{"type":"StackSpell"}"#, TargetFilter::StackSpell),
        (r#"{"type":"CastingSpell"}"#, TargetFilter::CastingSpell),
    ] {
        let loaded: TargetFilter = serde_json::from_str(json).unwrap();
        assert_eq!(loaded, filter);
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::from_str::<Value>(json).unwrap()
        );
    }
    let definition = locket_definition();
    let encoded = serde_json::to_string(&definition).unwrap();
    let loaded: StaticDefinition = serde_json::from_str(&encoded).unwrap();
    assert_eq!(loaded, definition);
    let (runner, source, spell) = locket_board(2, 4);
    let state_json = serde_json::to_string(runner.state()).unwrap();
    let state: GameState = serde_json::from_str(&state_json).unwrap();
    assert_eq!(state.objects[&source].static_definitions[0], loaded);
    let mut runner = GameRunner::from_state(state);
    assert_eq!(locket_count(&runner, source, spell), 2);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_life_delta(P0, 1);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

#[test]
fn locket_export_string_loader_compatibility() {
    let raw =
        shared_card_export_json().expect("fresh full export required at the run-end boundary");
    let entry = raw.get("locket of yesterdays").expect("exported Locket");
    assert_eq!(
        entry.get("oracle_text").and_then(Value::as_str),
        Some(LOCKET)
    );
    let mut entries = Map::new();
    entries.insert("locket of yesterdays".to_string(), entry.clone());
    let db = CardDatabase::from_json_str(&to_string(&entries).unwrap()).unwrap();
    let face = db.get_face_by_name("Locket of Yesterdays").unwrap();
    assert_eq!(face.static_abilities.as_slice(), &[locket_definition()]);
    let (mut runner, source, spell) = locket_board(2, 4);
    let object = runner.state_mut().objects.get_mut(&source).unwrap();
    object.static_definitions = face.static_abilities.clone().into();
    object.base_static_definitions = std::sync::Arc::new(face.static_abilities.clone());
    let outcome = runner.cast(spell).resolve();
    outcome.assert_life_delta(P0, 1);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

fn locket_definition() -> engine::types::ability::StaticDefinition {
    use engine::types::ability::{SharedQuality, SharedQualityRelation};
    let parsed = engine::parser::parse_oracle_text(
        LOCKET,
        "Locket of Yesterdays",
        &[],
        &["Artifact".into()],
        &[],
    );
    assert!(parsed.parse_warnings.is_empty());
    assert!(parsed.abilities.is_empty());
    assert_eq!(parsed.statics.len(), 1);
    let definition = parsed.statics[0].clone();
    assert_eq!(
        definition.affected,
        Some(TargetFilter::Typed(
            TypedFilter::card().controller(ControllerRef::You)
        ))
    );
    let StaticMode::ModifyCost {
        mode,
        amount,
        dynamic_count: Some(quantity),
        spell_filter,
        ..
    } = &definition.mode
    else {
        panic!("complete Locket modifier")
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(*amount, ManaCost::generic(1));
    assert!(spell_filter.is_none());
    assert_eq!(
        *quantity,
        QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::card()),
                    TargetFilter::Typed(TypedFilter::card().properties(vec![
                        FilterProp::SharesQuality {
                            quality: SharedQuality::Name,
                            reference: Some(Box::new(TargetFilter::CastingSpell)),
                            relation: SharedQualityRelation::Shares,
                        }
                    ])),
                    TargetFilter::Typed(TypedFilter::card().properties(vec![
                        FilterProp::InZone {
                            zone: Zone::Graveyard
                        },
                        FilterProp::Owned {
                            controller: ControllerRef::You
                        }
                    ])),
                ]
            }
        }
    );
    definition
}

fn add_locket(scenario: &mut GameScenario) -> ObjectId {
    let expected = locket_definition();
    let id = scenario
        .add_artifact_from_oracle(P0, "Locket of Yesterdays", LOCKET)
        .id();
    // The fixture itself is parsed from the complete printed Oracle, not injected AST.
    assert!(matches!(expected.mode, StaticMode::ModifyCost { .. }));
    id
}

fn locket_count(runner: &GameRunner, source: ObjectId, spell: ObjectId) -> i32 {
    let StaticMode::ModifyCost {
        dynamic_count: Some(qty),
        ..
    } = locket_definition().mode
    else {
        unreachable!()
    };
    engine::game::quantity::resolve_quantity_with_spell(
        runner.state(),
        &QuantityExpr::Ref { qty },
        P0,
        source,
        spell,
    )
}

fn locket_board(matches: usize, pool: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, pool);
    let source = add_locket(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic named instant", true, "You gain 1 life.")
        .with_mana_cost(blue_cost(5, 1))
        .id();
    for _ in 0..matches {
        scenario.add_spell_to_graveyard(P0, "Synthetic named instant", true);
    }
    for name in [
        "Locket of Yesterdays",
        "Different name one",
        "Different name two",
    ] {
        scenario.add_spell_to_graveyard(P0, name, true);
    }
    scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic named instant", true, "You gain 1 life.")
        .with_mana_cost(blue_cost(5, 1));
    scenario.add_spell_to_exile(P0, "Synthetic named instant", true);
    scenario.add_creature(P0, "Synthetic named instant", 1, 1);
    scenario.add_spell_to_graveyard(P1, "Synthetic named instant", true);
    (scenario.build(), source, spell)
}

#[test]
fn locket_name_population_cost() {
    // CR 118.7a: only generic mana is reduced, with a zero generic floor.
    for (matches, pool) in [(2, 4), (0, 6), (8, 1)] {
        let (mut runner, source, spell) = locket_board(matches, pool);
        assert_eq!(locket_count(&runner, source, spell), matches as i32);
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
            Some(blue_cost(5u32.saturating_sub(matches as u32), 1))
        );
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, 1);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
    let (mut runner, source, spell) = locket_board(2, 3);
    assert_eq!(locket_count(&runner, source, spell), 2);
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "one short after the payable sibling"
    );
    let (mut runner, source, spell) = locket_board(2, 4);
    assert_eq!(locket_count(&runner, source, spell), 2);
    runner.state_mut().players[0].mana_pool = (0..4)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect();
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "generic reduction cannot pay blue"
    );
}

#[test]
fn locket_independent_cast_authorities() {
    use engine::types::ability::{CardPlayMode, StaticDefinition};
    use engine::types::statics::{CastFrequency, GraveyardPermissionPool};
    for (sources, opponent_control, foreign_spell) in
        [(2, false, false), (1, true, false), (1, false, true)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let payable = if opponent_control { 6 } else { 6 - sources * 2 };
        mana(&mut scenario, payable);
        let source = add_locket(&mut scenario);
        if sources == 2 {
            add_locket(&mut scenario);
        }
        // Foreign ownership of a functioning source does not change "your".
        let spell = if foreign_spell {
            scenario
                .add_spell_to_graveyard(P1, "Authority instant", true)
                .from_oracle_text("You gain 1 life.")
                .with_mana_cost(blue_cost(5, 1))
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Authority instant", true, "You gain 1 life.")
                .with_mana_cost(blue_cost(5, 1))
                .id()
        };
        for _ in 0..2 {
            scenario.add_spell_to_graveyard(P0, "Authority instant", true);
        }
        scenario.add_spell_to_graveyard(P0, "Locket of Yesterdays", true);
        scenario.add_spell_to_graveyard(P1, "Authority instant", true);
        if foreign_spell {
            scenario
                .add_enchantment_from_oracle(P0, "Synthetic any-graveyard name permission", "")
                .with_static_definition(
                    StaticDefinition::new(StaticMode::GraveyardCastPermission {
                        frequency: CastFrequency::Unlimited,
                        play_mode: CardPlayMode::Cast,
                        graveyard_destination_replacement: None,
                        extra_cost: None,
                        enters_with_counter: None,
                        required_cast_keyword: None,
                        pool: GraveyardPermissionPool::AnyGraveyard,
                    })
                    .affected(TargetFilter::Typed(TypedFilter::card())),
                );
        }
        let mut runner = scenario.build();
        let object = runner.state_mut().objects.get_mut(&source).unwrap();
        object.owner = P1;
        if opponent_control {
            object.controller = P1;
            object.base_controller = Some(P1);
        }
        assert_eq!(locket_count(&runner, source, spell), 2);
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, 1);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert_eq!(outcome.state().objects[&source].owner, P1);
    }
    let (mut runner, source, spell) = locket_board(2, 4);
    runner.cast(spell).resolve().assert_life_delta(P0, 1);
    let next = runner.state().players[0]
        .hand
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Synthetic named instant")
        .unwrap();
    let source_object = runner.state_mut().objects.get_mut(&source).unwrap();
    source_object.controller = P1;
    source_object.base_controller = Some(P1);
    runner.state_mut().players[0].mana_pool = (0..6)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect();
    let outcome = runner.cast(next).resolve();
    outcome.assert_life_delta(P0, 1);
    outcome.assert_zone(&[next], Zone::Graveyard);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the subsequent cast pays full price after source control changes"
    );
}

#[test]
fn locket_no_stack_or_target_fallback() {
    use engine::game::filter::{matches_target_filter, CastSpellContext, FilterContext};
    use engine::types::ability::{SharedQuality, SharedQualityRelation};
    use engine::types::game_state::CastingVariant;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 9);
    let source = add_locket(&mut scenario);
    let unrelated = scenario
        .add_spell_to_hand_from_oracle(P0, "Unrelated stack name", true, "You gain 2 life.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let target = scenario
        .add_creature(P1, "Unrelated target name", 2, 2)
        .id();
    let first = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "First priced instant",
            true,
            "Target creature gets +1/+1 until end of turn.",
        )
        .with_mana_cost(blue_cost(5, 1))
        .id();
    let next = scenario
        .add_spell_to_hand_from_oracle(P0, "Second priced instant", true, "You gain 1 life.")
        .with_mana_cost(blue_cost(5, 1))
        .id();
    let member = scenario
        .add_spell_to_graveyard(P0, "First priced instant", true)
        .id();
    scenario.add_spell_to_graveyard(P0, "First priced instant", true);
    scenario.add_spell_to_graveyard(P0, "Second priced instant", true);
    for name in [
        "Unrelated stack name",
        "Unrelated target name",
        "Locket of Yesterdays",
    ] {
        for _ in 0..3 {
            scenario.add_spell_to_graveyard(P0, name, true);
        }
    }
    let mut runner = scenario.build();
    runner.cast(unrelated).commit();
    assert_eq!(locket_count(&runner, source, first), 2);
    let outcome = runner.cast(first).target_object(target).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 5);
    assert_eq!(outcome.state().objects[&target].power, Some(3));
    outcome.assert_life_delta(P0, 2);
    assert_eq!(locket_count(&runner, source, next), 1);
    let outcome = runner.cast(next).resolve();
    outcome.assert_life_delta(P0, 1);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    // No explicit cast context: the same positive board cannot borrow a source,
    // chosen target, stack event, or LKI authority, even under inversion.
    let StaticMode::ModifyCost {
        dynamic_count: Some(qty),
        ..
    } = locket_definition().mode
    else {
        unreachable!()
    };
    assert_eq!(
        resolve_quantity(runner.state(), &QuantityExpr::Ref { qty }, P0, source),
        0
    );
    for relation in [
        SharedQualityRelation::Shares,
        SharedQualityRelation::DoesNotShare,
    ] {
        let filter =
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::Name,
                    reference: Some(Box::new(TargetFilter::CastingSpell)),
                    relation,
                }]),
            );
        let absent = FilterContext::from_source_with_controller(source, P0);
        assert!(!matches_target_filter(
            runner.state(),
            member,
            &filter,
            &absent
        ));
        let missing = absent.with_casting_spell(CastSpellContext {
            object_id: ObjectId(u64::MAX),
            casting_variant: CastingVariant::Normal,
        });
        assert!(!matches_target_filter(
            runner.state(),
            member,
            &filter,
            &missing
        ));
        let bound = absent.with_casting_spell(CastSpellContext {
            object_id: first,
            casting_variant: CastingVariant::Normal,
        });
        assert_eq!(
            matches_target_filter(runner.state(), member, &filter, &bound),
            relation == SharedQualityRelation::Shares
        );
    }
}

#[test]
fn locket_graveyard_announcement_and_lock() {
    use engine::types::ability::{CardPlayMode, StaticDefinition};
    use engine::types::statics::{CastFrequency, GraveyardPermissionPool};
    for cancel in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, 1);
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_graveyard(P0, "Synthetic locked instant", true)
            .with_mana_cost(blue_cost(5, 1))
            .from_oracle_text("You gain 1 life.")
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic locked instant", true);
        let mana_source = scenario
            .add_artifact_from_oracle(
                P0,
                "Synthetic locked instant",
                "Sacrifice this artifact: Add {C}{C}{C}{C}.",
            )
            .id();
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic graveyard casting", "")
            .with_static_definition(
                StaticDefinition::new(StaticMode::GraveyardCastPermission {
                    frequency: CastFrequency::Unlimited,
                    play_mode: CardPlayMode::Cast,
                    graveyard_destination_replacement: None,
                    extra_cost: None,
                    enters_with_counter: None,
                    required_cast_keyword: None,
                    pool: GraveyardPermissionPool::OwnGraveyard,
                })
                .affected(TargetFilter::Typed(TypedFilter::card())),
            );
        let mut runner = scenario.build();
        assert_eq!(
            locket_count(&runner, source, spell),
            2,
            "in-origin preview includes the unannounced card"
        );
        begin_manual(&mut runner, spell);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        assert_eq!(locket_count(&runner, source, spell), 1);
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            blue_cost(4, 1)
        );
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: mana_source,
                ability_index: 0,
            })
            .unwrap();
        assert_eq!(runner.state().objects[&mana_source].zone, Zone::Graveyard);
        assert_eq!(locket_count(&runner, source, spell), 2);
        // CR 601.2f: the newly arrived matching card cannot change the locked total.
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            blue_cost(4, 1)
        );
        if cancel {
            runner.act(GameAction::CancelCast).unwrap();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert!(runner.state().objects[&spell].spell_announcement.is_none());
            assert!(!runner.state().stack.iter().any(|entry| entry.id == spell));
            assert_eq!(locket_count(&runner, source, spell), 3);
            runner.state_mut().players[0].mana_pool = (0..4)
                .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                .collect();
            let outcome = runner.cast(spell).resolve();
            outcome.assert_zone(&[spell], Zone::Graveyard);
            outcome.assert_life_delta(P0, 1);
            assert_eq!(
                outcome.mana_pool_total(P0),
                0,
                "retry binds the new two-card population"
            );
        } else {
            runner.act(GameAction::PassPriority).unwrap();
            assert!(runner.state().pending_cast.is_none());
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(runner.state().objects[&spell].mana_spent_to_cast_amount, 5);
            let before = runner.state().players[0].life;
            runner.resolve_top();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_eq!(runner.state().players[0].life, before + 1);
        }
    }
}

#[test]
fn cast_name_population_modifier_sources() {
    for source_kind in ["self", "battlefield", "temporary"] {
        for matching in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            mana(&mut scenario, if matching { 4 } else { 6 });
            let source = if source_kind == "battlefield" {
                scenario
                    .add_artifact_from_oracle(P0, "Synthetic independent dynamic source", "")
                    .with_static_definition(locket_definition())
                    .id()
            } else {
                scenario
                    .add_artifact_from_oracle(P0, "Synthetic independent dynamic source", "")
                    .id()
            };
            let oracle = if source_kind == "self" {
                "This spell costs {1} less to cast for each card with the same name as that spell in your graveyard.\nYou gain 1 life."
            } else {
                "You gain 1 life."
            };
            let spell = scenario
                .add_spell_to_hand_from_oracle(P0, "Synthetic dynamic spell", true, oracle)
                .with_mana_cost(blue_cost(5, 1))
                .id();
            for _ in 0..2 {
                scenario.add_spell_to_graveyard(
                    P0,
                    if matching {
                        "Synthetic dynamic spell"
                    } else {
                        "Synthetic independent dynamic source"
                    },
                    true,
                );
            }
            let definition = locket_definition();
            let mut runner = scenario.build();
            match source_kind {
                "self" => {
                    let statics = &runner.state().objects[&spell].static_definitions;
                    assert_eq!(statics.len(), 1);
                    assert!(matches!(
                        &statics[0].mode,
                        StaticMode::ModifyCost {
                            dynamic_count: Some(QuantityRef::ObjectCount { .. }),
                            ..
                        }
                    ));
                }
                "battlefield" => {
                    assert_eq!(runner.state().objects[&source].static_definitions.len(), 1);
                }
                "temporary" => {
                    runner
                        .state_mut()
                        .add_transient_continuous_effect(
                            source,
                            P0,
                            Duration::UntilEndOfTurn,
                            TargetFilter::SpecificPlayer { id: P0 },
                            vec![ContinuousModification::GrantStaticAbility {
                                definition: Box::new(definition),
                            }],
                            None,
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            engine::game::layers::flush_layers(runner.state_mut());
            assert_eq!(
                locket_count(&runner, source, spell),
                if matching { 2 } else { 0 }
            );
            assert_eq!(
                engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
                Some(blue_cost(if matching { 3 } else { 5 }, 1)),
                "{source_kind}, matching={matching}"
            );
            let outcome = runner.cast(spell).resolve();
            outcome.assert_zone(&[spell], Zone::Graveyard);
            outcome.assert_life_delta(P0, 1);
            assert_eq!(
                outcome.mana_pool_total(P0),
                0,
                "{source_kind}, matching={matching}"
            );
        }
    }
}

#[test]
fn locket_foretell_face_up_name() {
    use engine::types::game_state::{CastingVariant, StackEntryKind};
    for matching in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, 2);
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_hand(P0, "Behold the Multiverse", true)
            .with_mana_cost(blue_cost(3, 1))
            .from_oracle_text_with_keywords(&["Foretell", "Scry"], BEHOLD)
            .id();
        scenario.add_spell_to_graveyard(
            P0,
            if matching {
                "Behold the Multiverse"
            } else {
                "Other name"
            },
            true,
        );
        for _ in 0..5 {
            scenario.add_spell_to_library_top(P0, "Synthetic draw marker", true);
        }
        let mut runner = scenario.build();
        assert!(runner.state().objects[&spell].parse_warnings.is_empty());
        assert!(runner.state().objects[&spell]
            .keywords
            .contains(&Keyword::Foretell(blue_cost(1, 1))));
        assert_eq!(runner.state().objects[&spell].abilities.len(), 1);
        assert!(!matches!(
            runner.state().objects[&spell].abilities[0].effect.as_ref(),
            Effect::Unimplemented { .. }
        ));
        runner
            .act(GameAction::Foretell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
            })
            .unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        let object = &runner.state().objects[&spell];
        assert_eq!(object.zone, Zone::Exile);
        assert!(object.face_down && object.foretold);
        assert_eq!(object.name, "Behold the Multiverse");
        assert!(object.effective_names().is_empty());
        assert_eq!(
            object.spell_names_for(CastingVariant::Foretell),
            vec!["behold the multiverse"]
        );
        // Existing Foretell fixture recipe: the permission tests the recorded turn.
        runner.state_mut().turn_number += 1;
        runner.state_mut().players[0].mana_pool = (0..if matching { 1 } else { 2 })
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect();
        assert_eq!(locket_count(&runner, source, spell), i32::from(matching));
        let committed = runner
            .cast(spell)
            .casting_variant(CastingVariant::Foretell)
            .commit();
        let entry = committed
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == spell)
            .unwrap();
        assert!(
            matches!(&entry.kind, StackEntryKind::Spell { casting_variant: CastingVariant::Foretell, actual_mana_spent, .. } if *actual_mana_spent == if matching { 1 } else { 2 })
        );
        assert!(!committed.state().objects[&spell].face_down);
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_hand_drawn(P0, 2);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
}

#[test]
fn locket_concealed_exile_ordinary_cost_name() {
    use engine::types::ability::{CastingPermission, ExileGrantCostProvenance};
    use engine::types::game_state::CastingVariant;
    for matching in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, if matching { 3 } else { 4 });
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_exile(P0, "Behold the Multiverse", true)
            .with_mana_cost(blue_cost(3, 1))
            .from_oracle_text_with_keywords(&["Foretell", "Scry"], BEHOLD)
            .id();
        scenario.add_spell_to_graveyard(
            P0,
            if matching {
                "Behold the Multiverse"
            } else {
                "Other name"
            },
            true,
        );
        for _ in 0..5 {
            scenario.add_spell_to_library_top(P0, "Synthetic ordinary draw marker", true);
        }
        let mut runner = scenario.build();
        let mut permission = alternative_permission(P0, 3, 0);
        let CastingPermission::ExileWithAltCost {
            cost,
            cost_provenance,
            cast_cost_modifier,
            ..
        } = &mut permission
        else {
            unreachable!()
        };
        *cost = blue_cost(3, 1);
        *cost_provenance = ExileGrantCostProvenance::NormalCost;
        *cast_cost_modifier = None;
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        object.face_down = true;
        object.casting_permissions = vec![permission];
        assert_eq!(
            object.spell_names_for(CastingVariant::Normal),
            vec!["behold the multiverse"]
        );
        assert_eq!(locket_count(&runner, source, spell), i32::from(matching));
        let committed = runner.cast(spell).commit();
        assert!(!committed.state().objects[&spell].face_down);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_hand_drawn(P0, 2);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
}

fn assert_restoration_front(state: &GameState, spell: ObjectId, back: &BackFaceData) {
    use engine::types::card_type::{CardType, CoreType};
    let parsed = engine::parser::parse_oracle_text(
        SEA_GATE_RESTORATION,
        "Sea Gate Restoration",
        &[],
        &["Sorcery".into()],
        &[],
    );
    assert!(parsed.parse_warnings.is_empty());
    assert!(!parsed.abilities.is_empty());
    let object = &state.objects[&spell];
    assert_eq!(object.name, "Sea Gate Restoration");
    assert_eq!(object.base_name, "Sea Gate Restoration");
    let types = CardType {
        core_types: vec![CoreType::Sorcery],
        ..Default::default()
    };
    assert_eq!(object.card_types, types);
    assert_eq!(object.base_card_types, types);
    assert_eq!(object.mana_cost, blue_cost(4, 3));
    assert_eq!(object.base_mana_cost, blue_cost(4, 3));
    assert_eq!(object.abilities.as_ref(), &parsed.abilities);
    assert_eq!(object.base_abilities.as_ref(), &parsed.abilities);
    assert_eq!(object.back_face.as_ref(), Some(back));
    assert_eq!(back.name, "Sea Gate, Reborn");
    assert_eq!(back.layout_kind, Some(LayoutKind::Modal));
    assert_eq!(back.mana_cost, ManaCost::NoCost);
    assert_eq!(back.card_types.core_types, vec![CoreType::Land]);
}

fn cancel_concealed_restoration(
    runner: &mut GameRunner,
    spell: ObjectId,
    variant: engine::types::game_state::CastingVariant,
    expected_cost: ManaCost,
    back: &BackFaceData,
) {
    let original = runner.state().objects[&spell].clone();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: original.card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
    if let WaitingFor::CastingVariantChoice { options, .. } = &runner.state().waiting_for {
        let index = options
            .iter()
            .position(|option| option.variant == variant)
            .unwrap();
        runner
            .act(GameAction::ChooseCastingVariant { index })
            .unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    let pending = runner.state().pending_cast.as_ref().unwrap();
    assert_eq!(pending.casting_variant, variant);
    assert_eq!(pending.cost, expected_cost);
    assert!(runner.state().objects[&spell].face_down);
    assert_restoration_front(runner.state(), spell, back);
    // CR 733.1: reversal retains the concealed origin and its cast permissions.
    runner.act(GameAction::CancelCast).unwrap();
    let object = &runner.state().objects[&spell];
    assert_eq!(object.zone, Zone::Exile);
    assert!(object.face_down);
    assert!(object.effective_names().is_empty());
    assert_eq!(object.foretold, original.foretold);
    assert_eq!(object.casting_permissions, original.casting_permissions);
    assert_restoration_front(runner.state(), spell, back);
    assert!(engine::game::filter::CastSpellContext::announced(runner.state(), spell).is_none());
    assert!(object.spell_announcement.is_none());
}

fn second_restoration_uses_resolved_front(
    runner: &mut GameRunner,
    source: ObjectId,
    first: ObjectId,
    second: ObjectId,
    witness: ObjectId,
    back: &BackFaceData,
) {
    use engine::types::game_state::{CastingVariant, StackEntryKind};
    // Fixture setup removes only the separate original witness. The first
    // resolved object must be the sole remaining common-name graveyard card.
    engine::game::zones::move_to_zone(runner.state_mut(), witness, Zone::Exile, &mut vec![]);
    engine::game::zones::move_to_zone(runner.state_mut(), second, Zone::Hand, &mut vec![]);
    assert_restoration_front(runner.state(), first, back);
    assert!(!runner.state().objects[&first].face_down);
    let matches: Vec<_> = runner.state().players[0]
        .graveyard
        .iter()
        .copied()
        .filter(|id| {
            runner.state().objects[id]
                .effective_names()
                .contains(&"sea gate restoration".to_string())
        })
        .collect();
    assert_eq!(matches, vec![first]);
    assert_eq!(locket_count(runner, source, second), 1);
    runner.state_mut().players[0].mana_pool = (0..6)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect();
    let offered = engine::game::casting::display_spell_cost(runner.state(), P0, second).unwrap();
    assert_eq!(offered, blue_cost(3, 3));
    let committed = runner
        .cast(second)
        .casting_variant(CastingVariant::Normal)
        .commit();
    assert_restoration_front(committed.state(), second, back);
    assert!(!committed.state().objects[&second].face_down);
    assert!(committed
        .state()
        .stack
        .iter()
        .any(|entry| entry.id == second
            && matches!(
                &entry.kind,
                StackEntryKind::Spell {
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 6,
                    ..
                }
            )));
    assert_eq!(committed.state().players[0].mana_pool.total(), 0);
    let outcome = committed.resolve();
    outcome.assert_zone(&[first, second], Zone::Graveyard);
    assert_restoration_front(outcome.state(), first, back);
    assert_restoration_front(outcome.state(), second, back);
    assert!(!outcome.state().objects[&second].face_down);
    assert!(outcome.hand_drawn(P0) > 0);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

#[test]
fn locket_granted_foretell_concealed_modal_name() {
    use engine::types::ability::CastingPermission;
    use engine::types::game_state::{CastingVariant, StackEntryKind};
    for matching in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, 2);
        let source = add_locket(&mut scenario);
        let devourer = scenario
            .add_creature(P0, "Dream Devourer", 0, 3)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Black],
            })
            .from_oracle_text(DREAM_DEVOURER)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Sea Gate Restoration", false, SEA_GATE_RESTORATION)
            .with_mana_cost(blue_cost(4, 3))
            .id();
        let witness = scenario
            .add_spell_to_graveyard(
                P0,
                if matching {
                    "Sea Gate Restoration"
                } else {
                    "Other name"
                },
                false,
            )
            .id();
        let second = scenario
            .add_spell_to_exile(P0, "Sea Gate Restoration", false)
            .with_mana_cost(blue_cost(4, 3))
            .from_oracle_text(SEA_GATE_RESTORATION)
            .id();
        for _ in 0..2 {
            scenario.add_spell_to_hand(P0, "Synthetic retained hand card", false);
        }
        for _ in 0..32 {
            scenario.add_spell_to_library_top(P0, "Synthetic restoration draw marker", false);
        }
        let mut runner = scenario.build();
        let back_face = sea_gate_reborn_face();
        runner
            .state_mut()
            .objects
            .get_mut(&second)
            .unwrap()
            .back_face = Some(back_face.clone());
        runner
            .state_mut()
            .objects
            .get_mut(&spell)
            .unwrap()
            .back_face = Some(back_face.clone());
        for id in [devourer, spell] {
            let object = &runner.state().objects[&id];
            assert!(object.parse_warnings.is_empty());
            assert!(object
                .abilities
                .iter()
                .all(|ability| !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. })));
        }
        assert!(!runner.state().objects[&spell].abilities.is_empty());
        let foretold_turn = runner.state().turn_number;
        runner
            .act(GameAction::Foretell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
            })
            .expect("printed Dream Devourer grants Foretell to the modal front");
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        let object = &runner.state().objects[&spell];
        assert_eq!(object.zone, Zone::Exile);
        assert!(object.face_down && object.foretold);
        assert_eq!(object.back_face.as_ref(), Some(&back_face));
        assert!(object.effective_names().is_empty());
        assert!(object.casting_permissions.iter().any(|permission| matches!(
            permission, CastingPermission::Foretold { cost, turn_foretold }
                if *cost == blue_cost(2, 3) && *turn_foretold == foretold_turn
        )));
        // Resolve Dream Devourer's printed trigger before the later-turn cast.
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&devourer].power, Some(2));
        // CR 702.143a: the recorded foretell turn must have ended.
        runner.state_mut().turn_number = foretold_turn + 1;
        assert_eq!(runner.state().active_player, P0);
        assert_eq!(runner.state().phase, Phase::PreCombatMain);
        let paid = if matching { 4 } else { 5 };
        runner.state_mut().players[0].mana_pool = (0..paid)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect();
        assert_eq!(locket_count(&runner, source, spell), i32::from(matching));
        // CR 601.2f + CR 118.7a: {4}UUU -> Foretell {2}UUU -> {1}UUU.
        cancel_concealed_restoration(
            &mut runner,
            spell,
            CastingVariant::Foretell,
            blue_cost(if matching { 1 } else { 2 }, 3),
            &back_face,
        );
        let committed = runner
            .cast(spell)
            .casting_variant(CastingVariant::Foretell)
            .commit();
        let object = &committed.state().objects[&spell];
        assert_eq!(object.name, "Sea Gate Restoration");
        assert_eq!(object.back_face.as_ref(), Some(&back_face));
        assert_eq!(
            object.spell_names_for(CastingVariant::Foretell),
            vec!["sea gate restoration"]
        );
        assert!(committed.state().stack.iter().any(|entry| entry.id == spell && matches!(
            &entry.kind, StackEntryKind::Spell { casting_variant: CastingVariant::Foretell, actual_mana_spent, .. }
                if *actual_mana_spent == paid
        )));
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        assert_restoration_front(committed.state(), spell, &back_face);
        assert!(!committed.state().objects[&spell].face_down);
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_hand_drawn(P0, 3);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert_restoration_front(outcome.state(), spell, &back_face);
        assert!(!outcome.state().objects[&spell].face_down);
        second_restoration_uses_resolved_front(
            &mut runner,
            source,
            spell,
            second,
            witness,
            &back_face,
        );
    }
}

#[test]
fn locket_concealed_modal_ordinary_cost_name() {
    use engine::types::ability::{CastingPermission, ExileGrantCostProvenance};
    use engine::types::game_state::{CastingVariant, StackEntryKind};
    // Synthetic ordinary-cost exile permission, applied to the complete printed MDFC.
    for matching in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let paid = if matching { 6 } else { 7 };
        mana(&mut scenario, paid);
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_exile(P0, "Sea Gate Restoration", false)
            .with_mana_cost(blue_cost(4, 3))
            .from_oracle_text(SEA_GATE_RESTORATION)
            .id();
        let witness = scenario
            .add_spell_to_graveyard(
                P0,
                if matching {
                    "Sea Gate Restoration"
                } else {
                    "Other name"
                },
                false,
            )
            .id();
        let second = scenario
            .add_spell_to_exile(P0, "Sea Gate Restoration", false)
            .with_mana_cost(blue_cost(4, 3))
            .from_oracle_text(SEA_GATE_RESTORATION)
            .id();
        for _ in 0..32 {
            scenario.add_spell_to_library_top(
                P0,
                "Synthetic ordinary restoration draw marker",
                false,
            );
        }
        let mut runner = scenario.build();
        let mut permission = alternative_permission(P0, 4, 0);
        let CastingPermission::ExileWithAltCost {
            cost,
            cost_provenance,
            cast_cost_modifier,
            ..
        } = &mut permission
        else {
            unreachable!()
        };
        *cost = blue_cost(4, 3);
        *cost_provenance = ExileGrantCostProvenance::NormalCost;
        *cast_cost_modifier = None;
        let back_face = sea_gate_reborn_face();
        runner
            .state_mut()
            .objects
            .get_mut(&second)
            .unwrap()
            .back_face = Some(back_face.clone());
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        object.back_face = Some(back_face.clone());
        object.face_down = true;
        object.casting_permissions = vec![permission];
        assert!(object.parse_warnings.is_empty());
        assert!(!object.abilities.is_empty());
        assert!(object
            .abilities
            .iter()
            .all(|ability| !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. })));
        assert!(object.effective_names().is_empty());
        assert_eq!(locket_count(&runner, source, spell), i32::from(matching));
        // CR 406.3a + CR 712.8f: selected Normal uses the named front face.
        cancel_concealed_restoration(
            &mut runner,
            spell,
            CastingVariant::Normal,
            blue_cost(if matching { 3 } else { 4 }, 3),
            &back_face,
        );
        let committed = runner
            .cast(spell)
            .casting_variant(CastingVariant::Normal)
            .commit();
        assert_eq!(
            committed.state().objects[&spell].name,
            "Sea Gate Restoration"
        );
        assert_eq!(
            committed.state().objects[&spell].back_face.as_ref(),
            Some(&back_face)
        );
        assert!(committed.state().stack.iter().any(|entry| entry.id == spell && matches!(
            &entry.kind, StackEntryKind::Spell { casting_variant: CastingVariant::Normal, actual_mana_spent, .. }
                if *actual_mana_spent == u32::try_from(paid).unwrap()
        )));
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        assert_restoration_front(committed.state(), spell, &back_face);
        assert!(!committed.state().objects[&spell].face_down);
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_hand_drawn(P0, 1);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert_restoration_front(outcome.state(), spell, &back_face);
        assert!(!outcome.state().objects[&spell].face_down);
        second_restoration_uses_resolved_front(
            &mut runner,
            source,
            spell,
            second,
            witness,
            &back_face,
        );
    }
}

fn blue_cost(generic: u32, blue: usize) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: vec![ManaCostShard::Blue; blue],
    }
}

fn mana(scenario: &mut GameScenario, count: usize) {
    scenario.with_mana_pool(
        P0,
        (0..count)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn sailors_board(populated: bool, pool: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, pool);
    let spell = scenario
        .add_creature_to_hand(P0, "Sailors' Bane", 7, 7)
        .with_mana_cost(blue_cost(7, 2))
        .from_oracle_text_with_keywords(&["Ward"], SAILORS)
        .id();
    let mut adventure_ids = Vec::new();
    if populated {
        let overlapping = scenario
            .add_spell_to_exile(P0, "Synthetic instant with Adventure", true)
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic sorcery", false);
        let adventure = scenario
            .add_creature_to_graveyard(P0, "Synthetic Adventure permanent", 1, 1)
            .id();
        // CR 715.2a + CR 715.2c: actual alternative-face metadata qualifies a
        // permanent; the instant with the same metadata still counts only once.
        adventure_ids.extend([overlapping, adventure]);
        scenario.add_spell_to_exile(P1, "Wrong owner exile", true);
        scenario.add_spell_to_graveyard(P1, "Opponent graveyard", false);
        scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
        scenario.add_spell_to_hand(P0, "Wrong zone instant", true);
    }
    let mut runner = scenario.build();
    for id in adventure_ids {
        runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(BackFaceData {
            name: "Synthetic Adventure alternative".into(),
            layout_kind: Some(LayoutKind::Adventure),
            ..Default::default()
        });
    }
    (runner, spell)
}

#[test]
fn sailors_population_cost() {
    for (populated, pool) in [(true, 6), (false, 9)] {
        let (mut runner, spell) = sailors_board(populated, pool);
        assert_eq!(runner.state().objects[&spell].static_definitions.len(), 1);
        assert!(runner.state().objects[&spell].parse_warnings.is_empty());
        let parsed = engine::parser::parse_oracle_text(
            SAILORS,
            "Sailors' Bane",
            &["Ward".into()],
            &["Creature".into()],
            &["Dragon".into(), "Turtle".into()],
        );
        let StaticMode::ModifyCost {
            dynamic_count: Some(quantity),
            ..
        } = &parsed.statics[0].mode
        else {
            panic!("full public coordinated population");
        };
        assert_eq!(
            resolve_quantity(
                runner.state(),
                &QuantityExpr::Ref {
                    qty: quantity.clone()
                },
                P0,
                spell
            ),
            if populated { 3 } else { 0 },
            "instant plus Adventure is one shared-population member"
        );
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        // CR 118.7a + CR 601.2f: three qualifying cards remove three generic
        // mana, retaining both blue pips; all hostile cards contribute zero.
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty());
        assert!(outcome.state().resolution_stack.is_empty());
    }
    let (mut runner, spell) = sailors_board(true, 6);
    runner.state_mut().players[0].mana_pool = (0..6)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect();
    // CR 118.7a: the generic reduction cannot pay either blue pip.
    assert!(runner.cast(spell).try_resolve().is_err());
    let (mut runner, spell) = sailors_board(true, 5);
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "one short cannot pay the locked cost"
    );
}

#[test]
fn conditional_head_preserves_paid_akki_underling_bonus() {
    const AKKI: &str = "As long as you have seven or more cards in hand, this creature gets +2/+1 and has first strike.";
    for remaining in [7, 6] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_creature_to_hand(P0, "Akki Underling", 2, 1)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Red],
            })
            .from_oracle_text(AKKI)
            .id();
        for index in 0..remaining {
            scenario.add_card_to_hand(P0, &format!("Synthetic hand witness {index}"));
        }
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ],
        );
        let mut runner = scenario.build();
        let committed = runner.cast(spell).commit();
        // CR 601.2f + CR 601.2h: the full printed 1R is locked and paid.
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        assert_eq!(committed.state().players[0].hand.len(), remaining);
        assert!(committed.state().stack.iter().any(|entry| entry.id == spell
            && matches!(
                entry.kind,
                StackEntryKind::Spell {
                    actual_mana_spent: 2,
                    ..
                }
            )));
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        assert_eq!(outcome.state().players[0].hand.len(), remaining);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        // CR 208.1 + CR 611.3a: the live seven-card gate adds exactly 2/1.
        outcome.assert_power_toughness(
            spell,
            if remaining == 7 { 4 } else { 2 },
            if remaining == 7 { 2 } else { 1 },
        );
        // CR 613.1f: the same gate grants first strike in the ability layer.
        assert_eq!(
            engine::game::keywords::object_has_effective_keyword_kind(
                outcome.state(),
                spell,
                engine::types::keywords::KeywordKind::FirstStrike
            ),
            remaining == 7
        );
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty());
        assert!(outcome.state().resolution_stack.is_empty());
    }
}

const HEARTH: &str = "This spell costs {X} less to cast, where X is the number of cards in your graveyard that are instant cards, sorcery cards, and/or have an Adventure.";
const STOKE: &str = "Discard your hand, then draw two cards. (Then exile this card. You may cast the creature later from exile.)";
const FRANTIC: &str = "Frantic Firebolt deals X damage to target creature, where X is 2 plus the number of cards in your graveyard that are instant cards, sorcery cards, and/or have an Adventure.";

#[test]
fn resolving_adventure_whole_stoke_paid_discard_draw_and_normal_play() {
    use engine::types::ability::CastingPermission;
    use engine::types::events::GameEvent;
    for caster in [P0, P1] {
        for empty in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let hearth = scenario
                .add_creature_to_hand(caster, "Hearth Elemental", 4, 5)
                .with_mana_cost(ManaCost::Cost {
                    generic: 5,
                    shards: vec![ManaCostShard::Red],
                })
                .with_subtypes(vec!["Elemental"])
                .from_oracle_text(HEARTH)
                .id();
            let discarded: Vec<_> = if empty {
                vec![]
            } else {
                ["First ordinary creature", "Second ordinary creature"]
                    .into_iter()
                    .map(|name| scenario.add_creature_to_hand(caster, name, 1, 1).id())
                    .collect()
            };
            scenario.with_library_top(caster, &["First draw", "Second draw", "Reserve"]);
            scenario.with_mana_pool(
                caster,
                (0..2)
                    .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                    .collect(),
            );
            let mut runner = scenario.build();
            runner.state_mut().active_player = caster;
            runner.state_mut().priority_player = caster;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
            runner
                .state_mut()
                .objects
                .get_mut(&hearth)
                .unwrap()
                .back_face = Some(stoke_genius_face());
            let library: Vec<_> = runner.state().players[caster.0 as usize]
                .library
                .iter()
                .copied()
                .collect();
            let committed = runner.cast(hearth).adventure_face(false).commit();
            assert_eq!(committed.state().objects[&hearth].name, "Stoke Genius");
            assert_eq!(committed.state().objects[&hearth].mana_cost.mana_value(), 2);
            assert!(matches!(
                committed.state().stack.back().unwrap().kind,
                StackEntryKind::Spell {
                    actual_mana_spent: 2,
                    ..
                }
            ));
            let outcome = committed.resolve();
            outcome.assert_zone(&discarded, Zone::Graveyard);
            outcome.assert_zone(&[hearth], Zone::Exile);
            assert_eq!(outcome.state().players[caster.0 as usize].hand.len(), 2);
            assert_eq!(
                outcome.state().players[caster.0 as usize]
                    .hand
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                library[..2].iter().copied().collect()
            );
            assert_eq!(outcome.state().objects[&library[2]].zone, Zone::Library);
            assert_eq!(outcome.state().objects[&hearth].name, "Hearth Elemental");
            assert_eq!(outcome.state().objects[&hearth].mana_cost.mana_value(), 6);
            assert!(outcome.state().objects[&hearth].casting_permissions.iter().any(|p| matches!(p,
                CastingPermission::AdventureCreature { granted_to: Some(player), exile_incarnation: Some(incarnation) }
                    if *player == caster && *incarnation == outcome.state().objects[&hearth].incarnation)));
            let first_draw = outcome
                .events()
                .iter()
                .position(
                    |e| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == caster),
                )
                .unwrap();
            for id in &discarded {
                let discard = outcome
                    .events()
                    .iter()
                    .position(
                        |e| matches!(e, GameEvent::Discarded { object_id, .. } if object_id == id),
                    )
                    .unwrap();
                assert!(discard < first_draw);
            }
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(outcome.state().stack.is_empty());
            assert!(outcome.state().resolution_stack.is_empty());
            drop(outcome);
            for _ in 0..6 {
                runner.state_mut().players[caster.0 as usize]
                    .mana_pool
                    .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
            }
            let committed = runner.cast(hearth).commit();
            assert!(matches!(
                committed.state().stack.back().unwrap().kind,
                StackEntryKind::Spell {
                    actual_mana_spent: 6,
                    ..
                }
            ));
            let outcome = committed.resolve();
            outcome.assert_zone(&[hearth], Zone::Battlefield);
            outcome.assert_power_toughness(hearth, 4, 5);
            assert!(outcome.state().objects[&hearth]
                .casting_permissions
                .is_empty());
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(
                outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty()
            );
        }
    }
}

#[test]
fn whole_stoke_payment_and_cancel_negatives_have_a_paid_positive_twin() {
    for (pool, red) in [(1, true), (2, false), (2, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let hearth = scenario
            .add_creature_to_hand(P0, "Hearth Elemental", 4, 5)
            .with_mana_cost(ManaCost::Cost {
                generic: 5,
                shards: vec![ManaCostShard::Red],
            })
            .with_subtypes(vec!["Elemental"])
            .from_oracle_text(HEARTH)
            .id();
        scenario.with_library_top(P0, &["Draw first", "Draw second", "Reserve"]);
        scenario.with_mana_pool(
            P0,
            (0..pool)
                .map(|_| {
                    ManaUnit::new(
                        if red {
                            ManaType::Red
                        } else {
                            ManaType::Colorless
                        },
                        ObjectId(0),
                        false,
                        vec![],
                    )
                })
                .collect(),
        );
        let remaining = [
            scenario
                .add_creature_to_hand(P0, "Stoke counter unchanged A", 1, 1)
                .id(),
            scenario
                .add_creature_to_hand(P0, "Stoke counter unchanged B", 1, 1)
                .id(),
        ];
        let cancel = scenario
            .add_spell_to_hand_from_oracle(P1, "Cancel", true, "Counter target spell.")
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue; 2],
            })
            .id();
        scenario.with_mana_pool(
            P1,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&hearth)
            .unwrap()
            .back_face = Some(stoke_genius_face());
        if pool == 2 && red {
            runner
                .act(GameAction::CastSpell {
                    object_id: hearth,
                    card_id: runner.state().objects[&hearth].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::CastOffer {
                    player: P0,
                    kind: engine::types::game_state::CastOfferKind::Adventure { .. }
                }
            ));
            assert!(
                engine::ai_support::legal_actions(runner.state()).contains(&GameAction::CancelCast)
            );
            runner.act(GameAction::CancelCast).unwrap();
            assert_eq!(runner.state().objects[&hearth].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&hearth].name, "Hearth Elemental");
            assert_eq!(runner.state().players[0].mana_pool.total(), 2);
            let mut countered = GameRunner::from_state(runner.state().clone());
            let library_before = countered.state().players[0].library.clone();
            countered.cast(hearth).adventure_face(false).commit();
            countered.act(GameAction::PassPriority).unwrap();
            assert!(matches!(
                countered.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            ));
            let committed = countered.cast(cancel).target_object(hearth).commit();
            assert!(matches!(
                committed.state().stack.back().unwrap().kind,
                StackEntryKind::Spell {
                    actual_mana_spent: 3,
                    ..
                }
            ));
            let outcome = committed.resolve();
            outcome.assert_zone(&[hearth], Zone::Graveyard);
            outcome.assert_zone(&remaining, Zone::Hand);
            assert_eq!(outcome.state().players[0].library, library_before);
            assert!(!outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::CardDrawn { player_id: P0, .. }
                    | GameEvent::Discarded { player_id: P0, .. }
            )));
            assert!(outcome.state().objects[&hearth]
                .casting_permissions
                .is_empty());
            assert_eq!(outcome.state().objects[&hearth].name, "Hearth Elemental");
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(
                outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty()
            );
            // CR 715.3d: countering the paid Adventure and subsequently
            // exiling its card earns no play permission. Retaining its actual
            // stack program and later exile history must never mint a grant.
            let mut counter_exiled = outcome.state().clone();
            engine::game::zones::move_to_zone(
                &mut counter_exiled,
                hearth,
                Zone::Exile,
                &mut Vec::new(),
            );
            assert_eq!(counter_exiled.objects[&hearth].zone, Zone::Exile);
            for (route, payload) in adventure_current_save_payloads(&counter_exiled)
                .into_iter()
                .enumerate()
            {
                let loaded = restore_adventure_current_save(route, payload).unwrap();
                assert!(loaded.objects[&hearth].casting_permissions.is_empty());
                for player in [P0, P1] {
                    assert!(
                        !engine::game::casting::has_potentially_authorizing_object_cast_permission(
                            &loaded.objects[&hearth],
                            player
                        )
                    );
                }
            }
            runner
                .cast(hearth)
                .adventure_face(false)
                .resolve()
                .assert_zone(&[hearth], Zone::Exile);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        } else {
            assert!(!engine::game::casting::can_cast_adventure_face_now(
                runner.state(),
                P0,
                hearth,
                false
            ));
            assert!(runner
                .cast(hearth)
                .adventure_face(false)
                .try_resolve()
                .is_err());
            assert_eq!(runner.state().objects[&hearth].zone, Zone::Hand);
            assert_eq!(runner.state().players[0].mana_pool.total(), pool);
            assert!(runner.state().objects[&hearth]
                .casting_permissions
                .is_empty());
        }
    }
}

fn paid_whole_stoke_authority_scene() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Draw A",
            "Draw B",
            "Reserve A",
            "Reserve B",
            "Reserve C",
            "Reserve D",
        ],
    );
    scenario.with_library_top(
        P1,
        &[
            "Opponent reserve A",
            "Opponent reserve B",
            "Opponent reserve C",
        ],
    );
    let hearth = scenario
        .add_creature_to_hand(P0, "Hearth Elemental", 4, 5)
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::Red],
        })
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text(HEARTH)
        .id();
    let silence = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Silence",
            true,
            "Your opponents can't cast spells this turn.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&hearth)
        .unwrap()
        .back_face = Some(stoke_genius_face());
    runner
        .cast(hearth)
        .adventure_face(false)
        .resolve()
        .assert_zone(&[hearth], Zone::Exile);
    assert_eq!(runner.state().objects[&silence].zone, Zone::Hand);
    (runner, hearth, silence)
}

fn adventure_authority_save_representations(state: &GameState) -> Vec<GameState> {
    use engine::types::game_state::PersistedGameState;
    use engine::types::resolution::ResolutionStateWire;
    fn old_delivery_fields(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                fields.remove("delivery_owner");
                for child in fields.values_mut() {
                    old_delivery_fields(child);
                }
            }
            Value::Array(values) => {
                for child in values {
                    old_delivery_fields(child);
                }
            }
            _ => {}
        }
    }
    let raw = serde_json::to_value(state).unwrap();
    let direct: GameState = serde_json::from_value(raw.clone()).unwrap();
    let persisted: PersistedGameState = serde_json::from_value(raw.clone()).unwrap();
    let trusted: PersistedGameState = serde_json::from_value(
        serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap(),
    )
    .unwrap();
    let mut restored = vec![
        direct,
        persisted.into_game_state().unwrap(),
        trusted.into_game_state().unwrap(),
    ];
    let mut v1 = raw;
    v1.as_object_mut().unwrap().remove("resolution_stack");
    v1["resolution_state_version"] = serde_json::json!(1);
    if let Some(continuation) = state.active_ability_continuation() {
        v1["pending_continuation"] = serde_json::to_value(continuation).unwrap();
    }
    if let Some(spell) = state.active_spell_resolution() {
        v1["pending_spell_resolution"] = serde_json::to_value(spell).unwrap();
    }
    if let Some(drains) = state.active_post_replacement_drains() {
        v1["post_replacement_drains"] = serde_json::to_value(drains).unwrap();
    }
    old_delivery_fields(&mut v1);
    restored.push(
        serde_json::from_value::<ResolutionStateWire>(v1)
            .unwrap()
            .into_game_state(),
    );
    for version in [2, 3, 4] {
        let mut value =
            serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap();
        old_delivery_fields(&mut value);
        value["resolution_state_version"] = serde_json::json!(version);
        restored.push(
            serde_json::from_value::<ResolutionStateWire>(value)
                .unwrap()
                .into_game_state(),
        );
    }
    for loaded in &restored {
        assert_eq!(loaded.waiting_for, state.waiting_for);
        assert_eq!(
            loaded.owed_spell_final_part(),
            state.owed_spell_final_part()
        );
    }
    restored
}

#[test]
fn resolving_adventure_selected_index_survives_payment_restore_and_rejects_a_stale_slot() {
    use engine::types::ability::CastingPermission;
    use engine::types::game_state::CastingPermissionIndex;
    for foreign_first in [false, true] {
        let (mut runner, hearth, _) = paid_whole_stoke_authority_scene();
        let earned = runner.state().objects[&hearth].casting_permissions[0].clone();
        let foreign = alternative_permission(P1, 0, 0);
        runner
            .state_mut()
            .objects
            .get_mut(&hearth)
            .unwrap()
            .casting_permissions = if foreign_first {
            vec![foreign, earned]
        } else {
            vec![earned, foreign]
        };
        assert!(!engine::game::casting::can_cast_adventure_face_now(
            runner.state(),
            P0,
            hearth,
            false
        ));
        let independent = runner.state().clone();
        for _ in 0..6 {
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::Red,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        runner
            .act(GameAction::CastSpell {
                object_id: hearth,
                card_id: runner.state().objects[&hearth].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Manual,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ));
        let index = usize::from(foreign_first);
        let selected = runner.state().pending_cast.as_ref().unwrap();
        assert_eq!(
            selected.casting_permission_index,
            Some(CastingPermissionIndex(index))
        );
        assert_eq!(
            selected.cost,
            ManaCost::Cost {
                generic: 5,
                shards: vec![ManaCostShard::Red]
            }
        );
        for loaded in adventure_authority_save_representations(runner.state()) {
            assert_eq!(
                loaded
                    .pending_cast
                    .as_ref()
                    .unwrap()
                    .casting_permission_index,
                Some(CastingPermissionIndex(index))
            );
            let mut stale = GameRunner::from_state(loaded.clone());
            stale
                .state_mut()
                .objects
                .get_mut(&hearth)
                .unwrap()
                .casting_permissions
                .remove(index);
            assert!(stale.act(GameAction::PassPriority).is_err());
            assert_eq!(
                stale.state().players[0].mana_pool.total(),
                6,
                "a missing or wrong exact slot fails before payment"
            );
            let mut valid = GameRunner::from_state(loaded);
            let sibling = 1 - index;
            let CastingPermission::ExileWithAltCost { granted_to, .. } = &mut valid
                .state_mut()
                .objects
                .get_mut(&hearth)
                .unwrap()
                .casting_permissions[sibling]
            else {
                panic!("the hostile sibling remains indexed");
            };
            *granted_to = Some(P0);
            valid.act(GameAction::PassPriority).unwrap();
            assert!(
                matches!(
                    valid.state().stack.back().unwrap().kind,
                    StackEntryKind::Spell {
                        actual_mana_spent: 6,
                        ..
                    }
                ),
                "a newly free sibling must not reprice the elected normal grant"
            );
            assert_eq!(valid.state().players[0].mana_pool.total(), 0);
            valid.resolve_top();
            assert_eq!(valid.state().objects[&hearth].zone, Zone::Battlefield);
            assert!(valid.state().stack.is_empty() && valid.state().resolution_stack.is_empty());
        }
        let mut independent = GameRunner::from_state(independent);
        independent.advance_to_phase(Phase::End);
        independent.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(independent.state().active_player, P1);
        independent
            .act(GameAction::CastSpell {
                object_id: hearth,
                card_id: independent.state().objects[&hearth].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(matches!(
            independent.state().waiting_for,
            WaitingFor::CastOffer {
                player: P1,
                kind: engine::types::game_state::CastOfferKind::Adventure { .. }
            }
        ));
        assert!(engine::ai_support::legal_actions(independent.state())
            .contains(&GameAction::ChooseAdventureFace { creature: false }));
        independent
            .act(GameAction::ChooseAdventureFace { creature: false })
            .unwrap();
        assert_eq!(independent.state().objects[&hearth].name, "Stoke Genius");
        assert!(matches!(
            independent.state().stack.back().unwrap().kind,
            StackEntryKind::Spell {
                actual_mana_spent: 0,
                ..
            }
        ));
        independent.resolve_top();
        let object = &independent.state().objects[&hearth];
        assert_eq!(object.owner, P0);
        assert_eq!(object.zone, Zone::Exile);
        assert_eq!(
            object.casting_permissions,
            vec![CastingPermission::resolved_adventure(
                P1,
                engine::types::identifiers::ObjectIncarnationRef::from_object(object)
            )]
        );
    }
}

/// CR 715.3a + CR 715.3d: An independent instant/sorcery permission evaluates
/// the chosen Adventure characteristics; the earned normal-play grant cannot
/// lend its grantee or authorize that Adventure.
#[test]
fn resolving_adventure_filtered_independent_permission_uses_the_selected_face_in_state() {
    use engine::game::casting::{
        can_cast_adventure_face_now, can_cast_object_now, legal_target_slots_for_castable_spell,
    };
    use engine::types::ability::{CardPlayMode, CastingPermission, PlayFromExileProvenance};
    use engine::types::game_state::{CastOfferKind, CastingPermissionIndex};
    use engine::types::identifiers::{ObjectIncarnationRef, TrackedSetId};
    use engine::types::statics::CastFrequency;
    for independent_first in [false, true] {
        let (mut runner, hearth, silence) = paid_whole_stoke_authority_scene();
        let object = &runner.state().objects[&hearth];
        let original_pin = ObjectIncarnationRef::from_object(object);
        let earned = CastingPermission::resolved_adventure(P0, original_pin);
        assert_eq!(object.casting_permissions, vec![earned.clone()]);
        assert_eq!(object.name, "Hearth Elemental");
        for _ in 0..6 {
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::Red,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        assert!(can_cast_adventure_face_now(
            runner.state(),
            P0,
            hearth,
            true
        ));
        assert!(!can_cast_adventure_face_now(
            runner.state(),
            P0,
            hearth,
            false
        ));
        let p0_hand = runner.state().players[0].hand.clone();
        runner.advance_to_phase(Phase::End);
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, P1);
        let filtered = CastingPermission::PlayFromExile {
            provenance: PlayFromExileProvenance::Impulse,
            mode: CardPlayMode::Cast,
            duration: Duration::UntilEndOfTurn,
            granted_to: P1,
            frequency: CastFrequency::Unlimited,
            source_id: Some(silence),
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::AnyOf(vec![
                    TypeFilter::Instant,
                    TypeFilter::Sorcery,
                ])],
                controller: None,
                properties: vec![],
            })),
            single_use_group: Some(TrackedSetId(9021)),
            single_use: true,
            cast_cost_modifier: None,
            alt_ability_cost: None,
            without_paying_mana_cost: false,
            land_enter_tapped: engine::types::zones::EtbTapState::Unspecified,
        };
        runner
            .state_mut()
            .objects
            .get_mut(&hearth)
            .unwrap()
            .casting_permissions = if independent_first {
            vec![filtered.clone(), earned.clone()]
        } else {
            vec![earned.clone(), filtered.clone()]
        };
        for _ in 0..6 {
            runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
                ManaType::Red,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        assert!(!can_cast_adventure_face_now(
            runner.state(),
            P1,
            hearth,
            true
        ));
        assert!(!can_cast_adventure_face_now(
            runner.state(),
            P0,
            hearth,
            false
        ));
        assert!(can_cast_adventure_face_now(
            runner.state(),
            P1,
            hearth,
            false
        ));
        let index = usize::from(!independent_first);
        let mut wrong_grantee = runner.state().clone();
        let CastingPermission::PlayFromExile { granted_to, .. } = &mut wrong_grantee
            .objects
            .get_mut(&hearth)
            .unwrap()
            .casting_permissions[index]
        else {
            panic!("the independent filtered authority keeps its exact slot");
        };
        *granted_to = P0;
        assert!(!can_cast_adventure_face_now(
            &wrong_grantee,
            P1,
            hearth,
            false
        ));
        assert!(!can_cast_object_now(&wrong_grantee, P1, hearth));
        assert!(
            can_cast_object_now(runner.state(), P1, hearth),
            "general castability must agree with the independently legal Adventure"
        );
        assert!(legal_target_slots_for_castable_spell(runner.state(), hearth).is_empty());
        let cast = GameAction::CastSpell {
            object_id: hearth,
            card_id: runner.state().objects[&hearth].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        };
        assert!(engine::ai_support::legal_actions(runner.state()).iter().any(|action|
            matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == hearth)));
        runner.act(cast).unwrap();
        assert!(matches!(runner.state().waiting_for, WaitingFor::CastOffer {
            player: P1, kind: CastOfferKind::Adventure { object_id, .. }
        } if object_id == hearth));
        assert_eq!(
            runner.state().objects[&hearth].name,
            "Hearth Elemental",
            "read-only projection must not swap the authoritative exiled card"
        );
        for restored in adventure_authority_save_representations(runner.state()) {
            let mut resumed = GameRunner::from_state(restored);
            let actions = engine::ai_support::legal_actions(resumed.state());
            assert!(actions.contains(&GameAction::ChooseAdventureFace { creature: false }));
            assert!(!actions.contains(&GameAction::ChooseAdventureFace { creature: true }));
            let mut forbidden = GameRunner::from_state(resumed.state().clone());
            assert!(forbidden
                .act(GameAction::ChooseAdventureFace { creature: true })
                .is_err());
            resumed
                .act(GameAction::ChooseAdventureFace { creature: false })
                .unwrap();
            assert!(matches!(
                resumed.state().waiting_for,
                WaitingFor::ManaPayment { player: P1, .. }
            ));
            let pending = resumed.state().pending_cast.as_ref().unwrap();
            assert_eq!(
                pending.casting_permission_index,
                Some(CastingPermissionIndex(index))
            );
            assert_eq!(
                pending.cost,
                ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Red],
                }
            );
            assert_eq!(
                resumed.state().objects[&hearth].casting_permissions[index],
                filtered
            );
            assert_eq!(resumed.state().players[1].mana_pool.total(), 6);
            let discarded = resumed.state().players[1].hand.clone();
            let library: Vec<_> = resumed.state().players[1].library.iter().copied().collect();
            assert_eq!(
                library.len(),
                2,
                "the next-turn draw leaves two cards for Stoke"
            );
            resumed.act(GameAction::PassPriority).unwrap();
            assert_eq!(resumed.state().objects[&hearth].name, "Stoke Genius");
            assert_eq!(resumed.state().objects[&hearth].mana_cost.mana_value(), 2);
            assert!(matches!(
                resumed.state().stack.back().unwrap().kind,
                StackEntryKind::Spell {
                    actual_mana_spent: 2,
                    ..
                }
            ));
            assert_eq!(resumed.state().players[1].mana_pool.total(), 4);
            let mut events = Vec::new();
            for _ in 0..4 {
                if resumed.state().objects[&hearth].zone == Zone::Exile {
                    break;
                }
                events.extend(resumed.act(GameAction::PassPriority).unwrap().events);
            }
            for id in &discarded {
                assert_eq!(resumed.state().objects[id].zone, Zone::Graveyard);
            }
            assert_eq!(
                resumed.state().players[1]
                    .hand
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                library[..2].iter().copied().collect()
            );
            assert!(resumed.state().players[1].library.is_empty());
            assert_eq!(resumed.state().players[0].hand, p0_hand);
            let first_draw = events
                .iter()
                .position(|event| matches!(event, GameEvent::CardDrawn { player_id: P1, .. }))
                .unwrap();
            assert!(!discarded.is_empty());
            for id in &discarded {
                assert!(events.iter().position(|event|
                    matches!(event, GameEvent::Discarded { object_id, .. } if object_id == id)).unwrap() < first_draw);
            }
            let object = &resumed.state().objects[&hearth];
            assert_eq!(object.owner, P0);
            assert_eq!(object.zone, Zone::Exile);
            assert_eq!(object.name, "Hearth Elemental");
            assert_ne!(ObjectIncarnationRef::from_object(object), original_pin);
            assert_eq!(
                object.casting_permissions,
                vec![CastingPermission::resolved_adventure(
                    P1,
                    ObjectIncarnationRef::from_object(object),
                )]
            );
            assert!(matches!(
                resumed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                resumed.state().stack.is_empty() && resumed.state().resolution_stack.is_empty()
            );
            assert!(
                resumed.state().pending_trigger.is_none()
                    && resumed.state().deferred_triggers.is_empty()
                    && resumed.state().pending_trigger_order.is_none()
            );
            assert!(resumed.state().owed_spell_final_part().is_none());
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn resolving_adventure_zone_authority_preserves_timing_stack_and_silence_prohibition() {
    let (mut runner, hearth, silence) = paid_whole_stoke_authority_scene();
    let earned = runner.state().objects[&hearth].casting_permissions.clone();
    let normal_cost = ManaCost::Cost {
        generic: 5,
        shards: vec![ManaCostShard::Red],
    };
    for _ in 0..6 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    assert!(engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        hearth
    ));
    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert_eq!(runner.state().active_player, P0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(runner.state().stack.is_empty());
    // CR 106.4 + CR 500.5: the phase boundary drains ordinary mana, so
    // replenish the full normal cost before testing timing independently.
    assert!(runner.state().players[0].mana_pool.is_empty());
    for _ in 0..6 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Red),
        6
    );
    assert_eq!(
        engine::game::casting::display_spell_cost(runner.state(), P0, hearth),
        Some(normal_cost.clone())
    );
    assert_eq!(runner.state().objects[&hearth].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&hearth].casting_permissions, earned);
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        hearth
    ));
    let combat_forbidden = GameAction::CastSpell {
        object_id: hearth,
        card_id: runner.state().objects[&hearth].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(!engine::ai_support::legal_actions(runner.state()).contains(&combat_forbidden));
    assert!(runner.act(combat_forbidden).is_err());
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    runner.advance_to_phase(Phase::PostCombatMain);
    for _ in 0..6 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    assert!(engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        hearth
    ));
    runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
        ManaType::White,
        ObjectId(0),
        false,
        vec![],
    ));
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner.cast(silence).commit();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(!runner.state().stack.is_empty());
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        hearth
    ));
    runner.resolve_top();
    assert_eq!(runner.state().objects[&silence].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        hearth
    ));
    let forbidden = GameAction::CastSpell {
        object_id: hearth,
        card_id: runner.state().objects[&hearth].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(!engine::ai_support::legal_actions(runner.state()).contains(&forbidden));
    assert!(runner.act(forbidden).is_err());
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    // CR 117.3d: pass the active player's priority so timing, rather than
    // lack of priority, is the reason P0 cannot cast the normal permanent.
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert!(runner.state().stack.is_empty());
    // CR 106.4 + CR 500.5: end-step and turn progression also drain the
    // pool; fund the unchanged 5R normal cost again in this priority window.
    assert!(runner.state().players[0].mana_pool.is_empty());
    for _ in 0..6 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Red),
        6
    );
    assert_eq!(
        engine::game::casting::display_spell_cost(runner.state(), P0, hearth),
        Some(normal_cost.clone())
    );
    assert_eq!(runner.state().objects[&hearth].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&hearth].casting_permissions, earned);
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        hearth
    ));
    let opponent_turn_forbidden = GameAction::CastSpell {
        object_id: hearth,
        card_id: runner.state().objects[&hearth].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(!engine::ai_support::legal_actions(runner.state()).contains(&opponent_turn_forbidden));
    assert!(runner.act(opponent_turn_forbidden).is_err());
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    runner.act(GameAction::PassPriority).unwrap();
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    for _ in 0..6 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    assert_eq!(
        engine::game::casting::effective_spell_cost(runner.state(), P0, hearth),
        Some(normal_cost)
    );
    assert!(engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        hearth
    ));
    runner
        .cast(hearth)
        .resolve()
        .assert_zone(&[hearth], Zone::Battlefield);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}

#[test]
fn resolving_adventure_foreign_fertile_search_uses_the_current_controller_and_restores_its_response(
) {
    use engine::types::ability::CastingPermission;
    use engine::types::mana::ManaColor;
    for fail_to_find in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Controller reserve A", "Controller reserve B"]);
        scenario.with_library_top(P1, &["Owner reserve A", "Owner reserve B"]);
        let forest = scenario.add_basic_land(P0, ManaColor::Green);
        let island = scenario.add_basic_land(P0, ManaColor::Blue);
        let giant=scenario.add_creature_to_hand(P1,"Beanstalk Giant",0,0).with_subtypes(vec!["Giant"])
            .with_mana_cost(ManaCost::Cost {generic:6,shards:vec![ManaCostShard::Green]})
            .from_oracle_text("Beanstalk Giant's power and toughness are each equal to the number of lands you control.").id();
        let commandeer = scenario
            .add_spell_to_hand_from_oracle(P0, "Commandeer", true, ADVENTURE_COMMANDEER)
            .with_mana_cost(blue_cost(5, 2))
            .id();
        scenario.with_mana_pool(
            P1,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
                .collect(),
        );
        scenario.with_mana_pool(
            P0,
            (0..7)
                .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                .collect(),
        );
        let parsed=engine::parser::parse_oracle_text("Search your library for a basic land card, put it onto the battlefield, then shuffle. (Then exile this card. You may cast the creature later from exile.)","Fertile Footsteps",&[],&["Sorcery".into()],&["Adventure".into()]);
        let mut runner = scenario.build();
        for land in [forest, island] {
            engine::game::zones::move_to_library_at_index(
                runner.state_mut(),
                land,
                Some(0),
                &mut vec![],
            );
        }
        runner
            .state_mut()
            .objects
            .get_mut(&giant)
            .unwrap()
            .back_face = Some(BackFaceData {
            name: "Fertile Footsteps".into(),
            layout_kind: Some(LayoutKind::Adventure),
            mana_cost: ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            },
            card_types: engine::types::card_type::CardType {
                core_types: vec![engine::types::card_type::CoreType::Sorcery],
                subtypes: vec!["Adventure".into()],
                ..Default::default()
            },
            abilities: parsed.abilities,
            trigger_definitions: parsed.triggers.into(),
            static_definitions: parsed.statics.into(),
            replacement_definitions: parsed.replacements.into(),
            ..Default::default()
        });
        runner.state_mut().active_player = P1;
        runner.state_mut().priority_player = P1;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
        runner.cast(giant).adventure_face(false).commit();
        runner.act(GameAction::PassPriority).unwrap();
        runner
            .cast(commandeer)
            .decline_optional()
            .target_object(giant)
            .commit();
        assert!(matches!(
            runner.state().stack.back().unwrap().kind,
            StackEntryKind::Spell {
                actual_mana_spent: 7,
                ..
            }
        ));
        let mut changed = false;
        for _ in 0..12 {
            if runner.state().objects[&commandeer].zone == Zone::Graveyard
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::OptionalEffectChoice { player: P0, .. } => {
                    GameAction::DecideOptionalEffect { accept: false }
                }
                ref other => panic!("unexpected Commandeer prompt {other:?}"),
            };
            changed|=runner.act(action).unwrap().events.iter().any(|event|matches!(event,GameEvent::ControllerChanged {object_id,old_controller:P1,new_controller:P0} if *object_id==giant));
        }
        assert!(changed);
        runner.resolve_top();
        let WaitingFor::SearchChoice {
            player,
            cards,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real Fertile must pause on its controller's two basic candidates");
        };
        assert_eq!(*player, P0);
        assert_eq!(*count, 1);
        assert_eq!(cards.len(), 2);
        assert!(cards.contains(&forest) && cards.contains(&island));
        let nonfirst = cards[1];
        let chosen = if fail_to_find { vec![] } else { vec![nonfirst] };
        let expected_waiting = runner.state().waiting_for.clone();
        let mut old_unattempted = serde_json::to_value(runner.state()).unwrap();
        let old_debt = old_unattempted["resolving_spell_final_part"]
            .as_object_mut()
            .unwrap();
        old_debt.remove("stack_incarnation");
        old_debt.remove("pending_adventure_exile");
        let old_unattempted: GameState = serde_json::from_value(old_unattempted).unwrap();
        for restored in adventure_authority_save_representations(runner.state())
            .into_iter()
            .chain(std::iter::once(old_unattempted))
        {
            assert_eq!(restored.waiting_for, expected_waiting);
            let mut resumed = GameRunner::from_state(restored);
            let response = resumed
                .act(GameAction::SelectCards {
                    cards: chosen.clone(),
                })
                .unwrap();
            assert_eq!(resumed.state().objects[&giant].owner, P1);
            assert_eq!(resumed.state().objects[&giant].zone, Zone::Exile);
            let object = &resumed.state().objects[&giant];
            assert!(object
                .casting_permissions
                .contains(&CastingPermission::resolved_adventure(
                    P0,
                    engine::types::identifiers::ObjectIncarnationRef::from_object(object)
                )));
            assert_eq!(
                resumed.state().objects[&nonfirst].zone,
                if fail_to_find {
                    Zone::Library
                } else {
                    Zone::Battlefield
                }
            );
            assert_eq!(response.events.iter().filter(|event|matches!(event,GameEvent::StackResolved {object_id} if *object_id==giant)).count(),1);
            assert!(resumed.state().owed_spell_final_part().is_none());
            assert!(
                resumed.state().stack.is_empty() && resumed.state().resolution_stack.is_empty()
            );
            assert!(matches!(
                resumed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
}

fn stoke_genius_face() -> BackFaceData {
    let parsed = engine::parser::parse_oracle_text(
        STOKE,
        "Stoke Genius",
        &[],
        &["Sorcery".into()],
        &["Adventure".into()],
    );
    assert!(parsed.parse_warnings.is_empty());
    assert_eq!(parsed.abilities.len(), 1);
    assert!(matches!(
        parsed.abilities[0].effect.as_ref(),
        Effect::Discard {
            count: QuantityExpr::Ref {
                qty: QuantityRef::HandSize { .. }
            },
            ..
        }
    ));
    assert!(matches!(
        parsed.abilities[0]
            .sub_ability
            .as_ref()
            .unwrap()
            .effect
            .as_ref(),
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            ..
        }
    ));
    BackFaceData {
        name: "Stoke Genius".into(),
        layout_kind: Some(LayoutKind::Adventure),
        mana_cost: ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        },
        card_types: engine::types::card_type::CardType {
            core_types: vec![engine::types::card_type::CoreType::Sorcery],
            subtypes: vec!["Adventure".into()],
            ..Default::default()
        },
        abilities: parsed.abilities,
        trigger_definitions: parsed.triggers.into(),
        static_definitions: parsed.statics.into(),
        replacement_definitions: parsed.replacements.into(),
        ..Default::default()
    }
}

const ADVENTURE_COMMANDEER: &str = "You may exile two blue cards from your hand rather than pay this spell's mana cost.\nGain control of target noncreature spell. You may choose new targets for it. (If that spell is an artifact, enchantment, or planeswalker, the permanent enters under your control.)";
const ADVENTURE_STOLEN_GOODS: &str = "Target opponent exiles cards from the top of their library until they exile a nonland card. Until end of turn, you may cast that card without paying its mana cost.";

fn adventure_current_save_payloads(state: &GameState) -> [Value; 3] {
    use engine::types::game_state::PersistedGameState;
    use engine::types::resolution::ResolutionStateWire;
    [
        serde_json::to_value(state).unwrap(),
        serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap(),
        serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap(),
    ]
}

fn restore_adventure_current_save(route: usize, payload: Value) -> Result<GameState, String> {
    use engine::types::game_state::PersistedGameState;
    use engine::types::resolution::ResolutionStateWire;
    match route {
        0 => serde_json::from_value(payload).map_err(|error| error.to_string()),
        1 => serde_json::from_value::<ResolutionStateWire>(payload)
            .map(ResolutionStateWire::into_game_state)
            .map_err(|error| error.to_string()),
        2 => serde_json::from_value::<PersistedGameState>(payload)
            .map_err(|error| error.to_string())?
            .into_game_state()
            .map_err(|error| error.to_string()),
        _ => unreachable!("the three existing full-save routes"),
    }
}

fn assert_public_adventure_grant_current_and_legacy_saves(
    state: &GameState,
    spell: ObjectId,
    grantee: engine::types::player::PlayerId,
    normal_payment: u32,
) {
    use engine::types::ability::CastingPermission;
    use engine::types::game_state::PersistedGameState;
    use engine::types::resolved_commands::ResolvedRulesCommand;
    let departures: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::ZoneChange(command))
                if command.object.object_id == spell
                    && command.from == Zone::Stack
                    && command.to == Zone::Exile
                    && command.resulting_incarnation == state.objects[&spell].incarnation =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        departures.len(),
        1,
        "the real successful Adventure exit is retained"
    );
    let departure = departures[0];
    assert!(
        !state
            .departed_stack_spells
            .get(&spell)
            .is_some_and(|departures| departures.contains_key(&departure.object.incarnation)),
        "successful Adventure resolution is not a nonresolving Stack removal"
    );
    let context = departure
        .zone_change_record
        .trigger_source_context
        .as_ref()
        .unwrap();
    assert_eq!(context.identity.reference, departure.object);
    assert_eq!(context.identity.expected_zone, Zone::Stack);
    assert_eq!(context.lki.controller, grantee);
    assert!(state.resolved_rules_journal.entries().iter().any(|entry| matches!(
        &entry.command,
        Some(ResolvedRulesCommand::StackRemoval(removal))
            if removal.entry.id == spell && removal.entry.source_id == spell
                && matches!(&removal.entry.kind, StackEntryKind::Spell {
                    casting_variant: engine::types::game_state::CastingVariant::Adventure,
                    ability: Some(ability), ..
                } if ability.source_id == spell && ability.source_incarnation.is_none_or(|incarnation| incarnation == departure.object.incarnation))
    )), "the exact resolving Adventure program, rather than owner resemblance, survives");
    let expected = CastingPermission::resolved_adventure(
        grantee,
        engine::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&spell]),
    );
    for loaded in adventure_authority_save_representations(state) {
        assert!(loaded.objects[&spell]
            .casting_permissions
            .contains(&expected));
    }

    let raw = serde_json::to_value(state).unwrap();
    let current: GameState = serde_json::from_value(raw.clone()).unwrap();
    let persisted: PersistedGameState = serde_json::from_value(raw.clone()).unwrap();
    let trusted: PersistedGameState = serde_json::from_value(
        serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap(),
    )
    .unwrap();
    for loaded in [
        current,
        persisted.into_game_state().unwrap(),
        trusted.into_game_state().unwrap(),
    ] {
        assert!(loaded.objects[&spell]
            .casting_permissions
            .contains(&expected));
        assert!(
            engine::game::casting::has_potentially_authorizing_object_cast_permission(
                &loaded.objects[&spell],
                grantee
            )
        );
        let other = if grantee == P0 { P1 } else { P0 };
        assert!(
            !engine::game::casting::has_potentially_authorizing_object_cast_permission(
                &loaded.objects[&spell],
                other
            )
        );
    }
    // CR 715.3d + CR 400.7j: these full saves retain the real successful
    // Adventure's original program, departure and unchanged exile stay. BASE
    // stack::finish_spell_stack_exit wrote the unit {type: AdventureCreature};
    // zones removed it on every exile exit. Only the permission payload below
    // is changed to that actual old writer's shape; none of its proof is made up.
    for missing in [
        vec!["granted_to", "exile_incarnation"],
        vec!["granted_to"],
        vec!["exile_incarnation"],
    ] {
        for (route, mut legacy) in adventure_current_save_payloads(state)
            .into_iter()
            .enumerate()
        {
            let saved_state = if route == 2 {
                &mut legacy["state"]
            } else {
                &mut legacy
            };
            let object = &mut saved_state["objects"][spell.0.to_string()];
            let permission = object["casting_permissions"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|p| p["type"] == "AdventureCreature")
                .unwrap()
                .as_object_mut()
                .unwrap();
            for field in &missing {
                permission.remove(*field);
            }
            if missing.len() == 2 {
                assert_eq!(
                    Value::Object(permission.clone()),
                    serde_json::json!({"type": "AdventureCreature"})
                );
            }
            let loaded = restore_adventure_current_save(route, legacy).unwrap();
            assert!(loaded.objects[&spell]
                .casting_permissions
                .contains(&expected));
            assert!(
                engine::game::casting::has_potentially_authorizing_object_cast_permission(
                    &loaded.objects[&spell],
                    grantee
                )
            );
            let other = if grantee == P0 { P1 } else { P0 };
            assert!(
                !engine::game::casting::has_potentially_authorizing_object_cast_permission(
                    &loaded.objects[&spell],
                    other
                )
            );
            let mut independent = loaded.clone();
            independent
                .objects
                .get_mut(&spell)
                .unwrap()
                .casting_permissions
                .retain(|permission| {
                    !matches!(permission, CastingPermission::AdventureCreature { .. })
                });
            assert!(
                !engine::game::casting::has_potentially_authorizing_object_cast_permission(
                    &independent.objects[&spell],
                    grantee
                )
            );
            independent
                .objects
                .get_mut(&spell)
                .unwrap()
                .casting_permissions
                .push(alternative_permission(grantee, 0, 0));
            assert!(
                engine::game::casting::has_potentially_authorizing_object_cast_permission(
                    &independent.objects[&spell],
                    grantee
                )
            );
            let owner = loaded.objects[&spell].owner;
            let mut resumed = GameRunner::from_state(loaded);
            if resumed.state().active_player != grantee {
                resumed.advance_to_end_step();
                resumed.advance_to_phase(Phase::PreCombatMain);
            }
            assert_eq!(resumed.state().active_player, grantee);
            assert!(
                matches!(resumed.state().waiting_for, WaitingFor::Priority { player } if player == grantee)
            );
            assert_eq!(
                resumed.state().players[grantee.0 as usize]
                    .mana_pool
                    .total(),
                0
            );
            for _ in 0..normal_payment {
                resumed.state_mut().players[grantee.0 as usize]
                    .mana_pool
                    .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
            }
            assert!(engine::game::casting::can_cast_object_now(
                resumed.state(),
                grantee,
                spell
            ));
            assert!(!engine::game::casting::can_cast_adventure_face_now(
                resumed.state(),
                grantee,
                spell,
                false
            ));
            resumed
                .act(GameAction::CastSpell {
                    object_id: spell,
                    card_id: resumed.state().objects[&spell].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            assert!(matches!(
                resumed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!(resumed.state().objects[&spell].zone, Zone::Stack);
            assert_eq!(resumed.state().objects[&spell].name, "Hearth Elemental");
            assert!(matches!(resumed.state().stack.back().unwrap().kind,
            StackEntryKind::Spell { actual_mana_spent, casting_variant: engine::types::game_state::CastingVariant::Normal, .. }
                if actual_mana_spent == normal_payment));
            assert_eq!(
                resumed.state().players[grantee.0 as usize]
                    .mana_pool
                    .total(),
                0
            );
            resumed.resolve_top();
            let creature = &resumed.state().objects[&spell];
            assert_eq!(creature.zone, Zone::Battlefield);
            assert_eq!((creature.owner, creature.controller), (owner, grantee));
            assert_eq!((creature.power, creature.toughness), (Some(4), Some(5)));
            assert!(creature.casting_permissions.is_empty());
            assert!(matches!(
                resumed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                resumed.state().stack.is_empty() && resumed.state().resolution_stack.is_empty()
            );
        }
    }
}

#[test]
fn resolving_adventure_countered_directly_to_exile_cannot_recover_legacy_grant() {
    use engine::types::resolved_commands::ResolvedRulesCommand;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hearth = scenario
        .add_creature_to_hand(P0, "Hearth Elemental", 4, 5)
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::Red],
        })
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text(HEARTH)
        .id();
    let untouched = scenario
        .add_creature_to_hand(P0, "Unspent Stoke hand", 1, 1)
        .id();
    scenario.with_library_top(
        P0,
        &[
            "Counter exile draw A",
            "Counter exile draw B",
            "Counter exile reserve",
        ],
    );
    let dissipate = scenario.add_spell_to_hand_from_oracle(P1, "Dissipate", true,
        "Counter target spell. If that spell is countered this way, exile it instead of putting it into its owner's graveyard.")
        .with_mana_cost(blue_cost(1, 2)).id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_mana_pool(
        P1,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&hearth)
        .unwrap()
        .back_face = Some(stoke_genius_face());
    let library_before = runner.state().players[0].library.clone();
    let committed = runner.cast(hearth).adventure_face(false).commit();
    assert!(matches!(
        committed.state().stack.back().unwrap().kind,
        StackEntryKind::Spell {
            actual_mana_spent: 2,
            ..
        }
    ));
    let stack_birth = committed.state().objects[&hearth].incarnation;
    drop(committed);
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    let committed = runner.cast(dissipate).target_object(hearth).commit();
    assert!(matches!(
        committed.state().stack.back().unwrap().kind,
        StackEntryKind::Spell {
            actual_mana_spent: 3,
            ..
        }
    ));
    let outcome = committed.resolve();
    outcome.assert_zone(&[hearth], Zone::Exile);
    outcome.assert_zone(&[dissipate], Zone::Graveyard);
    outcome.assert_zone(&[untouched], Zone::Hand);
    assert_eq!(outcome.state().players[0].library, library_before);
    assert!(!outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::Discarded { player_id: P0, .. } | GameEvent::CardDrawn { player_id: P0, .. }
    )));
    assert!(outcome.state().objects[&hearth]
        .casting_permissions
        .is_empty());
    let departed = &outcome.state().departed_stack_spells[&hearth][&stack_birth];
    assert_eq!(departed.object.incarnation, stack_birth);
    assert!(matches!(
        departed.entry.kind,
        StackEntryKind::Spell {
            casting_variant: engine::types::game_state::CastingVariant::Adventure,
            ..
        }
    ));
    assert!(outcome.state().resolved_rules_journal.entries().iter().any(|entry| matches!(
        &entry.command, Some(ResolvedRulesCommand::ZoneChange(command))
            if command.object.object_id == hearth && command.object.incarnation == stack_birth
                && command.from == Zone::Stack && command.to == Zone::Exile
                && command.resulting_incarnation == outcome.state().objects[&hearth].incarnation
    )), "the hostile case really reaches the same Stack-to-Exile proof boundary");
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
    // CR 715.3d: this direct exile was a counter, not successful Adventure
    // resolution. Its retained nonresolving Stack departure defeats a grafted
    // old unit permission even though original program and exact stay survive.
    for (route, payload) in adventure_current_save_payloads(outcome.state())
        .into_iter()
        .enumerate()
    {
        let loaded = restore_adventure_current_save(route, payload.clone()).unwrap();
        assert!(loaded.objects[&hearth].casting_permissions.is_empty());
        let mut forged = payload;
        let state = if route == 2 {
            &mut forged["state"]
        } else {
            &mut forged
        };
        state["objects"][hearth.0.to_string()]["casting_permissions"] =
            serde_json::json!([{"type":"AdventureCreature"}]);
        let before = forged.clone();
        let error = restore_adventure_current_save(route, forged.clone()).unwrap_err();
        assert!(
            error.contains("Adventure permission"),
            "direct counter-exile route {route}: {error}"
        );
        assert_eq!(forged, before);
    }
}

#[test]
fn resolving_adventure_legacy_success_witness_rejects_ambiguous_and_expired_authority() {
    use engine::types::ability::CastingPermission;
    let (runner, hearth, _) = paid_whole_stoke_authority_scene();
    assert_eq!(runner.state().objects[&hearth].zone, Zone::Exile);
    assert_eq!(runner.state().players[0].hand.len(), 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_public_adventure_grant_current_and_legacy_saves(runner.state(), hearth, P0, 6);

    for corruption in [
        "wrong_grantee",
        "stale_birth",
        "no_success",
        "no_original_program",
        "ambiguous_controller",
    ] {
        for (route, mut payload) in adventure_current_save_payloads(runner.state())
            .into_iter()
            .enumerate()
        {
            let state = if route == 2 {
                &mut payload["state"]
            } else {
                &mut payload
            };
            let permission = state["objects"][hearth.0.to_string()]["casting_permissions"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|permission| permission["type"] == "AdventureCreature")
                .unwrap();
            match corruption {
                "wrong_grantee" => permission["granted_to"] = serde_json::json!(P1),
                "stale_birth" => {
                    permission["exile_incarnation"] =
                        serde_json::json!(runner.state().objects[&hearth].incarnation + 1)
                }
                _ => *permission = serde_json::json!({"type": "AdventureCreature"}),
            }
            if !matches!(corruption, "wrong_grantee" | "stale_birth") {
                let next_ordinal = state["resolved_rules_journal"]["next_command_ordinal"]
                    .as_u64()
                    .unwrap();
                let entries = state["resolved_rules_journal"]["entries"]
                    .as_array_mut()
                    .unwrap();
                let departure_index = entries
                    .iter()
                    .position(|entry| {
                        let command = &entry["command"]["ZoneChange"];
                        command["object"]["object_id"] == serde_json::json!(hearth)
                            && command["from"] == "Stack"
                            && command["to"] == "Exile"
                    })
                    .unwrap();
                let removal_index = entries
                    .iter()
                    .position(|entry| {
                        let original = &entry["command"]["StackRemoval"]["entry"];
                        original["id"] == serde_json::json!(hearth)
                            && original["kind"]["data"]["casting_variant"] == "Adventure"
                    })
                    .unwrap();
                match corruption {
                    "no_success" => {
                        entries[departure_index]["command"] = Value::Null;
                    }
                    "no_original_program" => {
                        for entry in entries.iter_mut() {
                            for kind in ["StackPush", "StackRemoval", "StackEntryFinalize"] {
                                if entry["command"][kind]["entry"]["id"]
                                    == serde_json::json!(hearth)
                                {
                                    entry["command"][kind]["entry"]["kind"]["data"]
                                        ["casting_variant"] = serde_json::json!("Normal");
                                }
                            }
                        }
                        state["resolving_stack_entry"] = Value::Null;
                    }
                    "ambiguous_controller" => {
                        let mut competing_exit = entries[departure_index].clone();
                        competing_exit["ordinal"] = serde_json::json!(next_ordinal);
                        competing_exit["command"]["ZoneChange"]["zone_change_record"]
                            ["controller"] = serde_json::json!(P1);
                        competing_exit["command"]["ZoneChange"]["zone_change_record"]
                            ["trigger_source_context"]["lki"]["controller"] = serde_json::json!(P1);
                        let mut competing_owner = entries[removal_index].clone();
                        competing_owner["ordinal"] = serde_json::json!(next_ordinal + 1);
                        competing_owner["command"]["StackRemoval"]["entry"]["controller"] =
                            serde_json::json!(P1);
                        competing_owner["command"]["StackRemoval"]["entry"]["kind"]["data"]
                            ["ability"]["controller"] = serde_json::json!(P1);
                        // Keep every actual original command and its ordinal:
                        // this sibling adds a competing complete authority.
                        entries.push(competing_exit);
                        entries.push(competing_owner);
                        state["resolved_rules_journal"]["next_command_ordinal"] =
                            serde_json::json!(next_ordinal + 2);
                    }
                    _ => unreachable!(),
                }
            }
            let before = payload.clone();
            let error = restore_adventure_current_save(route, payload.clone()).unwrap_err();
            assert!(
                error.contains("Adventure permission"),
                "{corruption} route {route}: {error}"
            );
            assert_eq!(payload, before, "a refused full save is kept intact");
        }
    }

    // CR 715.3d: the earned permission ends on the first exile exit, even
    // when the same card immediately returns to exile (CR 400.7).
    let original_birth = runner.state().objects[&hearth].incarnation;
    let mut expired = runner.state().clone();
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(&mut expired, hearth, Zone::Hand, &mut events);
    engine::game::zones::move_to_zone(&mut expired, hearth, Zone::Exile, &mut events);
    assert!(expired.objects[&hearth].incarnation > original_birth);
    assert!(expired.objects[&hearth].casting_permissions.is_empty());
    assert!(events.iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Exile), to: Zone::Hand, .. } if *object_id == hearth)));
    for (route, payload) in adventure_current_save_payloads(&expired)
        .into_iter()
        .enumerate()
    {
        let loaded = restore_adventure_current_save(route, payload.clone()).unwrap();
        assert!(loaded.objects[&hearth].casting_permissions.is_empty());
        assert!(
            !engine::game::casting::has_potentially_authorizing_object_cast_permission(
                &loaded.objects[&hearth],
                P0
            )
        );
        let mut stale = payload;
        let state = if route == 2 {
            &mut stale["state"]
        } else {
            &mut stale
        };
        state["objects"][hearth.0.to_string()]["casting_permissions"] =
            serde_json::json!([{"type": "AdventureCreature"}]);
        let before = stale.clone();
        let error = restore_adventure_current_save(route, stale.clone()).unwrap_err();
        assert!(
            error.contains("Adventure permission"),
            "away/back route {route}: {error}"
        );
        assert_eq!(stale, before);
        let mut independent = loaded;
        independent
            .objects
            .get_mut(&hearth)
            .unwrap()
            .casting_permissions
            .push(alternative_permission(P0, 0, 0));
        assert!(
            engine::game::casting::has_potentially_authorizing_object_cast_permission(
                &independent.objects[&hearth],
                P0
            )
        );
        assert!(!independent.objects[&hearth]
            .casting_permissions
            .iter()
            .any(|permission| matches!(permission, CastingPermission::AdventureCreature { .. })));
    }
}

#[test]
fn resolving_adventure_public_commandeer_binds_resolving_controller_and_paid_cost() {
    use engine::types::ability::CastingPermission;
    use engine::types::mana::ManaColor;
    for relative_population in [false, true] {
        for (owner, grantee) in [(P1, P0), (P0, P1)] {
            for alternative in [false, true] {
                for blue_count in if alternative { vec![0, 1, 2] } else { vec![2] } {
                    let mut scenario = GameScenario::new();
                    scenario.at_phase(Phase::PreCombatMain);
                    scenario.with_library_top(
                        grantee,
                        &[
                            "grantee draw A",
                            "grantee draw B",
                            "grantee reserve",
                            "grantee reserve 2",
                        ],
                    );
                    scenario.with_library_top(
                        owner,
                        &["owner reserve A", "owner reserve B", "owner reserve C"],
                    );
                    let hearth = scenario
                        .add_creature_to_hand(owner, "Hearth Elemental", 4, 5)
                        .with_mana_cost(ManaCost::Cost {
                            generic: 5,
                            shards: vec![ManaCostShard::Red],
                        })
                        .with_subtypes(vec!["Elemental"])
                        .from_oracle_text(HEARTH)
                        .id();
                    let owner_hand = scenario
                        .add_creature_to_hand(owner, "Owner's untouched creature", 1, 1)
                        .id();
                    let first = scenario
                        .add_creature_to_hand(grantee, "First blue payment", 1, 1)
                        .with_color(vec![if blue_count >= 1 {
                            ManaColor::Blue
                        } else {
                            ManaColor::Red
                        }])
                        .id();
                    let second = scenario
                        .add_creature_to_hand(grantee, "Second blue payment", 1, 1)
                        .with_color(vec![if blue_count >= 2 {
                            ManaColor::Blue
                        } else {
                            ManaColor::Red
                        }])
                        .id();
                    let commandeer = scenario
                        .add_spell_to_hand_from_oracle(
                            grantee,
                            "Commandeer",
                            true,
                            ADVENTURE_COMMANDEER,
                        )
                        .with_mana_cost(blue_cost(5, 2))
                        .id();
                    scenario.with_mana_pool(
                        owner,
                        (0..2)
                            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                            .collect(),
                    );
                    scenario.with_mana_pool(
                        grantee,
                        (0..if alternative { 0 } else { 7 })
                            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                            .collect(),
                    );
                    let preexisting = if relative_population {
                        vec![
                            scenario
                                .add_spell_to_graveyard(grantee, "Integrated own Instant", true)
                                .id(),
                            scenario
                                .add_spell_to_graveyard(grantee, "Integrated own Sorcery", false)
                                .id(),
                            scenario
                                .add_creature_to_graveyard(
                                    grantee,
                                    "Integrated own adventurer",
                                    1,
                                    1,
                                )
                                .id(),
                        ]
                    } else {
                        vec![]
                    };
                    let unrelated = scenario
                        .add_creature_to_exile(grantee, "Unrelated exiled Hearth", 4, 5)
                        .from_oracle_text(HEARTH)
                        .id();
                    let mut runner = scenario.build();
                    if let Some(id) = preexisting.last() {
                        runner.state_mut().objects.get_mut(id).unwrap().back_face =
                            Some(stoke_genius_face());
                    }
                    runner
                        .state_mut()
                        .objects
                        .get_mut(&unrelated)
                        .unwrap()
                        .back_face = Some(stoke_genius_face());
                    runner.state_mut().active_player = owner;
                    runner.state_mut().priority_player = owner;
                    runner.state_mut().waiting_for = WaitingFor::Priority { player: owner };
                    runner
                        .state_mut()
                        .objects
                        .get_mut(&hearth)
                        .unwrap()
                        .back_face = Some(stoke_genius_face());
                    runner.cast(hearth).adventure_face(false).commit();
                    assert_eq!(runner.state().objects[&hearth].owner, owner);
                    assert_eq!(runner.state().objects[&hearth].controller, owner);
                    runner.act(GameAction::PassPriority).unwrap();
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { player } if player == grantee
                    ));
                    if alternative && blue_count < 2 {
                        let cast = GameAction::CastSpell {
                            object_id: commandeer,
                            card_id: runner.state().objects[&commandeer].card_id,
                            targets: vec![],
                            payment_mode: CastPaymentMode::Auto,
                        };
                        assert!(!engine::game::casting::can_cast_object_now(
                            runner.state(),
                            grantee,
                            commandeer
                        ));
                        assert!(!engine::ai_support::legal_actions(runner.state()).contains(&cast));
                        assert!(runner.act(cast).is_err());
                        assert_eq!(runner.state().objects[&commandeer].zone, Zone::Hand);
                        assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
                        assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
                        assert_eq!(runner.state().objects[&hearth].controller, owner);
                        assert_eq!(
                            runner.state().players[grantee.0 as usize].mana_pool.total(),
                            0
                        );
                        runner.resolve_top();
                        let object = &runner.state().objects[&hearth];
                        assert_eq!(object.zone, Zone::Exile);
                        assert!(object.casting_permissions.contains(
                            &CastingPermission::resolved_adventure(
                                owner,
                                engine::types::identifiers::ObjectIncarnationRef::from_object(
                                    object
                                )
                            )
                        ));
                        assert!(matches!(
                            runner.state().waiting_for,
                            WaitingFor::Priority { .. }
                        ));
                        assert!(
                            runner.state().stack.is_empty()
                                && runner.state().resolution_stack.is_empty()
                        );
                        continue;
                    }
                    runner
                        .act(GameAction::CastSpell {
                            object_id: commandeer,
                            card_id: runner.state().objects[&commandeer].card_id,
                            targets: vec![],
                            payment_mode: CastPaymentMode::Auto,
                        })
                        .unwrap();
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::OptionalCostChoice { player, .. } if player == grantee
                    ));
                    runner
                        .act(GameAction::DecideOptionalCost { pay: alternative })
                        .unwrap();
                    if alternative {
                        let WaitingFor::PayCost {
                            player,
                            choices,
                            count,
                            ..
                        } = &runner.state().waiting_for
                        else {
                            panic!("the printed alternative must choose its two blue cards");
                        };
                        assert_eq!(*player, grantee);
                        assert_eq!(*count, 2);
                        assert!(choices.contains(&first) && choices.contains(&second));
                        assert!(
                            !choices.contains(&commandeer),
                            "Commandeer is already being announced and cannot pay for itself"
                        );
                        let original_prompt = runner.state().waiting_for.clone();
                        for invalid in [
                            vec![first],
                            vec![first, first],
                            vec![commandeer, first],
                            vec![owner_hand, first],
                        ] {
                            assert!(runner
                                .act(GameAction::SelectCards { cards: invalid })
                                .is_err());
                            assert_eq!(runner.state().waiting_for, original_prompt);
                            assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
                            assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
                        }
                        runner
                            .act(GameAction::SelectCards {
                                cards: vec![second, first],
                            })
                            .unwrap();
                    }
                    let WaitingFor::TargetSelection {
                        player,
                        target_slots,
                        selection,
                        ..
                    } = &runner.state().waiting_for
                    else {
                        panic!("Commandeer must choose the actual noncreature Adventure spell");
                    };
                    assert_eq!(*player, grantee);
                    assert!(target_slots[selection.current_slot]
                        .legal_targets
                        .contains(&engine::types::ability::TargetRef::Object(hearth)));
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(engine::types::ability::TargetRef::Object(hearth)),
                        })
                        .unwrap();
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        matches!(runner.state().stack.back().unwrap().kind, StackEntryKind::Spell { actual_mana_spent, .. } if actual_mana_spent == if alternative { 0 } else { 7 })
                    );
                    if alternative {
                        assert_eq!(runner.state().objects[&first].zone, Zone::Exile);
                        assert_eq!(runner.state().objects[&second].zone, Zone::Exile);
                    }
                    let mut changed = false;
                    for _ in 0..12 {
                        if runner.state().objects[&commandeer].zone == Zone::Graveyard
                            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                        {
                            break;
                        }
                        let action = match &runner.state().waiting_for {
                            WaitingFor::Priority { .. } => GameAction::PassPriority,
                            WaitingFor::OptionalEffectChoice { player, .. } => {
                                assert_eq!(*player, grantee);
                                GameAction::DecideOptionalEffect { accept: false }
                            }
                            other => panic!("unexpected Commandeer resolution prompt: {other:?}"),
                        };
                        changed |= runner.act(action).unwrap().events.iter().any(|event| matches!(event,
                GameEvent::ControllerChanged { object_id, old_controller, new_controller } if *object_id == hearth && *old_controller == owner && *new_controller == grantee));
                    }
                    assert!(
                        changed,
                        "real Commandeer must change control before Stoke resolves"
                    );
                    assert_eq!(runner.state().objects[&hearth].zone, Zone::Stack);
                    assert_eq!(runner.state().objects[&hearth].controller, grantee);
                    if relative_population {
                        assert_eq!(preexisting.len(), 3);
                        assert!(preexisting
                            .iter()
                            .all(|id| runner.state().objects[id].zone == Zone::Graveyard));
                        assert_eq!(runner.state().objects[&commandeer].zone, Zone::Graveyard);
                    }
                    let draw_ids: Vec<_> = runner.state().players[grantee.0 as usize]
                        .library
                        .iter()
                        .copied()
                        .collect();
                    let mut events = Vec::new();
                    for _ in 0..4 {
                        if runner.state().objects[&hearth].zone == Zone::Exile {
                            break;
                        }
                        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                    }
                    let object = &runner.state().objects[&hearth];
                    assert_eq!(object.zone, Zone::Exile);
                    assert_eq!(object.owner, owner);
                    assert_eq!(object.name, "Hearth Elemental");
                    assert!(object.casting_permissions.contains(
                        &CastingPermission::resolved_adventure(
                            grantee,
                            engine::types::identifiers::ObjectIncarnationRef::from_object(object)
                        )
                    ));
                    assert_eq!(runner.state().objects[&owner_hand].zone, Zone::Hand);
                    assert_eq!(
                        runner.state().players[grantee.0 as usize]
                            .hand
                            .iter()
                            .copied()
                            .collect::<std::collections::BTreeSet<_>>(),
                        draw_ids[..2].iter().copied().collect()
                    );
                    assert_eq!(runner.state().objects[&draw_ids[2]].zone, Zone::Library);
                    if !alternative {
                        let first_draw = events
                .iter()
                .position(|event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == grantee))
                .unwrap();
                        for id in [first, second] {
                            assert!(events.iter().position(|event|matches!(event,GameEvent::Discarded {object_id,..} if *object_id==id)).unwrap()<first_draw);
                        }
                    }

                    assert_public_adventure_grant_current_and_legacy_saves(
                        runner.state(),
                        hearth,
                        grantee,
                        if relative_population { 2 } else { 5 },
                    );
                    if !alternative {
                        assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
                        assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
                    }
                    assert_eq!(runner.state().players[grantee.0 as usize].hand.len(), 2);
                    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::StackResolved { object_id } if *object_id == hearth)).count(), 1);
                    for player in [grantee, owner] {
                        for _ in 0..6 {
                            runner.state_mut().players[player.0 as usize]
                                .mana_pool
                                .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
                        }
                    }
                    assert!(
                        !engine::game::casting::can_cast_object_now(runner.state(), owner, hearth),
                        "the adequately funded owner cannot spend the resolving controller's grant"
                    );
                    runner.advance_to_end_step();
                    runner.advance_to_phase(Phase::PreCombatMain);
                    assert_eq!(runner.state().active_player, grantee);
                    let later_payment = if relative_population { 2 } else { 5 };
                    for _ in 0..later_payment {
                        runner.state_mut().players[grantee.0 as usize]
                            .mana_pool
                            .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
                    }
                    // The resolved Commandeer is an additional distinct Instant: isolated
                    // one ID costs 4R; three preexisting IDs plus it cost 1R.
                    assert_eq!(runner.state().priority_player, grantee);
                    assert!(runner.state().objects[&unrelated]
                        .casting_permissions
                        .is_empty());
                    assert!(!engine::game::casting::can_cast_object_now(
                        runner.state(),
                        grantee,
                        unrelated
                    ));
                    assert_eq!(
                        engine::game::casting::effective_spell_cost(
                            runner.state(),
                            grantee,
                            hearth
                        ),
                        Some(ManaCost::Cost {
                            generic: if relative_population { 1 } else { 4 },
                            shards: vec![ManaCostShard::Red]
                        })
                    );
                    let committed = runner.cast(hearth).commit();
                    assert!(matches!(
                        committed.state().stack.back().unwrap().kind,
                        StackEntryKind::Spell {
                            actual_mana_spent,
                            ..
                        } if actual_mana_spent == later_payment
                    ));
                    let outcome = committed.resolve();
                    outcome.assert_zone(&[hearth], Zone::Battlefield);
                    outcome.assert_power_toughness(hearth, 4, 5);
                    assert_eq!(outcome.state().objects[&hearth].controller, grantee);
                    assert_eq!(outcome.state().objects[&hearth].owner, owner);
                    assert!(
                        outcome.state().stack.is_empty()
                            && outcome.state().resolution_stack.is_empty()
                    );
                    assert!(matches!(
                        outcome.final_waiting_for(),
                        WaitingFor::Priority { .. }
                    ));
                }
            }
        }
    }
}

#[test]
fn resolving_adventure_paid_stolen_goods_offers_native_face_free_and_later_normal_cost() {
    use engine::types::ability::CastingPermission;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Goods draw A", "Goods draw B", "Goods reserve"]);
    scenario.with_library_top(P1, &["Foreign reserve"]);
    let hearth = scenario
        .add_spell_to_library_top(P1, "Hearth Elemental", false)
        .as_creature()
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::Red],
        })
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text(HEARTH)
        .id();
    let discarded = [
        scenario
            .add_creature_to_hand(P0, "Goods discard A", 1, 1)
            .id(),
        scenario
            .add_creature_to_hand(P0, "Goods discard B", 1, 1)
            .id(),
    ];
    let goods = scenario
        .add_spell_to_hand_from_oracle(P0, "Stolen Goods", false, ADVENTURE_STOLEN_GOODS)
        .with_mana_cost(blue_cost(3, 1))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    {
        let object = runner.state_mut().objects.get_mut(&hearth).unwrap();
        object.back_face = Some(stoke_genius_face());
        object.power = Some(4);
        object.toughness = Some(5);
        object.base_power = Some(4);
        object.base_toughness = Some(5);
    }
    let outcome = runner.cast(goods).target_player(P1).resolve();
    outcome.assert_zone(&[goods], Zone::Graveyard);
    outcome.assert_zone(&[hearth], Zone::Exile);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let draw_ids: Vec<_> = outcome.state().players[0].library.iter().copied().collect();
    let first_exile = outcome.state().objects[&hearth].incarnation;
    assert!(outcome.state().objects[&hearth]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                granted_to: P0,
                without_paying_mana_cost: true,
                ..
            }
        )));
    drop(outcome);
    let mut expired = GameRunner::from_state(runner.state().clone());
    expired.advance_to_phase(Phase::End);
    expired.advance_to_phase(Phase::PreCombatMain);
    expired.advance_to_phase(Phase::End);
    expired.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(expired.state().active_player, P0);
    assert!(
        !engine::game::casting::can_cast_adventure_face_now(expired.state(), P0, hearth, false),
        "the actual end-of-turn Goods window expires"
    );
    runner
        .act(GameAction::CastSpell {
            object_id: hearth,
            card_id: runner.state().objects[&hearth].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            player: P0,
            kind: engine::types::game_state::CastOfferKind::Adventure { .. }
        }
    ));
    assert!(engine::ai_support::legal_actions(runner.state())
        .contains(&GameAction::ChooseAdventureFace { creature: false }));
    runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .unwrap();
    assert_eq!(runner.state().objects[&hearth].name, "Stoke Genius");
    assert_eq!(runner.state().objects[&hearth].mana_cost.mana_value(), 2);
    assert!(matches!(
        runner.state().stack.back().unwrap().kind,
        StackEntryKind::Spell {
            actual_mana_spent: 0,
            casting_variant: engine::types::game_state::CastingVariant::Adventure,
            ..
        }
    ));
    let mut stoke_events = Vec::new();
    for _ in 0..4 {
        if runner.state().objects[&hearth].zone == Zone::Exile {
            break;
        }
        stoke_events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert_eq!(
        runner.state().players[0]
            .hand
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        draw_ids[..2].iter().copied().collect()
    );
    assert_eq!(runner.state().objects[&draw_ids[2]].zone, Zone::Library);
    let first_draw = stoke_events
        .iter()
        .position(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
        .unwrap();
    for id in discarded {
        assert!(
            stoke_events
                .iter()
                .position(
                    |event| matches!(event,GameEvent::Discarded {object_id,..} if *object_id==id)
                )
                .unwrap()
                < first_draw
        );
    }
    for id in discarded {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(runner.state().objects[&hearth].zone, Zone::Exile);
    let object = &runner.state().objects[&hearth];
    assert!(object.incarnation > first_exile);
    assert_public_adventure_grant_current_and_legacy_saves(runner.state(), hearth, P0, 5);
    assert!(object
        .casting_permissions
        .contains(&CastingPermission::resolved_adventure(
            P0,
            engine::types::identifiers::ObjectIncarnationRef::from_object(object)
        )));
    assert!(!object
        .casting_permissions
        .iter()
        .any(|permission| matches!(permission, CastingPermission::PlayFromExile { .. })));
    assert!(!engine::game::casting::can_cast_adventure_face_now(
        runner.state(),
        P0,
        hearth,
        false
    ));
    for _ in 0..5 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    let committed = runner.cast(hearth).commit();
    // Stolen Goods is one Sorcery in P0's graveyard: normal Hearth costs 4R.
    assert!(matches!(
        committed.state().stack.back().unwrap().kind,
        StackEntryKind::Spell {
            actual_mana_spent: 5,
            ..
        }
    ));
    let outcome = committed.resolve();
    outcome.assert_zone(&[hearth], Zone::Battlefield);
    outcome.assert_power_toughness(hearth, 4, 5);
    assert_eq!(outcome.state().objects[&hearth].owner, P1);
    assert_eq!(outcome.state().objects[&hearth].controller, P0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
}

fn coordinated_graveyard_witnesses(
    scenario: &mut GameScenario,
    caster: engine::types::player::PlayerId,
    populated: bool,
) -> Vec<ObjectId> {
    if !populated {
        return vec![];
    }
    let other = if caster == P0 { P1 } else { P0 };
    let overlap = scenario
        .add_spell_to_graveyard(caster, "Synthetic instant Adventure overlap", true)
        .id();
    scenario.add_spell_to_graveyard(caster, "Synthetic own graveyard sorcery", false);
    let adventure = scenario
        .add_creature_to_graveyard(caster, "Synthetic own graveyard adventurer", 1, 1)
        .id();
    let foreign = scenario
        .add_spell_to_graveyard(other, "Synthetic foreign instant Adventure", true)
        .id();
    scenario.add_spell_to_hand(caster, "Synthetic own Hand instant decoy", true);
    scenario.add_spell_to_exile(caster, "Synthetic own Exile instant decoy", true);
    scenario.add_creature_to_graveyard(caster, "Synthetic ordinary graveyard permanent", 1, 1);
    // CR 715.2a + CR 715.2c: real alternative-characteristic metadata qualifies
    // an object once, even when that same object also has an Instant card type.
    vec![overlap, adventure, foreign]
}

#[test]
fn coordinated_four_distinct_graveyard_ids_keep_hearth_one_red_and_frantic_six() {
    for caster in [P0, P1] {
        for hearth_case in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let adventures = coordinated_graveyard_witnesses(&mut scenario, caster, true);
            scenario.add_spell_to_graveyard(caster, "Fourth distinct own graveyard instant", true);
            let spell = if hearth_case {
                scenario
                    .add_creature_to_hand(caster, "Hearth Elemental", 4, 5)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 5,
                        shards: vec![ManaCostShard::Red],
                    })
                    .with_subtypes(vec!["Elemental"])
                    .from_oracle_text(HEARTH)
                    .id()
            } else {
                scenario
                    .add_spell_to_hand_from_oracle(caster, "Frantic Firebolt", true, FRANTIC)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 2,
                        shards: vec![ManaCostShard::Red],
                    })
                    .id()
            };
            let target = scenario
                .add_creature(
                    if caster == P0 { P1 } else { P0 },
                    "Four-ID damage target",
                    1,
                    10,
                )
                .id();
            let untouched = scenario
                .add_creature(
                    if caster == P0 { P1 } else { P0 },
                    "Four-ID untouched target",
                    1,
                    10,
                )
                .id();
            let paid = if hearth_case { 2 } else { 3 };
            scenario.with_mana_pool(
                caster,
                (0..paid)
                    .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                    .collect(),
            );
            let mut runner = scenario.build();
            runner.state_mut().active_player = caster;
            runner.state_mut().priority_player = caster;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
            for id in adventures {
                runner.state_mut().objects.get_mut(&id).unwrap().back_face =
                    Some(stoke_genius_face());
            }
            if hearth_case {
                runner
                    .state_mut()
                    .objects
                    .get_mut(&spell)
                    .unwrap()
                    .back_face = Some(stoke_genius_face());
            }
            let committed = if hearth_case {
                runner.cast(spell).adventure_face(true).commit()
            } else {
                runner.cast(spell).target_object(target).commit()
            };
            assert!(
                matches!(committed.state().stack.back().unwrap().kind,StackEntryKind::Spell {actual_mana_spent,..} if actual_mana_spent==paid)
            );
            let outcome = committed.resolve();
            if hearth_case {
                outcome.assert_zone(&[spell], Zone::Battlefield);
                outcome.assert_power_toughness(spell, 4, 5);
            } else {
                assert_eq!(outcome.state().objects[&target].damage_marked, 6);
                assert_eq!(outcome.state().objects[&untouched].damage_marked, 0);
                outcome.assert_zone(&[spell], Zone::Graveyard);
            }
            assert_eq!(outcome.mana_pool_total(caster), 0);
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(
                outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty()
            );
        }
    }
}

#[test]
fn coordinated_relative_hearth_elemental_paid_cost() {
    for caster in [P0, P1] {
        for (populated, pool, red, succeeds) in [
            (true, 3, true, true),
            (false, 6, true, true),
            (true, 2, true, false),
            (true, 3, false, false),
        ] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let spell = scenario
                .add_creature_to_hand(caster, "Hearth Elemental", 4, 5)
                .with_mana_cost(ManaCost::Cost {
                    generic: 5,
                    shards: vec![ManaCostShard::Red],
                })
                .with_subtypes(vec!["Elemental"])
                .from_oracle_text(HEARTH)
                .id();
            let adventures = coordinated_graveyard_witnesses(&mut scenario, caster, populated);
            scenario.with_mana_pool(
                caster,
                (0..pool)
                    .map(|index| {
                        ManaUnit::new(
                            if red && index == 0 {
                                ManaType::Red
                            } else {
                                ManaType::Colorless
                            },
                            ObjectId(0),
                            false,
                            vec![],
                        )
                    })
                    .collect(),
            );
            let mut runner = scenario.build();
            // Fixture authority is established before the first production action.
            runner.state_mut().active_player = caster;
            runner.state_mut().priority_player = caster;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
            let back = stoke_genius_face();
            runner
                .state_mut()
                .objects
                .get_mut(&spell)
                .unwrap()
                .back_face = Some(back.clone());
            for id in adventures {
                runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(back.clone());
            }
            assert_eq!(runner.state().objects[&spell].static_definitions.len(), 1);
            if !succeeds {
                assert!(runner
                    .cast(spell)
                    .adventure_face(true)
                    .try_resolve()
                    .is_err());
                continue;
            }
            let committed = runner.cast(spell).adventure_face(true).commit();
            let entry = committed
                .state()
                .stack
                .iter()
                .find(|entry| entry.id == spell)
                .unwrap();
            // CR 118.7a + CR 601.2f + CR 601.2h: count3 removes only generic3.
            assert_eq!(entry.source_id, spell);
            assert_eq!(entry.controller, caster);
            assert!(
                matches!(entry.kind, StackEntryKind::Spell { actual_mana_spent, .. } if actual_mana_spent == pool)
            );
            assert_eq!(
                committed.state().players[caster.0 as usize]
                    .mana_pool
                    .total(),
                0
            );
            assert_eq!(committed.state().objects[&spell].name, "Hearth Elemental");
            let outcome = committed.resolve();
            outcome.assert_zone(&[spell], Zone::Battlefield);
            outcome.assert_power_toughness(spell, 4, 5);
            assert_eq!(outcome.state().objects[&spell].controller, caster);
            assert_eq!(outcome.state().objects[&spell].back_face, Some(back));
            assert_eq!(outcome.mana_pool_total(caster), 0);
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(outcome.state().stack.is_empty());
            assert!(outcome.state().resolution_stack.is_empty());
        }
    }
}

#[test]
fn coordinated_relative_frantic_firebolt_paid_damage() {
    for caster in [P0, P1] {
        for populated in [true, false] {
            let other = if caster == P0 { P1 } else { P0 };
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let spell = scenario
                .add_spell_to_hand_from_oracle(caster, "Frantic Firebolt", true, FRANTIC)
                .with_mana_cost(ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::Red],
                })
                .id();
            let target = scenario
                .add_creature(other, "Synthetic selected target", 1, 20)
                .id();
            let untouched = scenario
                .add_creature(other, "Synthetic other target", 1, 20)
                .id();
            let adventures = coordinated_graveyard_witnesses(&mut scenario, caster, populated);
            scenario.with_mana_pool(
                caster,
                [ManaType::Red, ManaType::Colorless, ManaType::Colorless]
                    .into_iter()
                    .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
                    .collect(),
            );
            let mut runner = scenario.build();
            runner.state_mut().active_player = caster;
            runner.state_mut().priority_player = caster;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
            for id in adventures {
                runner.state_mut().objects.get_mut(&id).unwrap().back_face =
                    Some(stoke_genius_face());
            }
            let committed = runner.cast(spell).target_object(target).commit();
            let entry = committed
                .state()
                .stack
                .iter()
                .find(|entry| entry.id == spell)
                .unwrap();
            // CR 601.2a + CR 601.2c + CR 601.2h: exact source, actor, target and payment.
            assert_eq!(entry.source_id, spell);
            assert_eq!(entry.controller, caster);
            assert_eq!(
                entry.ability().unwrap().targets,
                vec![engine::types::ability::TargetRef::Object(target)]
            );
            assert!(matches!(
                entry.kind,
                StackEntryKind::Spell {
                    actual_mana_spent: 3,
                    ..
                }
            ));
            assert_eq!(
                committed.state().players[caster.0 as usize]
                    .mana_pool
                    .total(),
                0
            );
            let outcome = committed.resolve();
            outcome.assert_zone(&[spell], Zone::Graveyard);
            // CR 120.3e + CR 608.2h: the resolving Instant is still on Stack
            // when count+2 is applied, and does not count itself or change damage later.
            assert_eq!(
                outcome.state().objects[&target].damage_marked,
                if populated { 5 } else { 2 }
            );
            assert_eq!(outcome.state().objects[&untouched].damage_marked, 0);
            outcome.assert_life_delta(P0, 0);
            outcome.assert_life_delta(P1, 0);
            assert_eq!(outcome.mana_pool_total(caster), 0);
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(outcome.state().stack.is_empty());
            assert!(outcome.state().resolution_stack.is_empty());
        }
    }
}

/// CR 601.2c + CR 608.2h: target identity is announced; the graveyard
/// population is read only when the damage instruction is applied.
#[test]
fn relative_frantic_reads_the_actual_paid_response_graveyard_after_commit() {
    use engine::types::ability::TargetRef;
    for caster in [P0, P1] {
        let other = if caster == P0 { P1 } else { P0 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let adventures = coordinated_graveyard_witnesses(&mut scenario, caster, true);
        let firebolt = scenario
            .add_spell_to_hand_from_oracle(caster, "Frantic Firebolt", true, FRANTIC)
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            })
            .id();
        // A synthetic independent one-card Draw is an ordinary paid response,
        // not a pre-populated journal or a second initial graveyard member.
        let response = scenario
            .add_spell_to_hand_from_oracle(caster, "Population response", true, "Draw a card.")
            .with_mana_cost(blue_cost(0, 1))
            .id();
        let first = scenario
            .add_creature(other, "First legal Firebolt target", 1, 20)
            .id();
        let selected = scenario
            .add_creature(other, "Second legal Firebolt target", 1, 20)
            .id();
        let drawn = scenario.add_card_to_library_top(caster, "Response draw");
        let mut runner = scenario.build();
        runner.state_mut().active_player = caster;
        runner.state_mut().priority_player = caster;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
        for id in adventures {
            runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(stoke_genius_face());
        }
        for kind in [
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
        ] {
            runner
                .state_mut()
                .add_mana_to_pool(caster, ManaUnit::new(kind, ObjectId(0), false, vec![]));
        }
        runner
            .act(GameAction::CastSpell {
                object_id: firebolt,
                card_id: runner.state().objects[&firebolt].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Firebolt must announce its actual target");
        };
        assert_eq!(*player, caster);
        assert_eq!(target_slots.len(), 1);
        assert_eq!(target_slots[0].legal_targets.len(), 2);
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(first)));
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(selected)));
        assert_ne!(
            target_slots[0].legal_targets.first(),
            Some(&TargetRef::Object(selected))
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(selected)),
            })
            .unwrap();
        assert_eq!(
            runner.state().players[caster.0 as usize].mana_pool.total(),
            1
        );
        assert!(matches!(
            runner.state().stack.back().unwrap().kind,
            StackEntryKind::Spell {
                actual_mana_spent: 3,
                ..
            }
        ));
        assert_eq!(runner.state().objects[&response].zone, Zone::Hand);
        runner.cast(response).commit();
        runner.resolve_top();
        assert_eq!(runner.state().objects[&response].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
        assert_eq!(runner.state().objects[&firebolt].zone, Zone::Stack);
        assert_eq!(runner.state().objects[&selected].damage_marked, 0);
        for restored in adventure_authority_save_representations(runner.state()) {
            let mut resumed = GameRunner::from_state(restored);
            let entry = resumed.state().stack.back().unwrap();
            assert_eq!((entry.source_id, entry.controller), (firebolt, caster));
            assert_eq!(
                entry.ability().unwrap().targets,
                vec![TargetRef::Object(selected)]
            );
            resumed.advance_until_stack_empty();
            assert_eq!(resumed.state().objects[&selected].damage_marked, 6);
            assert_eq!(resumed.state().objects[&first].damage_marked, 0);
            assert_eq!(resumed.state().objects[&firebolt].zone, Zone::Graveyard);
            assert_eq!(
                resumed.state().players[caster.0 as usize].mana_pool.total(),
                0
            );
            assert_eq!(
                (
                    resumed.state().players[0].life,
                    resumed.state().players[1].life
                ),
                (20, 20)
            );
            assert!(matches!(
                resumed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                resumed.state().stack.is_empty()
                    && resumed.state().pending_trigger.is_none()
                    && resumed.state().deferred_triggers.is_empty()
                    && resumed.state().pending_trigger_order.is_none()
                    && resumed.state().resolution_stack.is_empty()
            );
        }
    }
}

/// CR 601.2f + CR 601.2h: a large live population removes all generic
/// mana, while the printed red shard still requires actual red payment.
#[test]
fn relative_hearth_population_floors_generic_and_retains_the_red_shard() {
    for caster in [P0, P1] {
        for red in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let adventures = coordinated_graveyard_witnesses(&mut scenario, caster, true);
            for n in 0..3 {
                scenario.add_spell_to_graveyard(
                    caster,
                    &format!("Additional distinct qualifying Instant {n}"),
                    true,
                );
            }
            let hearth = scenario
                .add_creature_to_hand(caster, "Hearth Elemental", 4, 5)
                .with_subtypes(vec!["Elemental"])
                .from_oracle_text(HEARTH)
                .with_mana_cost(ManaCost::Cost {
                    generic: 5,
                    shards: vec![ManaCostShard::Red],
                })
                .id();
            scenario.with_mana_pool(
                caster,
                vec![ManaUnit::new(
                    if red {
                        ManaType::Red
                    } else {
                        ManaType::Colorless
                    },
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
            let mut runner = scenario.build();
            runner.state_mut().active_player = caster;
            runner.state_mut().priority_player = caster;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
            for id in adventures.into_iter().chain(std::iter::once(hearth)) {
                runner.state_mut().objects.get_mut(&id).unwrap().back_face =
                    Some(stoke_genius_face());
            }
            if red {
                let committed = runner.cast(hearth).adventure_face(true).commit();
                assert!(matches!(
                    committed.state().stack.back().unwrap().kind,
                    StackEntryKind::Spell {
                        actual_mana_spent: 1,
                        ..
                    }
                ));
                let result = committed.resolve();
                result.assert_zone(&[hearth], Zone::Battlefield);
                result.assert_power_toughness(hearth, 4, 5);
                assert_eq!(result.mana_pool_total(caster), 0);
                assert!(matches!(
                    result.final_waiting_for(),
                    WaitingFor::Priority { .. }
                ));
                assert!(
                    result.state().stack.is_empty()
                        && result.state().pending_trigger.is_none()
                        && result.state().deferred_triggers.is_empty()
                        && result.state().pending_trigger_order.is_none()
                        && result.state().resolution_stack.is_empty()
                );
            } else {
                assert!(runner
                    .cast(hearth)
                    .adventure_face(true)
                    .try_resolve()
                    .is_err());
                assert_eq!(runner.state().objects[&hearth].zone, Zone::Hand);
                assert_eq!(
                    runner.state().players[caster.0 as usize].mana_pool.total(),
                    1
                );
            }
        }
    }
}

#[test]
fn ordinary_teferi_grant_preserves_paid_hand_creature_flash() {
    const TEFERI: &str = "Flash\nCreature cards you own that aren't on the battlefield have flash.\nEach opponent can cast spells only any time they could cast a sorcery.";
    for (grantor, creature) in [(true, true), (false, true), (true, false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = grantor.then(|| {
            scenario
                .add_creature_to_hand(P0, "Teferi, Mage of Zhalfir", 3, 4)
                .with_mana_cost(blue_cost(2, 3))
                .with_subtypes(vec!["Human", "Wizard"])
                .from_oracle_text_with_keywords(&["Flash"], TEFERI)
                .id()
        });
        let recipient = if creature {
            scenario
                .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Green],
                })
                .from_oracle_text("")
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
                .with_mana_cost(blue_cost(2, 1))
                .id()
        };
        let forest = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
        let island = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
        let second_island = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
        scenario.with_library_top(
            P0,
            &[
                "Synthetic draw one",
                "Synthetic draw two",
                "Synthetic draw three",
            ],
        );
        scenario.with_library_top(
            P1,
            &["Synthetic opponent draw one", "Synthetic opponent draw two"],
        );
        mana(&mut scenario, 5);
        let mut runner = scenario.build();
        if let Some(source) = source {
            let committed = runner.cast(source).commit();
            let entry = committed
                .state()
                .stack
                .iter()
                .find(|entry| entry.id == source)
                .unwrap();
            assert_eq!(entry.controller, P0);
            assert_eq!(entry.source_id, source);
            assert!(matches!(
                entry.kind,
                StackEntryKind::Spell {
                    actual_mana_spent: 5,
                    ..
                }
            ));
            let outcome = committed.resolve();
            outcome.assert_zone(&[source], Zone::Battlefield);
            outcome.assert_power_toughness(source, 3, 4);
            assert_eq!(outcome.mana_pool_total(P0), 0);
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(outcome.state().stack.is_empty());
            assert!(outcome.state().resolution_stack.is_empty());
        }
        runner.advance_to_end_step();
        assert_eq!(runner.state().phase, Phase::End);
        runner.advance_to_upkeep();
        assert_eq!(runner.state().phase, Phase::Upkeep);
        assert_eq!(runner.state().active_player, P1);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        for land in if creature {
            vec![forest, island]
        } else {
            vec![forest, island, second_island]
        } {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: land,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
        }
        // CR 702.8a: a creature Hand recipient uses its own controller's live
        // Flash grant during the opponent's upkeep; a sorcery and no-grant twin cannot.
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            if creature { 2 } else { 3 }
        );
        if !grantor || !creature {
            assert!(runner.cast(recipient).try_resolve().is_err());
            continue;
        }
        let committed = runner.cast(recipient).commit();
        let entry = committed
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == recipient)
            .unwrap();
        assert_eq!(entry.source_id, recipient);
        assert_eq!(entry.controller, P0);
        assert!(matches!(
            entry.kind,
            StackEntryKind::Spell {
                actual_mana_spent: 2,
                ..
            }
        ));
        let outcome = committed.resolve();
        outcome.assert_zone(&[recipient], Zone::Battlefield);
        outcome.assert_power_toughness(recipient, 2, 2);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty());
        assert!(outcome.state().resolution_stack.is_empty());
    }
}

const CANDLEKEEP: &str = "Until end of turn, creatures you control have base power and toughness X/X, where X is the number of cards you own in exile and in your graveyard that are instant cards, are sorcery cards, and/or have an Adventure.";

fn candlekeep_board(
    populated: bool,
    recipients: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 5);
    let spell = scenario
        .add_spell_to_hand(P0, "Candlekeep Inspiration", false)
        .with_mana_cost(blue_cost(4, 1))
        .from_oracle_text(CANDLEKEEP)
        .id();
    let mut controlled = Vec::new();
    if recipients {
        controlled.push(scenario.add_creature(P0, "Own recipient", 6, 7).id());
        controlled.push(
            scenario
                .add_creature(P0, "Foreign-owned recipient", 4, 5)
                .id(),
        );
    }
    let opponent = scenario.add_creature(P1, "Opponent recipient", 8, 9).id();
    let own_opponent = scenario
        .add_creature(P1, "Own opponent-controlled", 10, 11)
        .id();
    let late = scenario.add_creature_to_hand(P0, "Late entrant", 5, 6).id();
    let mut adventure_ids = Vec::new();
    if populated {
        let overlap = scenario
            .add_spell_to_exile(P0, "Instant Adventure", true)
            .id();
        scenario.add_spell_to_graveyard(P0, "Sorcery member", false);
        let adventure = scenario
            .add_creature_to_graveyard(P0, "Adventure permanent", 1, 1)
            .id();
        adventure_ids.extend([overlap, adventure]);
        scenario.add_spell_to_exile(P1, "Foreign exile", true);
        scenario.add_spell_to_graveyard(P1, "Opponent graveyard", false);
        scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
        scenario.add_spell_to_hand(P0, "Hand instant", true);
    }
    let mut runner = scenario.build();
    for id in adventure_ids {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.controller = P1;
        object.back_face = Some(BackFaceData {
            name: "Adventure alternative".into(),
            layout_kind: Some(LayoutKind::Adventure),
            ..Default::default()
        });
    }
    if recipients {
        runner
            .state_mut()
            .objects
            .get_mut(&controlled[1])
            .unwrap()
            .owner = P1;
    }
    runner
        .state_mut()
        .objects
        .get_mut(&own_opponent)
        .unwrap()
        .owner = P0;
    (runner, spell, controlled, opponent, late)
}

#[test]
fn candlekeep_population_base_pt() {
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(true, true);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    // CR 608.2h + CR 613.4b: the complete three-member population sets both
    // base characteristics when this sorcery resolves, before it enters the graveyard.
    for id in controlled {
        let obj = &outcome.state().objects[&id];
        assert_eq!((obj.power, obj.toughness), (Some(3), Some(3)));
    }
    let obj = &outcome.state().objects[&opponent];
    assert_eq!((obj.power, obj.toughness), (Some(8), Some(9)));
    let other = outcome
        .state()
        .objects
        .values()
        .find(|o| o.name == "Own opponent-controlled")
        .unwrap();
    assert_eq!((other.power, other.toughness), (Some(10), Some(11)));
}

#[test]
fn candlekeep_population_resolution_binding() {
    let (mut runner, spell, controlled, _, late) = candlekeep_board(true, true);
    runner
        .cast(spell)
        .resolve()
        .assert_zone(&[spell], Zone::Graveyard);
    let assert_three = |runner: &GameRunner| {
        for id in &controlled {
            let obj = &runner.state().objects[id];
            assert_eq!((obj.power, obj.toughness), (Some(3), Some(3)));
        }
    };
    // CR 611.2d: the source becoming a fourth graveyard member cannot update X.
    assert_three(&runner);
    let hand_instant = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Hand instant")
        .unwrap()
        .id;
    engine::game::zones::remove_from_zone(runner.state_mut(), hand_instant, Zone::Hand, P0);
    runner
        .state_mut()
        .objects
        .get_mut(&hand_instant)
        .unwrap()
        .zone = Zone::Graveyard;
    engine::game::zones::add_to_zone(runner.state_mut(), hand_instant, Zone::Graveyard, P0);
    engine::game::zones::remove_from_zone(runner.state_mut(), late, Zone::Hand, P0);
    runner.state_mut().objects.get_mut(&late).unwrap().zone = Zone::Battlefield;
    engine::game::zones::add_to_zone(runner.state_mut(), late, Zone::Battlefield, P0);
    {
        let recipient = runner.state_mut().objects.get_mut(&controlled[0]).unwrap();
        recipient.controller = P1;
        recipient.base_controller = Some(P1);
    }
    engine::game::layers::evaluate_layers(runner.state_mut());
    runner
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();
    // CR 611.2c: resolution fixes the recipients even after entry/control changes.
    assert_three(&runner);
    assert_eq!(runner.state().objects[&controlled[0]].controller, P1);
    let obj = &runner.state().objects[&late];
    assert_eq!((obj.power, obj.toughness), (Some(5), Some(6)));
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    // CR 514.2: real cleanup expires the effect and restores printed values.
    let obj = &runner.state().objects[&controlled[0]];
    assert_eq!((obj.power, obj.toughness), (Some(6), Some(7)));
    let obj = &runner.state().objects[&controlled[1]];
    assert_eq!((obj.power, obj.toughness), (Some(4), Some(5)));
}

#[test]
fn candlekeep_population_empty_cases() {
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(false, true);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    // CR 704.5f: zero base toughness is followed by the ordinary state-based death.
    outcome.assert_zone(&controlled, Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert_eq!(outcome.state().objects[&opponent].power, Some(8));
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(true, false);
    assert!(controlled.is_empty());
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert_eq!(outcome.state().objects[&opponent].toughness, Some(9));
}

#[test]
fn coordinated_population_draw_composition() {
    // Synthetic Sorcery witnesses: these compose the repaired population with
    // existing Draw/Offset/Multiply consumers, without replacing printed Oracle.
    for (introduction, populated_draws, empty_draws) in [
        ("the number of ", 3, 0),
        ("the total number of ", 3, 0),
        ("one plus the total number of ", 4, 1),
        ("twice the number of ", 6, 0),
    ] {
        for (populated, expected) in [(true, populated_draws), (false, empty_draws)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            mana(&mut scenario, 2);
            let source = format!("Draw X cards, where X is {introduction}cards you own in exile and in your graveyard that are instant cards, are sorcery cards, and/or have an Adventure.");
            let spell = scenario
                .add_spell_to_hand(P0, "Synthetic population Draw", false)
                .with_mana_cost(blue_cost(1, 1))
                .from_oracle_text(&source)
                .id();
            // Known qualifying types in Hand and Library must contribute zero.
            scenario.add_spell_to_hand(P0, "Hand instant", true);
            scenario.add_spell_to_hand(P1, "Opponent hand sorcery", false);
            for player in [P0, P1] {
                for index in 0..8 {
                    scenario.add_spell_to_library_top(
                        player,
                        &format!("Known library instant {index}"),
                        true,
                    );
                }
            }
            let mut adventure_ids = Vec::new();
            let mut foreign_ids = Vec::new();
            if populated {
                let overlap = scenario
                    .add_spell_to_exile(P0, "Instant Adventure member", true)
                    .id();
                let sorcery = scenario
                    .add_spell_to_graveyard(P0, "Sorcery member", false)
                    .id();
                let adventure = scenario
                    .add_creature_to_graveyard(P0, "Adventure permanent member", 1, 1)
                    .id();
                adventure_ids.extend([overlap, adventure]);
                foreign_ids.push(
                    scenario
                        .add_spell_to_exile(P1, "Foreign exile instant", true)
                        .id(),
                );
                foreign_ids.push(
                    scenario
                        .add_spell_to_graveyard(P1, "Opponent graveyard sorcery", false)
                        .id(),
                );
                scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
                let mut runner = scenario.build();
                // CR 108.3 + CR 109.4: off-zone controller values cannot replace
                // ownership; hostile foreign cards coexist with positive members.
                for id in [overlap, sorcery, adventure] {
                    runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
                }
                for id in foreign_ids {
                    runner.state_mut().objects.get_mut(&id).unwrap().controller = P0;
                }
                // CR 715.2a + CR 715.2c: actual Adventure metadata qualifies the
                // permanent; Instant plus Adventure is still one card.
                for id in adventure_ids {
                    runner.state_mut().objects.get_mut(&id).unwrap().back_face =
                        Some(BackFaceData {
                            name: "Adventure alternative".into(),
                            layout_kind: Some(LayoutKind::Adventure),
                            ..Default::default()
                        });
                }
                assert_population_draw(&mut runner, spell, expected);
            } else {
                assert_population_draw(&mut scenario.build(), spell, expected);
            }
        }
    }
}

fn assert_population_draw(runner: &mut GameRunner, spell: ObjectId, expected: i64) {
    let outcome = runner.cast(spell).resolve();
    // CR 121.1 + CR 121.2: measure actual library-to-hand deliveries from stack
    // commit. CR 608.2h: the source is on Stack at the one population read;
    // its subsequent graveyard arrival cannot add another draw.
    outcome.assert_hand_drawn(P0, expected);
    outcome.assert_hand_drawn(P1, 0);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

#[test]
fn coordinated_population_export_string_loader() {
    const BEACON: &str = "Beacon Bolt deals damage to target creature equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\nJump-start (You may cast this card from your graveyard by discarding a card in addition to paying its other costs. Then exile this card.)";
    const DRAKE: &str = "Flying\nCrackling Drake's power is equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\nWhen this creature enters, draw a card.";
    const HUSKBURSTER: &str = "This spell costs {1} less to cast for each creature card you own in exile and in your graveyard.\nMenace, deathtouch";
    const RAL: &str = "[+1]: Look at the top two cards of your library. Put one of them into your hand and the other into your graveyard.\n[−3]: Ral deals damage to target creature equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\n[−8]: You get an emblem with \"Whenever you cast an instant or sorcery spell, this emblem deals 4 damage to any target and you draw two cards.\"";
    const CURVE: &str = "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is one plus the total number of instant and sorcery cards you own in exile and in your graveyard.";

    let client_export =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    assert!(
        client_export.exists(),
        "generated client export is required"
    );
    let raw = shared_card_export_json().expect(
        "generated full export is required for coordinated population string-loader regression",
    );
    let mut entries = Map::<String, Value>::new();
    for (key, name, oracle) in [
        ("sailors' bane", "Sailors' Bane", SAILORS),
        (
            "candlekeep inspiration",
            "Candlekeep Inspiration",
            CANDLEKEEP,
        ),
        ("beacon bolt", "Beacon Bolt", BEACON),
        ("crackling drake", "Crackling Drake", DRAKE),
        ("huskburster swarm", "Huskburster Swarm", HUSKBURSTER),
        ("ral, izzet viceroy", "Ral, Izzet Viceroy", RAL),
        ("serpentine curve", "Serpentine Curve", CURVE),
    ] {
        let entry = raw.get(key).expect("required changed face in full export");
        assert_eq!(entry.get("name").and_then(Value::as_str), Some(name));
        assert_eq!(
            entry.get("oracle_text").and_then(Value::as_str),
            Some(oracle)
        );
        entries.insert(key.into(), entry.clone());
    }
    assert_eq!(entries.len(), 7);
    let serialized = to_string(&entries).expect("unchanged raw entries serialize");
    let db = CardDatabase::from_json_str(&serialized).expect("real changed export entries load");
    assert_eq!(db.face_iter().count(), 7);
    let sailors = db
        .get_face_by_name("Sailors' Bane")
        .expect("loaded Sailors' Bane");
    let candlekeep = db
        .get_face_by_name("Candlekeep Inspiration")
        .expect("loaded Candlekeep Inspiration");
    let beacon = db
        .get_face_by_name("Beacon Bolt")
        .expect("loaded Beacon Bolt");
    let drake = db
        .get_face_by_name("Crackling Drake")
        .expect("loaded Crackling Drake");
    let huskburster = db
        .get_face_by_name("Huskburster Swarm")
        .expect("loaded Huskburster Swarm");
    let ral = db
        .get_face_by_name("Ral, Izzet Viceroy")
        .expect("loaded Ral, Izzet Viceroy");
    let curve = db
        .get_face_by_name("Serpentine Curve")
        .expect("loaded Serpentine Curve");
    for (face, name, oracle) in [
        (sailors, "Sailors' Bane", SAILORS),
        (candlekeep, "Candlekeep Inspiration", CANDLEKEEP),
        (beacon, "Beacon Bolt", BEACON),
        (drake, "Crackling Drake", DRAKE),
        (huskburster, "Huskburster Swarm", HUSKBURSTER),
        (ral, "Ral, Izzet Viceroy", RAL),
        (curve, "Serpentine Curve", CURVE),
    ] {
        assert_eq!(face.name, name);
        assert_eq!(face.oracle_text.as_deref(), Some(oracle));
    }

    // CR 108.3 + CR 109.2a + CR 109.5 + CR 400.1: the spell's "you"
    // selects ownership and the stated zones, independently of stored controller.
    let owned = FilterProp::Owned {
        controller: ControllerRef::You,
    };
    let typed =
        |kind, properties| TargetFilter::Typed(TypedFilter::new(kind).properties(properties));
    let zones = TargetFilter::Or {
        filters: vec![
            typed(
                TypeFilter::Card,
                vec![FilterProp::InZone { zone: Zone::Exile }],
            ),
            typed(
                TypeFilter::Card,
                vec![
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                    owned.clone(),
                ],
            ),
        ],
    };
    let qa = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![
                    typed(TypeFilter::Card, vec![owned.clone()]),
                    zones.clone(),
                    TargetFilter::Or {
                        filters: vec![
                            typed(TypeFilter::Instant, vec![]),
                            typed(TypeFilter::Sorcery, vec![]),
                            typed(TypeFilter::Card, vec![FilterProp::HasAdventure]),
                        ],
                    },
                ],
            },
        },
    };
    let qi = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Or {
                        filters: vec![
                            typed(TypeFilter::Instant, vec![owned.clone()]),
                            typed(TypeFilter::Sorcery, vec![owned.clone()]),
                        ],
                    },
                    zones.clone(),
                ],
            },
        },
    };
    let qc = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![typed(TypeFilter::Creature, vec![owned]), zones],
            },
        },
    };

    let sailors_static = sailors
        .static_abilities
        .first()
        .expect("Sailors cost static");
    let StaticMode::ModifyCost {
        mode,
        amount,
        dynamic_count: Some(count),
        ..
    } = &sailors_static.mode
    else {
        panic!("Sailors dynamic cost slot");
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(*amount, ManaCost::generic(1));
    assert_eq!(sailors_static.affected, Some(TargetFilter::SelfRef));
    let sailors_count = QuantityExpr::Ref { qty: count.clone() };
    assert_eq!(sailors_count, qa);

    let candlekeep_ability = candlekeep
        .abilities
        .first()
        .expect("Candlekeep spell ability");
    let Effect::GenericEffect {
        static_abilities,
        duration,
        ..
    } = candlekeep_ability.effect.as_ref()
    else {
        panic!("Candlekeep continuous effect slot");
    };
    // CR 611.2a + CR 613.4b: both base characteristics have the printed duration.
    assert_eq!(candlekeep_ability.duration, Some(Duration::UntilEndOfTurn));
    assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
    let [candlekeep_static] = static_abilities.as_slice() else {
        panic!("one Candlekeep static");
    };
    assert_eq!(candlekeep_static.mode, StaticMode::Continuous);
    assert_eq!(
        candlekeep_static.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
    assert!(!candlekeep_static.characteristic_defining);
    let [ContinuousModification::SetPowerDynamic {
        value: candlekeep_power,
    }, ContinuousModification::SetToughnessDynamic {
        value: candlekeep_toughness,
    }] = candlekeep_static.modifications.as_slice()
    else {
        panic!("exactly one distinct Candlekeep power and toughness slot");
    };
    assert_eq!(candlekeep_power, &qa);
    assert_eq!(candlekeep_toughness, &qa);

    let Effect::DealDamage {
        amount: beacon_count,
        ..
    } = beacon
        .abilities
        .first()
        .expect("Beacon ability")
        .effect
        .as_ref()
    else {
        panic!("Beacon damage amount slot");
    };
    assert_eq!(beacon_count, &qi);
    let drake_static = drake.static_abilities.first().expect("Drake power static");
    assert_eq!(drake_static.mode, StaticMode::Continuous);
    assert!(drake_static.characteristic_defining);
    assert_eq!(drake_static.affected, Some(TargetFilter::SelfRef));
    let [ContinuousModification::SetDynamicPower { value: drake_count }] =
        drake_static.modifications.as_slice()
    else {
        panic!("exactly one Drake dynamic power slot");
    };
    assert_eq!(drake_count, &qi);
    let huskburster_static = huskburster
        .static_abilities
        .first()
        .expect("Huskburster cost static");
    let StaticMode::ModifyCost {
        mode,
        amount,
        dynamic_count: Some(count),
        ..
    } = &huskburster_static.mode
    else {
        panic!("Huskburster dynamic cost slot");
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(*amount, ManaCost::generic(1));
    assert_eq!(huskburster_static.affected, Some(TargetFilter::SelfRef));
    let huskburster_count = QuantityExpr::Ref { qty: count.clone() };
    assert_eq!(huskburster_count, qc);
    let ral_ability = ral.abilities.get(1).expect("Ral minus-three ability");
    assert_eq!(ral_ability.cost, Some(AbilityCost::Loyalty { amount: -3 }));
    let Effect::DealDamage {
        amount: ral_count, ..
    } = ral_ability.effect.as_ref()
    else {
        panic!("Ral minus-three damage amount slot");
    };
    assert_eq!(ral_count, &qi);
    let Effect::Token {
        enter_with_counters,
        ..
    } = curve
        .abilities
        .first()
        .expect("Curve token ability")
        .effect
        .as_ref()
    else {
        panic!("Curve token counter slot");
    };
    let [(CounterType::Plus1Plus1, curve_count)] = enter_with_counters.as_slice() else {
        panic!("one Curve +1/+1 counter quantity");
    };
    assert_eq!(
        curve_count,
        &QuantityExpr::Offset {
            inner: Box::new(qi),
            offset: 1
        }
    );

    let slots = [
        ("Sailors cost", &sailors_count, 3, 0),
        ("Candlekeep power", candlekeep_power, 3, 0),
        ("Candlekeep toughness", candlekeep_toughness, 3, 0),
        ("Beacon damage", beacon_count, 2, 0),
        ("Drake power", drake_count, 2, 0),
        ("Huskburster cost", &huskburster_count, 2, 0),
        ("Ral damage", ral_count, 2, 0),
        ("Curve counters", curve_count, 3, 1),
    ];
    assert_eq!(slots.len(), 8);
    for populated in [true, false] {
        let mut scenario = GameScenario::new();
        let source = scenario
            .add_artifact_from_oracle(P0, "Loaded count source", "")
            .id();
        let mut owned_ids = Vec::new();
        let mut adventure_ids = Vec::new();
        if populated {
            let overlap = scenario
                .add_spell_to_exile(P0, "Loaded Instant Adventure", true)
                .id();
            let sorcery = scenario
                .add_spell_to_graveyard(P0, "Loaded Sorcery member", false)
                .id();
            let adventure = scenario
                .add_creature_to_graveyard(P0, "Loaded Adventure creature", 1, 1)
                .id();
            let ordinary = scenario
                .add_creature_to_graveyard(P0, "Loaded ordinary creature", 1, 1)
                .id();
            owned_ids.extend([overlap, sorcery, adventure, ordinary]);
            adventure_ids.extend([overlap, adventure]);
        }
        let foreign_ids = [
            scenario
                .add_spell_to_exile(P1, "Loaded foreign exile Instant", true)
                .id(),
            scenario
                .add_spell_to_graveyard(P1, "Loaded opponent Sorcery", false)
                .id(),
            scenario
                .add_creature_to_graveyard(P1, "Loaded opponent Creature", 1, 1)
                .id(),
        ];
        scenario.add_spell_to_hand(P0, "Loaded hand Instant", true);
        scenario.add_spell_to_library_top(P0, "Loaded library Instant", true);
        let mut runner = scenario.build();
        // CR 109.4: off-zone controller storage is not ownership authority.
        for id in owned_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
        }
        for id in foreign_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().controller = P0;
        }
        // CR 715.2a + CR 715.2c: real alternative characteristics qualify the
        // Adventure creature; the Instant plus Adventure remains one card.
        for id in adventure_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(BackFaceData {
                name: "Loaded Adventure alternative".into(),
                layout_kind: Some(LayoutKind::Adventure),
                ..Default::default()
            });
        }
        for (slot, quantity, positive, empty) in slots {
            assert_eq!(
                resolve_quantity(runner.state(), quantity, P0, source),
                if populated { positive } else { empty },
                "{slot}, populated={populated}"
            );
        }
    }
}

const DISMISSAL: &str = "This spell costs {1} less to cast for each artifact you control and each artifact card in your hand.\nCounter target noncreature spell.";
const REPEATED: &str = "This spell costs {1} less to cast for each artifact you control and each artifact card in your hand.";
const LEECH: &str = "This spell costs {1} less to cast for each Cave you control and each Cave card in your graveyard.\nLifelink";
#[test]
fn dismissal_population_and_counter() {
    for (populated, short) in [(true, false), (false, false), (true, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(
            &mut scenario,
            if short {
                2
            } else if populated {
                3
            } else {
                5
            },
        );
        let selected = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic selected noncreature",
                true,
                "You gain 1 life.",
            )
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let other = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic other noncreature",
                true,
                "You gain 2 life.",
            )
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let dismissal = scenario
            .add_spell_to_hand_from_oracle(P0, "Machinist's Dismissal", true, DISMISSAL)
            .with_mana_cost(blue_cost(4, 1))
            .id();
        if populated {
            // CR 109.4 + CR 109.5: the controlled leg follows controller, not owner.
            scenario
                .add_artifact_from_oracle(P1, "Stolen artifact", "")
                .controlled_by(P0);
            scenario.add_artifact_to_hand_from_oracle(P0, "Own hand artifact", "");
            scenario.add_artifact_from_oracle(P1, "Opponent artifact", "");
            scenario
                .add_artifact_from_oracle(P0, "Owned uncontrolled artifact", "")
                .controlled_by(P1);
            scenario.add_artifact_to_hand_from_oracle(P1, "Opponent hand artifact", "");
            scenario
                .add_creature_to_graveyard(P0, "Graveyard artifact", 1, 1)
                .as_artifact_creature();
            scenario.add_creature_to_hand(P0, "Hand nonartifact", 1, 1);
        }
        let mut runner = scenario.build();
        let mut first = runner.cast(selected).commit();
        let mut second = first.cast(other).commit();
        if short {
            assert!(second
                .cast(dismissal)
                .target_object(selected)
                .try_resolve()
                .is_err());
            continue;
        }
        let outcome = second.cast(dismissal).target_object(selected).resolve();
        outcome.assert_zone(&[selected, dismissal, other], Zone::Graveyard);
        // CR 601.2f: both scoped members reduce {4}U to {2}U.
        assert_eq!(outcome.mana_pool_total(P0), 0);
        // CR 608.2b: only the declared noncreature target is countered. The
        // other eligible spell resolves; the selected spell never gains life.
        outcome.assert_life_delta(P0, 2);
        assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, countered_by, .. } if *object_id == selected && *countered_by == dismissal)));
        assert!(!outcome.events().iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == other)));
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 5);
    let creature = scenario
        .add_creature_to_hand(P0, "Synthetic illegal creature spell target", 1, 1)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let dismissal = scenario
        .add_spell_to_hand_from_oracle(P0, "Machinist's Dismissal", true, DISMISSAL)
        .with_mana_cost(blue_cost(4, 1))
        .id();
    let mut runner = scenario.build();
    let mut committed = runner.cast(creature).commit();
    assert!(committed.cast(dismissal).try_resolve().is_err());
}

fn artifact_in(zone: Zone) -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Artifact).properties(vec![FilterProp::InZone { zone }]),
    )
}

fn count(state: &GameState, source: ObjectId, filter: TargetFilter) -> i32 {
    resolve_quantity(
        state,
        &QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        P0,
        source,
    )
}

#[test]
fn pending_artifact_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 2);
    let spell = scenario
        .add_artifact_to_hand_from_oracle(P0, "Synthetic pending population spell", REPEATED)
        .with_keyword(Keyword::Flash)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.add_artifact_from_oracle(P0, "Battlefield member", "");
    let hand = scenario
        .add_artifact_to_hand_from_oracle(P0, "Ordinary hand member", "")
        .id();
    let unrelated = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unrelated nonartifact stack object",
            true,
            "You gain 1 life.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let _committed = runner.cast(unrelated).commit();
    let completed_state = runner.state().clone();
    let union = TargetFilter::Or {
        filters: vec![artifact_in(Zone::Battlefield), artifact_in(Zone::Hand)],
    };
    assert_eq!(count(runner.state(), spell, union.clone()), 3);
    let origin_snapshot = runner.state().objects[&spell].clone();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: origin_snapshot.card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
    assert!(runner.state().objects[&spell].spell_announcement.is_some());
    assert!(runner.state().stack.iter().any(|entry| entry.id == spell));
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().object_id,
        spell
    );
    // CR 601.2a + CR 601.2f: pending is Stack, not Hand, when cost locks.
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(2)
    );
    assert_eq!(count(runner.state(), spell, union.clone()), 2);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Hand)), 1);
    assert_eq!(
        count(
            runner.state(),
            spell,
            TargetFilter::And {
                filters: vec![
                    artifact_in(Zone::Hand),
                    TargetFilter::SpecificObject { id: spell }
                ]
            }
        ),
        0
    );
    assert_eq!(
        count(
            runner.state(),
            spell,
            TargetFilter::And {
                filters: vec![
                    artifact_in(Zone::Hand),
                    TargetFilter::SpecificObject { id: hand }
                ]
            }
        ),
        1
    );
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 1);
    assert_eq!(
        count(
            runner.state(),
            spell,
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
                FilterProp::InAnyZone {
                    zones: vec![Zone::Hand, Zone::Stack]
                }
            ]))
        ),
        2
    );
    let nested = TargetFilter::And {
        filters: vec![
            TargetFilter::Or {
                filters: vec![artifact_in(Zone::Hand), artifact_in(Zone::Stack)],
            },
            TargetFilter::Not {
                filter: Box::new(artifact_in(Zone::Hand)),
            },
        ],
    };
    assert_eq!(count(runner.state(), spell, nested), 1);
    assert_eq!(runner.state().objects[&hand].zone, Zone::Hand);
    runner.act(GameAction::CancelCast).unwrap();
    assert_eq!(count(runner.state(), spell, union), 3);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let mut completed = GameRunner::from_state(completed_state);
    let committed = completed.cast(spell).commit();
    assert_eq!(count(committed.state(), spell, artifact_in(Zone::Stack)), 1);
    let outcome = committed.resolve();
    outcome.assert_zone(&[spell], Zone::Battlefield);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

#[test]
fn leech_population_cost() {
    for (populated, colorless, short) in [
        (true, false, false),
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(
            P0,
            (0..if short {
                5
            } else if populated {
                6
            } else {
                8
            })
                .map(|_| {
                    ManaUnit::new(
                        if colorless {
                            ManaType::Colorless
                        } else {
                            ManaType::Black
                        },
                        ObjectId(0),
                        false,
                        vec![],
                    )
                })
                .collect(),
        );
        let spell = scenario
            .add_creature_to_hand(P0, "Gargantuan Leech", 5, 5)
            .with_subtypes(vec!["Leech"])
            .with_mana_cost(ManaCost::Cost {
                generic: 7,
                shards: vec![ManaCostShard::Black],
            })
            .from_oracle_text_with_keywords(&["Lifelink"], LEECH)
            .id();
        let mut off_zone = None;
        if populated {
            scenario
                .add_land_from_oracle(P1, "Controlled foreign Cave", "")
                .with_subtypes(vec!["Cave"])
                .controlled_by(P0);
            off_zone = Some(
                scenario
                    .add_land_to_graveyard(P0, "Own graveyard Cave")
                    .with_subtypes(vec!["Cave"])
                    .id(),
            );
            scenario
                .add_land_to_graveyard(P1, "Opponent Cave")
                .with_subtypes(vec!["Cave"]);
            scenario
                .add_land_from_oracle(P0, "Owned uncontrolled Cave", "")
                .with_subtypes(vec!["Cave"])
                .controlled_by(P1);
            scenario
                .add_land_to_hand(P0, "Hand Cave")
                .with_subtypes(vec!["Cave"]);
            scenario.add_land_to_graveyard(P0, "Ordinary land");
        }
        let mut runner = scenario.build();
        if let Some(id) = off_zone {
            runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
        }
        assert_eq!(runner.state().objects[&spell].static_definitions.len(), 1);
        let StaticMode::ModifyCost {
            dynamic_count: Some(quantity),
            ..
        } = &runner.state().objects[&spell].static_definitions[0].mode
        else {
            panic!("complete printed Cave population");
        };
        assert_eq!(
            resolve_quantity(
                runner.state(),
                &QuantityExpr::Ref {
                    qty: quantity.clone()
                },
                P0,
                spell
            ),
            if populated { 2 } else { 0 },
            "battlefield control and graveyard ownership have separate authorities"
        );
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
            Some(ManaCost::Cost {
                generic: if populated { 5 } else { 7 },
                shards: vec![ManaCostShard::Black]
            })
        );
        if colorless || short {
            assert!(runner.cast(spell).try_resolve().is_err());
            continue;
        }
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert!(outcome.state().objects[&spell]
            .keywords
            .contains(&Keyword::Lifelink));
    }
}

#[test]
fn independent_repeated_population_overlap() {
    // Synthetic expression witnesses: one object satisfies two independent each clauses.
    for second in ["creature", "artifact"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let member = scenario
            .add_creature(P0, "Artifact Creature", 2, 3)
            .as_artifact_creature()
            .id();
        let oracle =
            format!("Draw a card for each artifact you control and each {second} you control.");
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic independent Draw", false, &oracle)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        for i in 0..5 {
            scenario.add_spell_to_library_top(P0, &format!("Draw member {i}"), true);
        }
        let mut runner = scenario.build();
        let parsed = engine::parser::parse_oracle_text(
            &oracle,
            "Synthetic independent Draw",
            &[],
            &["Sorcery".into()],
            &[],
        );
        assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
        let outcome = runner.cast(spell).resolve();
        outcome.assert_hand_drawn(P0, 2);
        assert_eq!(outcome.state().objects[&member].power, Some(2));
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let member = scenario
        .add_creature(P0, "Anthem Artifact Creature", 2, 3)
        .as_artifact_creature()
        .id();
    scenario.add_enchantment_from_oracle(P0, "Synthetic independent anthem", "Creatures you control get +1/+1 for each artifact you control and each creature you control.");
    let marker = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic flush", false, "You gain 1 life.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(marker)
        .resolve()
        .assert_power_toughness(member, 4, 5);
}

#[test]
fn repeated_population_full_source_honesty() {
    for (text, name, types, keywords) in [
        (
            DISMISSAL,
            "Machinist's Dismissal",
            vec!["Instant".into()],
            vec![],
        ),
        (
            LEECH,
            "Gargantuan Leech",
            vec!["Creature".into()],
            vec!["Lifelink".into()],
        ),
    ] {
        let parsed = engine::parser::parse_oracle_text(text, name, &keywords, &types, &[]);
        assert_eq!(parsed.statics.len(), 1, "{parsed:#?}");
        assert!(
            matches!(&parsed.statics[0].mode, StaticMode::ModifyCost { dynamic_count: Some(QuantityRef::ObjectCount { filter: TargetFilter::Or { filters } }), .. } if filters.len() == 2)
        );
        assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
        if name == "Machinist's Dismissal" {
            assert_eq!(parsed.abilities.len(), 1);
        } else {
            assert!(parsed.extracted_keywords.contains(&Keyword::Lifelink));
        }
    }
    for body in [
        "artifact you control and each creature you control",
        "artifact you control and each artifact you control",
        "artifact you control and each mystery in your hand",
        "artifact you control and each artifact card in your hand and each",
        "artifact you control and each artifact card in your hand with unknown frosting",
        "card in exile and in your graveyard and each mystery you control",
        "artifact you control and eachness artifact card in your hand",
    ] {
        let text = format!("This spell costs {{1}} less to cast for each {body}.");
        let parsed = engine::parser::parse_oracle_text(
            &text,
            "Synthetic unsupported scalar",
            &[],
            &["Sorcery".into()],
            &[],
        );
        assert!(
            parsed.statics.is_empty(),
            "must not admit a partial/fixed discount: {body}: {parsed:#?}"
        );
        assert!(parsed.abilities.iter().any(|ability| matches!(ability.effect.as_ref(), Effect::Unimplemented { description: Some(description), .. } if description.trim_end_matches('.') == text.trim_end_matches('.') && ability.description.as_deref() == Some(text.as_str()))), "full source must remain visible: {parsed:#?}");
    }
}

#[test]
fn repeated_population_generalized_zone_families() {
    for (body, generic, expected) in [
        ("artifact card in your hand and each artifact you control", 4, 2),
        ("creature your opponents control and each creature card in your graveyard and each artifact card in your hand", 5, 2),
        ("card you own in exile and in your graveyard and each artifact you control", 5, 2),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, expected);
        let text = format!("This spell costs {{1}} less to cast for each {body}.\nYou gain 1 life.");
        let spell = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic disjoint family", false, &text).with_mana_cost(ManaCost::generic(generic)).id();
        scenario.add_artifact_from_oracle(P0, "Family battlefield artifact", "");
        scenario.add_artifact_to_hand_from_oracle(P0, "Family hand artifact", "");
        scenario.add_creature(P1, "Family opponent creature", 1, 1);
        scenario.add_creature_to_graveyard(P0, "Family graveyard creature", 1, 1);
        scenario.add_spell_to_exile(P0, "Family exiled instant", true);
        let mut runner = scenario.build();
        assert!(runner.state().objects[&spell].parse_warnings.is_empty());
        let outcome = runner.cast(spell).resolve();
        assert_eq!(outcome.mana_pool_total(P0), 0);
        outcome.assert_life_delta(P0, 1);
    }
}

fn begin_manual(runner: &mut GameRunner, spell: ObjectId) {
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
    assert!(runner.state().objects[&spell].spell_announcement.is_some());
    assert!(runner.state().stack.iter().any(|entry| entry.id == spell));
}

#[test]
fn pending_population_lock_payment_and_cancel() {
    for real_ability in [false, true] {
        for has_mana_source in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let text = if real_ability {
                format!("{REPEATED}\nYou gain 1 life.")
            } else {
                REPEATED.into()
            };
            let spell = scenario
                .add_artifact_to_hand_from_oracle(P0, "Synthetic locked artifact", &text)
                .with_mana_cost(ManaCost::generic(4))
                .id();
            let source = scenario
                .add_artifact_from_oracle(
                    P0,
                    "Synthetic sacrificial mana artifact",
                    if has_mana_source {
                        "Sacrifice this artifact: Add {C}{C}."
                    } else {
                        ""
                    },
                )
                .id();
            scenario.add_artifact_to_hand_from_oracle(P0, "Remaining hand artifact", "");
            let mut runner = scenario.build();
            assert_eq!(
                runner.state().objects[&spell].abilities.is_empty(),
                !real_ability
            );
            begin_manual(&mut runner, spell);
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { .. }
            ));
            if has_mana_source {
                // Confirming payment may activate the available source. An empty
                // starting pool is not an insufficient-payment fixture in this case.
                let mut automatic = GameRunner::from_state(runner.state().clone());
                automatic.act(GameAction::PassPriority).unwrap();
                assert!(automatic.state().pending_cast.is_none());
                assert_eq!(automatic.state().objects[&source].zone, Zone::Graveyard);
                assert_eq!(
                    automatic.state().objects[&spell].mana_spent_to_cast_amount,
                    2
                );
                assert_eq!(automatic.state().players[0].mana_pool.total(), 0);
            } else {
                assert!(
                    runner.act(GameAction::PassPriority).is_err(),
                    "an empty pool with no mana ability cannot pay the locked cost"
                );
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ManaPayment { .. }
                ));
                assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
                assert_eq!(runner.state().objects[&spell].mana_spent_to_cast_amount, 0);
            }
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
            runner.act(GameAction::CancelCast).unwrap();
            assert!(runner.state().pending_cast.is_none());
            assert!(runner.state().objects[&spell].spell_announcement.is_none());
            assert_eq!(count(runner.state(), spell, artifact_in(Zone::Hand)), 2);
            assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 0);
            if !has_mana_source {
                continue;
            }
            begin_manual(&mut runner, spell);
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
            runner
                .act(GameAction::ActivateAbility {
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert_eq!(
                count(runner.state(), spell, artifact_in(Zone::Battlefield)),
                0
            );
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
            runner.act(GameAction::PassPriority).unwrap();
            assert!(runner.state().pending_cast.is_none());
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            runner.resolve_top();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
        }
    }
}

fn population_modifier(dynamic: bool) -> engine::types::ability::StaticDefinition {
    let parsed = engine::parser::parse_oracle_text(
        REPEATED,
        "Synthetic population source",
        &[],
        &["Artifact".into()],
        &[],
    );
    let mut definition = parsed
        .statics
        .into_iter()
        .next()
        .expect("complete population static");
    definition.affected = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::You),
    ));
    // CR 113.6: board and object-granted abilities use battlefield default.
    definition.active_zones.clear();
    if !dynamic {
        let StaticMode::ModifyCost { dynamic_count, .. } = &mut definition.mode else {
            unreachable!()
        };
        *dynamic_count = None;
    }
    definition
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApplicabilitySource {
    SelfSpell,
    Battlefield,
    Command,
    ObjectGrant,
    TransientDefinition,
    TransientGate,
    TransientDuration,
}

fn applicability_fixture(
    source_kind: ApplicabilitySource,
    generic: u32,
    dynamic: bool,
    condition: Option<StaticCondition>,
    filter: Option<TargetFilter>,
) -> (GameRunner, ObjectId) {
    use engine::types::ability::StaticDefinition;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut definition = population_modifier(dynamic);
    let mut gate = None;
    let mut duration = Duration::UntilEndOfTurn;
    match source_kind {
        ApplicabilitySource::TransientGate => gate = condition,
        ApplicabilitySource::TransientDuration => {
            duration = Duration::ForAsLongAs {
                condition: condition.expect("duration needs its initial true condition"),
            };
        }
        ApplicabilitySource::SelfSpell
        | ApplicabilitySource::Battlefield
        | ApplicabilitySource::Command
        | ApplicabilitySource::ObjectGrant
        | ApplicabilitySource::TransientDefinition => definition.condition = condition,
    }
    let StaticMode::ModifyCost { spell_filter, .. } = &mut definition.mode else {
        unreachable!()
    };
    *spell_filter = filter;
    if source_kind == ApplicabilitySource::SelfSpell {
        definition.affected = Some(TargetFilter::SelfRef);
        definition.active_zones = engine::types::zones::self_spell_cost_mod_active_zones();
    } else if source_kind == ApplicabilitySource::Command {
        definition.active_zones = vec![Zone::Command];
    }
    let spell = if source_kind == ApplicabilitySource::SelfSpell {
        scenario
            .add_artifact_to_hand_from_oracle(P0, "Synthetic applicability spell", "")
            .with_mana_cost(ManaCost::generic(generic))
            .with_static_definition(definition.clone())
            .id()
    } else {
        scenario
            .add_artifact_to_hand_from_oracle(P0, "Synthetic applicability spell", "")
            .with_mana_cost(ManaCost::generic(generic))
            .id()
    };
    // The spell is the third hand artifact before announcement. Together
    // with the battlefield artifact it makes the repeated population four.
    scenario.add_artifact_to_hand_from_oracle(P0, "Applicability hand member one", "");
    scenario.add_artifact_to_hand_from_oracle(P0, "Applicability hand member two", "");
    scenario.add_artifact_from_oracle(P0, "Applicability battlefield member", "");
    let host = if matches!(
        source_kind,
        ApplicabilitySource::Battlefield | ApplicabilitySource::Command
    ) {
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic applicability host", "")
            .with_static_definition(definition.clone())
            .id()
    } else {
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic applicability host", "")
            .id()
    };
    if source_kind == ApplicabilitySource::ObjectGrant {
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic applicability granter", "")
            .with_static_definition(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: host })
                    .modifications(vec![ContinuousModification::GrantStaticAbility {
                        definition: Box::new(definition.clone()),
                    }]),
            );
    }
    let mut runner = scenario.build();
    if source_kind == ApplicabilitySource::Command {
        engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Command, &mut Vec::new());
    } else if matches!(
        source_kind,
        ApplicabilitySource::TransientDefinition
            | ApplicabilitySource::TransientGate
            | ApplicabilitySource::TransientDuration
    ) {
        let id = runner
            .state_mut()
            .add_transient_continuous_effect(
                host,
                P0,
                duration,
                TargetFilter::SpecificPlayer { id: P0 },
                vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(definition),
                }],
                gate,
            )
            .expect("the public producer installs this effect before announcement");
        assert!(runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| effect.id == id));
        assert!(runner.state().objects[&spell].static_definitions.is_empty());
    }
    // Settle the fixture's real layer-6 grants before reading a pure preview.
    engine::game::layers::flush_layers(runner.state_mut());
    (runner, spell)
}

fn assert_applicability_payment(
    mut runner: GameRunner,
    spell: ObjectId,
    preview: u32,
    locked: u32,
    source_kind: ApplicabilitySource,
) {
    let functioning_sources =
        engine::game::functioning_abilities::game_functioning_statics(runner.state())
            .filter(|(_, definition)| matches!(definition.mode, StaticMode::ModifyCost { .. }))
            .count();
    assert_eq!(
        functioning_sources,
        usize::from(matches!(
            source_kind,
            ApplicabilitySource::Battlefield
                | ApplicabilitySource::Command
                | ApplicabilitySource::ObjectGrant
        )),
        "{source_kind:?}: the intended object-source class must actually function"
    );
    assert_eq!(
        engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
        Some(ManaCost::generic(preview)),
        "{source_kind:?}: preview must reach the opposite predicate result"
    );
    begin_manual(&mut runner, spell);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Hand)), 2);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 1);
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(locked),
        "{source_kind:?}: announcement changes applicability"
    );
    // CR 601.2f + CR 601.2h: pay the newly locked amount exactly; neither
    // a stale free preview nor a stale full-cost preview can satisfy this.
    runner.state_mut().players[0].mana_pool = (0..locked)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect();
    runner.act(GameAction::PassPriority).unwrap();
    assert!(runner.state().pending_cast.is_none(), "{source_kind:?}");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{source_kind:?}"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Battlefield,
        "{source_kind:?}"
    );
}

#[test]
fn pending_population_condition_applicability_crosses_announcement() {
    use engine::types::ability::Comparator;
    for source_kind in [
        ApplicabilitySource::SelfSpell,
        ApplicabilitySource::Battlefield,
        ApplicabilitySource::Command,
        ApplicabilitySource::ObjectGrant,
        ApplicabilitySource::TransientDefinition,
        ApplicabilitySource::TransientGate,
        ApplicabilitySource::TransientDuration,
    ] {
        for disappears in [true, false] {
            // A false initial ForAsLongAs duration must never start. Exercise
            // that source's actual true-to-false lifetime only.
            if source_kind == ApplicabilitySource::TransientDuration && !disappears {
                continue;
            }
            let condition = StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: artifact_in(Zone::Hand),
                    },
                },
                comparator: if disappears {
                    Comparator::GE
                } else {
                    Comparator::LE
                },
                rhs: QuantityExpr::Fixed {
                    value: if disappears { 3 } else { 2 },
                },
            };
            // Nested conditions use the same conservative applicability proof.
            let condition = StaticCondition::And {
                conditions: vec![StaticCondition::Not {
                    condition: Box::new(StaticCondition::Not {
                        condition: Box::new(condition),
                    }),
                }],
            };
            let (generic, preview, locked) = if disappears { (4, 0, 4) } else { (2, 2, 1) };
            let (runner, spell) =
                applicability_fixture(source_kind, generic, disappears, Some(condition), None);
            assert_applicability_payment(runner, spell, preview, locked, source_kind);
        }
    }
}

#[test]
fn pending_population_spell_filter_applicability_crosses_announcement() {
    use engine::types::ability::Comparator;
    for source_kind in [
        ApplicabilitySource::SelfSpell,
        ApplicabilitySource::Battlefield,
        ApplicabilitySource::Command,
        ApplicabilitySource::ObjectGrant,
        ApplicabilitySource::TransientDefinition,
    ] {
        for disappears in [true, false] {
            let definition = population_modifier(true);
            let StaticMode::ModifyCost {
                dynamic_count: Some(repeated),
                ..
            } = definition.mode
            else {
                unreachable!()
            };
            let threshold = if disappears {
                repeated
            } else {
                QuantityRef::ObjectCount {
                    filter: artifact_in(Zone::Hand),
                }
            };
            // Nested filter logic must not use today's false result to erase
            // yesterday's contribution, or to miss tomorrow's fixed reduction.
            let filter = TargetFilter::And {
                filters: vec![TargetFilter::Not {
                    filter: Box::new(TargetFilter::Not {
                        filter: Box::new(TargetFilter::Typed(TypedFilter::default().properties(
                            vec![FilterProp::Cmc {
                                comparator: if disappears {
                                    Comparator::LE
                                } else {
                                    Comparator::GE
                                },
                                value: QuantityExpr::Ref { qty: threshold },
                            }],
                        ))),
                    }),
                }],
            };
            let (generic, preview, locked) = if disappears { (4, 0, 4) } else { (2, 2, 1) };
            let (runner, spell) =
                applicability_fixture(source_kind, generic, disappears, None, Some(filter));
            assert_applicability_payment(runner, spell, preview, locked, source_kind);
        }
    }
}

#[test]
fn pending_population_transient_grant_lock() {
    for mode in ["dynamic", "fixed", "expired", "wrong caster", "wrong spell"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_artifact_to_hand_from_oracle(P0, "Synthetic transient-only artifact", "")
            .with_mana_cost(ManaCost::generic(4))
            .id();
        let source = scenario
            .add_artifact_from_oracle(
                P0,
                "Synthetic temporary grant source",
                "Sacrifice this artifact: Add {C}{C}.",
            )
            .id();
        scenario.add_artifact_to_hand_from_oracle(P0, "Transient hand member", "");
        let mut runner = scenario.build();
        let mut definition = population_modifier(mode != "fixed");
        if mode == "wrong spell" {
            let StaticMode::ModifyCost { spell_filter, .. } = &mut definition.mode else {
                unreachable!()
            };
            *spell_filter = Some(TargetFilter::Typed(TypedFilter::creature()));
        }
        let controller = if mode == "wrong caster" { P1 } else { P0 };
        let effect_id = runner
            .state_mut()
            .add_transient_continuous_effect(
                source,
                controller,
                Duration::UntilHostLeavesPlay,
                TargetFilter::SpecificPlayer { id: controller },
                vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(definition),
                }],
                None,
            )
            .unwrap();
        engine::game::layers::flush_layers(runner.state_mut());
        assert!(runner.state().transient_continuous_effects.iter().any(|effect| effect.id == effect_id && matches!(effect.affected, TargetFilter::SpecificPlayer { id } if id == controller)));
        assert!(
            !engine::game::functioning_abilities::game_functioning_statics(runner.state())
                .any(|(_, definition)| matches!(definition.mode, StaticMode::ModifyCost { .. }))
        );
        assert!(runner.state().objects[&spell].static_definitions.is_empty());
        assert!(runner.state().objects[&source]
            .static_definitions
            .is_empty());
        if mode == "expired" {
            // The producer installed a real live effect. Retain its lapsed
            // entry to exercise liveness before the ordinary pruning sweep.
            runner.state_mut().objects.get_mut(&source).unwrap().zone = Zone::Graveyard;
        }
        let expected = match mode {
            "dynamic" => 2,
            "fixed" => 3,
            _ => 4,
        };
        if mode != "dynamic" {
            runner.state_mut().players[0].mana_pool = (0..expected)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .collect();
        }
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
            Some(ManaCost::generic(if mode == "dynamic" {
                1
            } else {
                expected
            })),
            "{mode}: the public grant supplies the pre-announcement preview"
        );
        begin_manual(&mut runner, spell);
        assert!(
            !engine::game::functioning_abilities::game_functioning_statics(runner.state())
                .any(|(_, definition)| matches!(definition.mode, StaticMode::ModifyCost { .. }))
        );
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            ManaCost::generic(expected)
        );
        if mode == "dynamic" {
            runner
                .act(GameAction::ActivateAbility {
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            assert_eq!(
                count(runner.state(), spell, artifact_in(Zone::Battlefield)),
                0
            );
            assert!(!runner
                .state()
                .transient_continuous_effects
                .iter()
                .any(|effect| effect.id == effect_id));
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
        }
        runner.act(GameAction::PassPriority).unwrap();
        assert!(runner.state().pending_cast.is_none());
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
    }
}

#[test]
fn pending_population_functioning_modifier_sources() {
    use engine::types::ability::StaticDefinition;
    for source_kind in ["battlefield", "command", "object grant"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_artifact_to_hand_from_oracle(P0, "Synthetic functioning-source artifact", "")
            .with_mana_cost(ManaCost::generic(4))
            .id();
        let mana_source = scenario
            .add_artifact_from_oracle(
                P0,
                "Functioning-source mana artifact",
                "Sacrifice this artifact: Add {C}{C}.",
            )
            .id();
        scenario.add_artifact_to_hand_from_oracle(P0, "Functioning-source hand member", "");
        let mut definition = population_modifier(true);
        if source_kind == "command" {
            definition.active_zones = vec![Zone::Command];
        }
        let host = if source_kind == "object grant" {
            scenario
                .add_enchantment_from_oracle(P0, "Synthetic modifier host", "")
                .id()
        } else {
            scenario
                .add_enchantment_from_oracle(P0, "Synthetic modifier host", "")
                .with_static_definition(definition.clone())
                .id()
        };
        if source_kind == "object grant" {
            scenario
                .add_enchantment_from_oracle(P0, "Synthetic object-bound granter", "")
                .with_static_definition(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SpecificObject { id: host })
                        .modifications(vec![ContinuousModification::GrantStaticAbility {
                            definition: Box::new(definition),
                        }]),
                );
        }
        let mut runner = scenario.build();
        if source_kind == "command" {
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                host,
                Zone::Command,
                &mut Vec::new(),
            );
        }
        engine::game::layers::flush_layers(runner.state_mut());
        assert_eq!(
            engine::game::functioning_abilities::game_functioning_statics(runner.state())
                .filter(|(object, definition)| {
                    object.id == host && matches!(definition.mode, StaticMode::ModifyCost { .. })
                })
                .count(),
            1,
            "{source_kind}: the intended host must supply the modifier"
        );
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
            Some(ManaCost::generic(1)),
            "{source_kind}: preview counts the unannounced hand artifact"
        );
        begin_manual(&mut runner, spell);
        assert!(runner.state().objects[&spell].static_definitions.is_empty());
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            ManaCost::generic(2),
            "{source_kind}"
        );
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: mana_source,
                ability_index: 0,
            })
            .unwrap();
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            ManaCost::generic(2)
        );
        runner.act(GameAction::PassPriority).unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
    }
}

#[test]
fn pending_population_modifier_order() {
    use engine::types::ability::StaticDefinition;
    use engine::types::statics::CostReductionReach;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 5);
    let spell = scenario
        .add_artifact_to_hand_from_oracle(P0, "Synthetic floor artifact", REPEATED)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.add_artifact_from_oracle(P0, "Floor battlefield member", "");
    scenario.add_artifact_to_hand_from_oracle(P0, "Floor hand member", "");
    for (mode, amount) in [(CostModifyMode::Raise, 2), (CostModifyMode::Minimum, 5)] {
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic cost modifier", "")
            .with_static_definition(StaticDefinition::new(StaticMode::ModifyCost {
                mode,
                amount: ManaCost::generic(amount),
                spell_filter: None,
                dynamic_count: None,
                reach: CostReductionReach::SpillsToGeneric,
            }));
    }
    let mut runner = scenario.build();
    begin_manual(&mut runner, spell);
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(5)
    );
    runner.act(GameAction::PassPriority).unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
}

fn alternative_permission(
    granted_to: engine::types::player::PlayerId,
    base: u32,
    rider: u32,
) -> engine::types::ability::CastingPermission {
    use engine::types::ability::{CastCostModifier, CastingPermission, ExileGrantCostProvenance};
    CastingPermission::ExileWithAltCost {
        cost: ManaCost::generic(base),
        cost_provenance: ExileGrantCostProvenance::Alternative,
        cast_transformed: false,
        constraint: None,
        granted_to: Some(granted_to),
        resolution_cleanup: None,
        duration: None,
        source_id: None,
        graveyard_replacement: None,
        enters_with_counter: None,
        enters_with_modifications: Default::default(),
        mana_spend_permission: None,
        cast_cost_modifier: Some(CastCostModifier::reduce(ManaCost::generic(rider))),
    }
}

fn assert_pending_reader_authorities(
    runner: &GameRunner,
    spell: ObjectId,
    origin: Zone,
    record: &engine::types::game_state::ZoneChangeRecord,
) {
    use engine::game::filter::{
        matches_target_filter, matches_target_filter_on_zone_change_record, FilterContext,
    };
    let ctx = FilterContext::from_source_with_controller(spell, P0);
    assert_eq!(runner.state().objects[&spell].owner, P1);
    assert_eq!(
        runner.state().objects[&spell].controller,
        P1,
        "retained origin still has its original facts"
    );
    let exact = |filter| TargetFilter::And {
        filters: vec![TargetFilter::SpecificObject { id: spell }, filter],
    };
    assert_eq!(
        count(runner.state(), spell, exact(artifact_in(Zone::Stack))),
        1
    );
    assert_eq!(count(runner.state(), spell, exact(artifact_in(origin))), 0);
    assert!(matches_target_filter(
        runner.state(),
        spell,
        &artifact_in(origin),
        &ctx
    ));
    assert!(!matches_target_filter(
        runner.state(),
        spell,
        &artifact_in(Zone::Stack),
        &ctx
    ));
    assert!(matches_target_filter_on_zone_change_record(
        runner.state(),
        record,
        &artifact_in(origin),
        &ctx
    ));
    assert!(!matches_target_filter_on_zone_change_record(
        runner.state(),
        record,
        &artifact_in(Zone::Stack),
        &ctx
    ));
    let stack_scope = |controller| {
        TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact)
                .controller(controller)
                .properties(vec![FilterProp::InZone { zone: Zone::Stack }]),
        )
    };
    assert_eq!(
        count(
            runner.state(),
            spell,
            exact(stack_scope(ControllerRef::You))
        ),
        1
    );
    assert_eq!(
        count(
            runner.state(),
            spell,
            exact(stack_scope(ControllerRef::Opponent))
        ),
        0
    );
    let owner = TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Owned {
        controller: ControllerRef::Opponent,
    }]));
    assert_eq!(
        count(
            runner.state(),
            spell,
            exact(TargetFilter::And {
                filters: vec![
                    TargetFilter::Or {
                        filters: vec![stack_scope(ControllerRef::You), artifact_in(origin)]
                    },
                    TargetFilter::Not {
                        filter: Box::new(stack_scope(ControllerRef::Opponent))
                    },
                    owner,
                ]
            })
        ),
        1
    );
}

#[test]
fn pending_population_selected_permission_cost() {
    use engine::types::ability::{CastingPermission, TargetRef};
    use engine::types::game_state::CastingPermissionIndex;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 3);
    let text = format!("{REPEATED}\nTarget player gains 1 life.");
    let spell = scenario
        .add_creature_to_exile(P1, "Synthetic foreign permission artifact", 2, 2)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(9))
        .from_oracle_text(&text)
        .with_keyword(Keyword::Flash)
        .id();
    scenario.add_artifact_from_oracle(P0, "Permission battlefield member", "");
    scenario.add_artifact_to_hand_from_oracle(P0, "Permission hand member", "");
    let unrelated = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic unrelated spell", true, "You gain 2 life.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let ability_source = scenario
        .add_creature_from_oracle(
            P0,
            "Synthetic unrelated activation",
            1,
            1,
            "{0}: You gain 1 life.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(unrelated).commit();
    runner
        .state_mut()
        .stack
        .iter_mut()
        .find(|entry| entry.id == unrelated)
        .unwrap()
        .controller = P1;
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: ability_source,
            ability_index: 0,
        })
        .unwrap();
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions = vec![
        alternative_permission(P1, 1, 0),
        alternative_permission(P0, 6, 1),
    ];
    let record = runner.state().objects[&spell].snapshot_for_zone_change(
        spell,
        Some(Zone::Exile),
        Zone::Stack,
    );
    let before = runner.state().stack.len();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!("selected permission must pause for the printed target");
    };
    assert_eq!(
        pending_cast.casting_permission_index,
        Some(CastingPermissionIndex(1))
    );
    assert_eq!(pending_cast.base_cost, Some(ManaCost::generic(6)));
    let CastingPermission::ExileWithAltCost { granted_to, .. } = &mut runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions[0]
    else {
        unreachable!()
    };
    *granted_to = Some(P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .unwrap();
    assert_eq!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .casting_permission_index,
        Some(CastingPermissionIndex(1)),
        "a newly eligible sibling cannot replace the selected authority"
    );
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().base_cost,
        Some(ManaCost::generic(6))
    );
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(3)
    );
    assert_pending_reader_authorities(&runner, spell, Zone::Exile, &record);
    runner.act(GameAction::CancelCast).unwrap();
    assert_eq!(runner.state().stack.len(), before);
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.id == unrelated && entry.controller == P1));
    assert_eq!(
        count(
            runner.state(),
            spell,
            TargetFilter::And {
                filters: vec![
                    artifact_in(Zone::Stack),
                    TargetFilter::SpecificObject { id: spell }
                ]
            }
        ),
        0
    );
    assert_eq!(runner.state().objects[&spell].owner, P1);
    // Restore the original eligibility and cast anew through the fluent driver.
    let CastingPermission::ExileWithAltCost { granted_to, .. } = &mut runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions[0]
    else {
        unreachable!()
    };
    *granted_to = Some(P1);
    let outcome = runner.cast(spell).target_player(P0).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    outcome.assert_zone(&[spell], Zone::Battlefield);
}

#[test]
fn pending_foreign_spell_controller() {
    use engine::types::ability::{CardPlayMode, StaticDefinition};
    use engine::types::statics::{CastFrequency, GraveyardPermissionPool};
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 2);
    let spell = scenario
        .add_creature_to_graveyard(P1, "Synthetic foreign graveyard artifact", 2, 2)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text(REPEATED)
        .id();
    scenario.add_artifact_from_oracle(P0, "Graveyard battlefield member", "");
    scenario.add_artifact_to_hand_from_oracle(P0, "Graveyard hand member", "");
    scenario
        .add_enchantment_from_oracle(P0, "Synthetic any-graveyard permission", "")
        .with_static_definition(
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
                extra_cost: None,
                enters_with_counter: None,
                required_cast_keyword: None,
                pool: GraveyardPermissionPool::AnyGraveyard,
            })
            .affected(TargetFilter::Typed(TypedFilter::creature())),
        );
    let mut runner = scenario.build();
    let record = runner.state().objects[&spell].snapshot_for_zone_change(
        spell,
        Some(Zone::Graveyard),
        Zone::Stack,
    );
    begin_manual(&mut runner, spell);
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(2)
    );
    assert_pending_reader_authorities(&runner, spell, Zone::Graveyard, &record);
    runner.act(GameAction::CancelCast).unwrap();
    assert_eq!(runner.state().objects[&spell].owner, P1);
    let outcome = runner.cast(spell).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    outcome.assert_zone(&[spell], Zone::Battlefield);
}

#[test]
fn pending_population_reader_authorities() {
    use engine::game::filter::{
        matches_target_filter, matches_target_filter_on_zone_change_record, FilterContext,
    };
    use engine::types::ability::Comparator;
    use engine::types::game_state::{CastingVariant, StackEntryKind};
    use engine::types::mana::ManaColor;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 4);
    let spell = scenario
        .add_artifact_to_hand_from_oracle(P0, "Synthetic characteristic carrier", "")
        .with_keyword(Keyword::Flash)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let other = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic competing spell", true, "You gain 1 life.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(other).commit();
    let other_index = runner
        .state()
        .stack
        .iter()
        .position(|entry| entry.id == other)
        .unwrap();
    let unrelated_ability = runner.state().stack[other_index].ability().unwrap().clone();
    if let StackEntryKind::Spell {
        casting_variant, ..
    } = &mut runner.state_mut().stack[other_index].kind
    {
        *casting_variant = CastingVariant::Fuse;
    }
    let record = runner.state().objects[&spell].snapshot_for_zone_change(
        spell,
        Some(Zone::Hand),
        Zone::Stack,
    );
    begin_manual(&mut runner, spell);
    let index = runner
        .state()
        .stack
        .iter()
        .position(|entry| entry.id == spell)
        .unwrap();
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .back_face = Some(BackFaceData {
        name: "Synthetic other Split half".into(),
        layout_kind: Some(LayoutKind::Split),
        mana_cost: ManaCost::generic(3),
        color: vec![ManaColor::Red],
        ..Default::default()
    });
    let characteristic_filter = TargetFilter::And {
        filters: vec![
            artifact_in(Zone::Stack),
            TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 5 },
                },
                FilterProp::HasColor {
                    color: ManaColor::Red,
                },
            ])),
        ],
    };
    // The unrelated Fuse entry cannot supply this spell's characteristics.
    assert_eq!(
        count(runner.state(), spell, characteristic_filter.clone()),
        0
    );
    if let StackEntryKind::Spell {
        casting_variant, ..
    } = &mut runner.state_mut().stack[index].kind
    {
        *casting_variant = CastingVariant::Fuse;
    }
    assert_eq!(count(runner.state(), spell, characteristic_filter), 1);
    assert!(
        !runner.state().objects[&spell].fused_split_spell,
        "projection never persists the hint early"
    );
    let ctx = FilterContext::from_source_with_controller(spell, P0);
    assert!(matches_target_filter(
        runner.state(),
        spell,
        &artifact_in(Zone::Hand),
        &ctx
    ));
    assert!(matches_target_filter_on_zone_change_record(
        runner.state(),
        &record,
        &artifact_in(Zone::Hand),
        &ctx
    ));
    assert!(!matches_target_filter_on_zone_change_record(
        runner.state(),
        &record,
        &artifact_in(Zone::Stack),
        &ctx
    ));
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 1);
    let announcement = runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .spell_announcement
        .take();
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 0);
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .spell_announcement = announcement;
    runner.state_mut().stack[index].kind = StackEntryKind::ActivatedAbility {
        source_id: spell,
        ability: Box::new(unrelated_ability),
    };
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 0);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Hand)), 1);
}

// Exact Oracle and face metadata from AtomicCards 2026-10-04, SHA256
// EE8B89375A0328A534153B1121E4272050AEF69CD49C58387AEF0877D34E4F8E.
const EXPORT_ROUTE_PRINTED: &[&str] = &[
    r####"{"colorIdentity":["W"],"colors":["W"],"identifiers":{"scryfallOracleId":"f86bcdde-8b1f-4efa-a01d-39e09dc826d0"},"layout":"normal","manaCost":"{1}{W}","manaValue":2.0,"name":"Angelic Renewal","subtypes":[],"supertypes":[],"text":"Whenever a creature is put into your graveyard from the battlefield, you may sacrifice this enchantment. If you do, return that card to the battlefield.","type":"Enchantment","types":["Enchantment"]}"####,
    r####"{"colorIdentity":[],"colors":[],"identifiers":{"scryfallOracleId":"f6de5bd7-7704-4a0c-a27a-9e565e49f5e9"},"layout":"normal","manaCost":"{3}","manaValue":3.0,"name":"Dragonstorm Globe","subtypes":[],"supertypes":[],"text":"Each Dragon you control enters with an additional +1/+1 counter on it.\n{T}: Add one mana of any color.","type":"Artifact","types":["Artifact"]}"####,
    r####"{"colorIdentity":["W"],"colors":["W"],"identifiers":{"scryfallOracleId":"b4dbbf56-d7df-4183-bb48-cfc6b4d0468f"},"keywords":["Flying"],"layout":"normal","manaCost":"{3}{W}","manaValue":4.0,"name":"Armored Skyhunter","power":"3","subtypes":["Cat","Knight"],"supertypes":[],"text":"Flying\nWhenever this creature attacks, look at the top six cards of your library. You may put an Aura or Equipment card from among them onto the battlefield. If an Equipment is put onto the battlefield this way, you may attach it to a creature you control. Put the rest of those cards on the bottom of your library in a random order.","toughness":"3","type":"Creature — Cat Knight","types":["Creature"]}"####,
    r####"{"colorIdentity":["W"],"colors":["W"],"identifiers":{"scryfallOracleId":"77322d69-330e-4b00-b984-62c6d45c9540"},"keywords":["Food"],"layout":"saga","manaCost":"{3}{W}","manaValue":4.0,"name":"Vault 101: Birthday Party","subtypes":["Saga"],"supertypes":[],"text":"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Create a 1/1 white Human Soldier creature token and a Food token. (A Food token is an artifact with \"{2}, {T}, Sacrifice this token: You gain 3 life.\")\nII, III — You may put an Aura or Equipment card from your hand or graveyard onto the battlefield. If an Equipment is put onto the battlefield this way, you may attach it to a creature you control.","type":"Enchantment — Saga","types":["Enchantment"]}"####,
    r####"{"colorIdentity":["B"],"colors":["B"],"identifiers":{"scryfallOracleId":"ebbd5b9c-07ff-4b42-81c2-ba09539f3a42"},"keywords":["Mill"],"layout":"normal","manaCost":"{5}{B}{B}","manaValue":7.0,"name":"Breach the Multiverse","subtypes":[],"supertypes":[],"text":"Each player mills ten cards. For each player, choose a creature or planeswalker card in that player's graveyard. Put those cards onto the battlefield under your control. Then each creature you control becomes a Phyrexian in addition to its other types.","type":"Sorcery","types":["Sorcery"]}"####,
    r####"{"colorIdentity":["B"],"colors":["B"],"identifiers":{"scryfallOracleId":"cc9f9f14-a11c-4e33-9e11-8bf6ef13cf2b"},"layout":"normal","manaCost":"{3}{B}{B}","manaValue":5.0,"name":"Ghouls' Night Out","subtypes":[],"supertypes":[],"text":"For each player, choose a creature card in that player's graveyard. Put those cards onto the battlefield under your control. They're black Zombies in addition to their other colors and types and they gain decayed. (A creature with decayed can't block. When it attacks, sacrifice it at end of combat.)","type":"Sorcery","types":["Sorcery"]}"####,
    r###"{"name":"Abandoned Campground","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["U","W"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {W} or {U}.","identifiers":{"scryfallOracleId":"0eec9984-cd11-4a52-9234-469c6a5fb9aa"}}"###,
    r###"{"name":"Arlinn, Voice of the Pack","manaCost":"{4}{G}{G}","manaValue":6.0,"type":"Legendary Planeswalker — Arlinn","types":["Planeswalker"],"subtypes":["Arlinn"],"supertypes":["Legendary"],"colors":["G"],"colorIdentity":["G"],"loyalty":"7","layout":"normal","text":"Each creature you control that's a Wolf or a Werewolf enters with an additional +1/+1 counter on it.\n[−2]: Create a 2/2 green Wolf creature token.","identifiers":{"scryfallOracleId":"a15b83df-91b7-49f0-9bec-5776a3dac8bd"}}"###,
    r###"{"name":"Ashling's Prerogative","manaCost":"{1}{R}","manaValue":2.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"As this enchantment enters, choose odd or even. (Zero is even.)\nEach creature with mana value of the chosen quality has haste.\nEach creature without mana value of the chosen quality enters tapped.","identifiers":{"scryfallOracleId":"b68bc097-482a-4fe5-9fca-df5941f3dc5c"}}"###,
    r###"{"name":"Bleeding Woods","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["G","R"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {R} or {G}.","identifiers":{"scryfallOracleId":"47b6d2ae-d3d7-41eb-9172-2076eb8d028d"}}"###,
    r###"{"name":"Bloatfly Swarm","manaCost":"{3}{B}","manaValue":4.0,"type":"Creature — Insect Mutant","types":["Creature"],"subtypes":["Insect","Mutant"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"0","toughness":"0","layout":"normal","text":"Flying\nThis creature enters with five +1/+1 counters on it.\nIf damage would be dealt to this creature while it has a +1/+1 counter on it, prevent that damage, remove that many +1/+1 counters from it, then give each player a rad counter for each +1/+1 counter removed this way.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"fc50e254-71bc-46b3-93e6-c6eb820a3473"}}"###,
    r###"{"name":"Blood Scrivener","manaCost":"{1}{B}","manaValue":2.0,"type":"Creature — Zombie Wizard","types":["Creature"],"subtypes":["Zombie","Wizard"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"1","layout":"normal","text":"If you would draw a card while you have no cards in hand, instead you draw two cards and you lose 1 life.","identifiers":{"scryfallOracleId":"af66da4b-2f36-4dd2-a5c6-18e6023c5aba"}}"###,
    r###"{"name":"Bountiful Promenade","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["G","W"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {G} or {W}.","identifiers":{"scryfallOracleId":"761cb262-f83b-4a99-9345-b773182a7671"}}"###,
    r###"{"name":"Buzzing Whack-a-Doodle","manaCost":"{4}","manaValue":4.0,"type":"Artifact","types":["Artifact"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"As this artifact enters, you and an opponent each secretly choose Whack or Doodle. Then those choices are revealed. If the choices match, this artifact has that ability. Otherwise, it has Buzz.\n• Whack — {T}: Target player loses 2 life.\n• Doodle — {T}: You gain 3 life.\n• Buzz — {2}, {T}: Draw a card.","identifiers":{"scryfallOracleId":"de13c8ab-295c-4561-830e-b5235a164c92"}}"###,
    r###"{"name":"Camato Scout","manaCost":"{1}{U}{U}","manaValue":3.0,"type":"Creature — Merfolk","types":["Creature"],"subtypes":["Merfolk"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"2","toughness":"3","layout":"normal","text":"As Camato Scout enters, choose a basic land type at random. Camato Scout has landwalk of the chosen type.","identifiers":{"scryfallOracleId":"2f05cfc0-6029-4618-8d79-39cd0a6a3b5f"}}"###,
    r###"{"name":"Etched Cornfield","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["G","W"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {G} or {W}.","identifiers":{"scryfallOracleId":"6ccca5c2-66c3-495a-8d9e-1a9805569e52"}}"###,
    r###"{"name":"Jace, Wielder of Mysteries","manaCost":"{1}{U}{U}{U}","manaValue":4.0,"type":"Legendary Planeswalker — Jace","types":["Planeswalker"],"subtypes":["Jace"],"supertypes":["Legendary"],"colors":["U"],"colorIdentity":["U"],"loyalty":"4","layout":"normal","text":"If you would draw a card while your library has no cards in it, you win the game instead.\n[+1]: Target player mills two cards. Draw a card.\n[−8]: Draw seven cards. Then if your library has no cards in it, you win the game.","keywords":["Mill"],"identifiers":{"scryfallOracleId":"5277a68e-1ceb-4b40-8e10-3d0564f5d7be"}}"###,
    r###"{"name":"Laboratory Maniac","manaCost":"{2}{U}","manaValue":3.0,"type":"Creature — Human Wizard","types":["Creature"],"subtypes":["Human","Wizard"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"2","toughness":"2","layout":"normal","text":"If you would draw a card while your library has no cards in it, you win the game instead.","identifiers":{"scryfallOracleId":"aa286dd5-aa19-446d-9003-684d81eb57ca"}}"###,
    r###"{"name":"Lakeside Shack","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["G","U"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {G} or {U}.","identifiers":{"scryfallOracleId":"c56cd2ec-5907-4282-9162-d93b7dfd63b5"}}"###,
    r###"{"name":"Living Conundrum","manaCost":"{4}{U}","manaValue":5.0,"type":"Creature — Elemental","types":["Creature"],"subtypes":["Elemental"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"2","toughness":"5","layout":"normal","text":"Hexproof\nIf you would draw a card while your library has no cards in it, skip that draw instead.\nAs long as there are no cards in your library, this creature has base power and toughness 10/10 and has flying and vigilance.","keywords":["Hexproof"],"identifiers":{"scryfallOracleId":"378ae023-04d3-44cc-9248-3d787796ed6c"}}"###,
    r###"{"name":"Luxury Suite","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","R"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {B} or {R}.","identifiers":{"scryfallOracleId":"819e1765-8325-4e6f-89c1-63ea86de369f"}}"###,
    r###"{"name":"Melira's Keepers","manaCost":"{4}{G}","manaValue":5.0,"type":"Creature — Human Warrior","types":["Creature"],"subtypes":["Human","Warrior"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"4","toughness":"4","layout":"normal","text":"This creature can't have counters put on it.","identifiers":{"scryfallOracleId":"6f2f4c31-74dc-4a52-96ad-94c3d51398d1"}}"###,
    r###"{"name":"Morphic Pool","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","U"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {U} or {B}.","identifiers":{"scryfallOracleId":"bd004c9d-771e-4e63-a97d-a2259c096af8"}}"###,
    r###"{"name":"Murky Sewer","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","U"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {U} or {B}.","identifiers":{"scryfallOracleId":"c2cdefeb-3176-4faf-be54-a62d31f777a5"}}"###,
    r###"{"name":"Neglected Manor","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","W"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {W} or {B}.","identifiers":{"scryfallOracleId":"c8c632ab-14ec-44e1-ac00-81d48336320d"}}"###,
    r###"{"name":"Oathsworn Knight","manaCost":"{1}{B}{B}","manaValue":3.0,"type":"Creature — Human Knight","types":["Creature"],"subtypes":["Human","Knight"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"0","toughness":"0","layout":"normal","text":"This creature enters with four +1/+1 counters on it.\nThis creature attacks each combat if able.\nIf damage would be dealt to this creature while it has a +1/+1 counter on it, prevent that damage and remove a +1/+1 counter from it.","identifiers":{"scryfallOracleId":"16c293c8-bd1d-4db9-b197-da14f2f37cb1"}}"###,
    r###"{"name":"Ormos, Archive Keeper","manaCost":"{4}{U}{U}","manaValue":6.0,"type":"Legendary Creature — Sphinx","types":["Creature"],"subtypes":["Sphinx"],"supertypes":["Legendary"],"colors":["U"],"colorIdentity":["U"],"power":"5","toughness":"5","layout":"normal","text":"Flying\nIf you would draw a card while your library has no cards in it, instead put five +1/+1 counters on Ormos.\n{1}{U}{U}, Discard three cards with different names: Draw five cards.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"06d60b71-a9f9-4a7f-ad69-15d760b6e5c3"}}"###,
    r###"{"name":"Out of the Tombs","manaCost":"{2}{B}","manaValue":3.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"At the beginning of your upkeep, put two eon counters on this enchantment, then mill cards equal to the number of eon counters on it.\nIf you would draw a card while your library has no cards in it, instead return a creature card from your graveyard to the battlefield. If you can't, you lose the game.","keywords":["Mill"],"identifiers":{"scryfallOracleId":"473a7998-669d-4181-83fe-f178a2e781f0"}}"###,
    r###"{"name":"Peculiar Lighthouse","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["R","U"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {U} or {R}.","identifiers":{"scryfallOracleId":"d55f7e20-11c6-44e2-8a21-dca67d3dbc68"}}"###,
    r###"{"name":"Phial of Galadriel","manaCost":"{3}","manaValue":3.0,"type":"Legendary Artifact","types":["Artifact"],"subtypes":[],"supertypes":["Legendary"],"colors":[],"colorIdentity":[],"layout":"normal","text":"If you would draw a card while you have no cards in hand, draw two cards instead.\nIf you would gain life while you have 5 or less life, you gain twice that much life instead.\n{T}: Add one mana of any color.","identifiers":{"scryfallOracleId":"08f17ebc-c0fd-493f-84b3-e9250694543e"}}"###,
    r###"{"name":"Polukranos, Unchained","manaCost":"{2}{B}{G}","manaValue":4.0,"type":"Legendary Creature — Zombie Hydra","types":["Creature"],"subtypes":["Zombie","Hydra"],"supertypes":["Legendary"],"colors":["B","G"],"colorIdentity":["B","G"],"power":"0","toughness":"0","layout":"normal","text":"Polukranos enters with six +1/+1 counters on it. It escapes with twelve +1/+1 counters on it instead.\nIf damage would be dealt to Polukranos while it has a +1/+1 counter on it, prevent that damage and remove that many +1/+1 counters from it.\n{1}{B}{G}: Polukranos fights another target creature.\nEscape—{4}{B}{G}, Exile six other cards from your graveyard.","keywords":["Escape","Fight"],"identifiers":{"scryfallOracleId":"649e7237-b38b-43e9-83f4-763751fb1bea"}}"###,
    r###"{"name":"Raucous Carnival","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["R","W"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {R} or {W}.","identifiers":{"scryfallOracleId":"24a97436-ba61-4ebc-a560-a6c027ccfdf3"}}"###,
    r###"{"name":"Razortrap Gorge","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","R"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {B} or {R}.","identifiers":{"scryfallOracleId":"8f69bd3a-244e-42d8-bfac-5a426f4b54b4"}}"###,
    r###"{"name":"Rejuvenating Springs","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["G","U"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {G} or {U}.","identifiers":{"scryfallOracleId":"d1620449-930a-4895-a143-fd2a0a3c8b17"}}"###,
    r###"{"name":"Sea of Clouds","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["U","W"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {W} or {U}.","identifiers":{"scryfallOracleId":"672e190d-8ea0-4a2e-b74f-5d35304631e4"}}"###,
    r###"{"name":"Spectator Seating","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["R","W"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {R} or {W}.","identifiers":{"scryfallOracleId":"cf6d10ed-85c3-48f2-8ba0-2960e03b408b"}}"###,
    r###"{"name":"Spire Garden","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["G","R"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {R} or {G}.","identifiers":{"scryfallOracleId":"45fe016e-1a09-410c-bbe3-4663ba06c5b7"}}"###,
    r###"{"name":"Strangled Cemetery","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","G"],"layout":"normal","text":"This land enters tapped unless a player has 13 or less life.\n{T}: Add {B} or {G}.","identifiers":{"scryfallOracleId":"3a5b3405-a1e3-4aad-ab4e-1b8db2d1f3a8"}}"###,
    r###"{"name":"Tatterkite","manaCost":"{3}","manaValue":3.0,"type":"Artifact Creature — Scarecrow","types":["Artifact","Creature"],"subtypes":["Scarecrow"],"supertypes":[],"colors":[],"colorIdentity":[],"power":"2","toughness":"1","layout":"normal","text":"Flying\nThis creature can't have counters put on it.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"9505f124-f152-48ce-b0d3-097abdda8e3e"}}"###,
    r###"{"name":"Thought Sponge","manaCost":"{3}{U}","manaValue":4.0,"type":"Creature — Sponge","types":["Creature"],"subtypes":["Sponge"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"1","layout":"normal","text":"Flash\nThis creature enters with a number of +1/+1 counters on it equal to the greatest number of cards an opponent has drawn this turn.\nWhen this creature dies, draw cards equal to its power.","keywords":["Flash"],"identifiers":{"scryfallOracleId":"256e094a-a9e8-42d9-90e2-7a24ed85e426"}}"###,
    r###"{"name":"Training Center","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["R","U"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {U} or {R}.","identifiers":{"scryfallOracleId":"e3570ac7-c593-40e3-bbd6-ec3da6d8158d"}}"###,
    r###"{"name":"Ugin's Conjurant","manaCost":"{X}","manaValue":0.0,"type":"Creature — Spirit Monk","types":["Creature"],"subtypes":["Spirit","Monk"],"supertypes":[],"colors":[],"colorIdentity":[],"power":"0","toughness":"0","layout":"normal","text":"This creature enters with X +1/+1 counters on it.\nIf damage would be dealt to this creature while it has a +1/+1 counter on it, prevent that damage and remove that many +1/+1 counters from this creature.","identifiers":{"scryfallOracleId":"787055e6-5d65-458e-a56e-66ef0574d2ca"}}"###,
    r###"{"name":"Undergrowth Champion","manaCost":"{1}{G}{G}","manaValue":3.0,"type":"Creature — Elemental","types":["Creature"],"subtypes":["Elemental"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"2","toughness":"2","layout":"normal","text":"If damage would be dealt to this creature while it has a +1/+1 counter on it, prevent that damage and remove a +1/+1 counter from this creature.\nLandfall — Whenever a land you control enters, put a +1/+1 counter on this creature.","keywords":["Landfall"],"identifiers":{"scryfallOracleId":"cb0eb84d-b41b-4147-aa7c-089b7fabd835"}}"###,
    r###"{"name":"Undergrowth Stadium","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","G"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {B} or {G}.","identifiers":{"scryfallOracleId":"7c69f718-acc8-4851-8e5d-0cbaaa86192c"}}"###,
    r###"{"name":"Vault of Champions","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","W"],"layout":"normal","text":"This land enters tapped unless you have two or more opponents.\n{T}: Add {W} or {B}.","identifiers":{"scryfallOracleId":"ebc5ac83-08d4-4d6b-b840-0c4ba71a38ab"}}"###,
    r###"{"name":"Besmirch","manaCost":"{1}{R}{R}","manaValue":3.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"Until end of turn, gain control of target creature and it gains haste. Untap and goad that creature. (Until your next turn, that creature attacks each combat if able and attacks a player other than you if able.)","keywords":["Goad"],"identifiers":{"scryfallOracleId":"fc80380c-7d75-4556-aaf2-bf7b2c757e75"}}"###,
    r###"{"name":"Blufferfish","manaCost":"{4}{U}{U}","manaValue":6.0,"type":"Creature — Fish","types":["Creature"],"subtypes":["Fish"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"3","toughness":"6","layout":"normal","text":"{3}{U}: An opponent tells you a statement about themselves and secretly notes whether it's true or false. You guess which it is. If you guessed correctly, this creature gets +2/+0 and can't be blocked this turn.","identifiers":{"scryfallOracleId":"6ca06048-7c84-49a0-95db-dca4ce4cb95c"}}"###,
    r###"{"name":"Garruk, Curse Breaker","manaCost":"{3}{G}{G}","manaValue":5.0,"type":"Legendary Planeswalker — Garruk","types":["Planeswalker"],"subtypes":["Garruk"],"supertypes":["Legendary"],"colors":["G"],"colorIdentity":["G"],"loyalty":"5","layout":"normal","text":"Whenever a creature you control with power 4 or greater enters, draw a card.\n[+2]: Untap up to two target lands.\n[−3]: Create a 4/4 green Beast creature token with trample.\n[−4]: Until your next turn, whenever one or more creatures attack one of your opponents, those creatures get +2/+2 and gain trample until end of turn.","identifiers":{"scryfallOracleId":"184d4672-05a9-4182-8b3f-3561fb33ed62"}}"###,
    r###"{"name":"Giant Slug","manaCost":"{1}{B}","manaValue":2.0,"type":"Creature — Slug","types":["Creature"],"subtypes":["Slug"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"1","toughness":"1","layout":"normal","text":"{5}: At the beginning of your next upkeep, choose a basic land type. This creature gains landwalk of the chosen type until the end of that turn. (It can't be blocked as long as defending player controls a land of that type.)","identifiers":{"scryfallOracleId":"1ce7f356-1f9b-44dc-9b05-f7b1ecc5d755"}}"###,
    r###"{"name":"Grab the Reins","manaCost":"{3}{R}","manaValue":4.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"Choose one —\n• Until end of turn, you gain control of target creature and it gains haste.\n• Sacrifice a creature. Grab the Reins deals damage equal to that creature's power to any target.\nEntwine {2}{R} (Choose both if you pay the entwine cost.)","keywords":["Entwine"],"identifiers":{"scryfallOracleId":"3ceda405-8175-416a-beda-9a95f5acc60c"}}"###,
    r###"{"name":"Haunted One","manaCost":"{2}{B}","manaValue":3.0,"type":"Legendary Enchantment — Background","types":["Enchantment"],"subtypes":["Background"],"supertypes":["Legendary"],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Commander creatures you own have \"Whenever this creature becomes tapped, it and other creatures you control that share a creature type with it each get +2/+0 and gain undying until end of turn.\" (When a creature with undying dies, if it had no +1/+1 counters on it, return it to the battlefield under its owner's control with a +1/+1 counter on it.)","identifiers":{"scryfallOracleId":"689cd6a8-8be0-49a6-9758-a81cc9f55cc8"}}"###,
    r###"{"name":"Invisible Woman","manaCost":"{2}{W}","manaValue":3.0,"type":"Legendary Creature — Human Hero","types":["Creature"],"subtypes":["Human","Hero"],"supertypes":["Legendary"],"colors":["W"],"colorIdentity":["G","R","U","W"],"power":"3","toughness":"3","layout":"normal","text":"At the beginning of combat on your turn, if you've cast a noncreature spell this turn, create a 0/3 colorless Wall creature token with defender and reach.\nWhenever you attack, you may pay {R}{G}{W}{U}. When you do, target creature gets +1/+0 until end of turn for each creature you control and can't be blocked this turn.","identifiers":{"scryfallOracleId":"085016ee-4429-453f-b7f3-d61065f1214e"}}"###,
    r###"{"name":"Kang Dynasty","manaCost":"{3}{U}","manaValue":4.0,"type":"Enchantment — Saga","types":["Enchantment"],"subtypes":["Saga"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"saga","text":"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II — For each opponent, tap up to one target creature that player controls. Goad those creatures. Until your next turn, whenever any of those creatures deals combat damage to a player, draw a card.\nIII — Target creature you control gets +1/+1 until end of turn for each card in your hand and can't be blocked this turn.","keywords":["Goad"],"identifiers":{"scryfallOracleId":"4e420f43-a4a3-44bb-9a4e-a943f93a3534"}}"###,
    r###"{"name":"Legion Leadership // Legion Stronghold","faceName":"Legion Leadership","side":"a","manaCost":"{1}{R/W}","manaValue":2.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["R","W"],"colorIdentity":["R","W"],"layout":"modal_dfc","text":"Until end of turn, double target creature's power and it gains first strike.","keywords":["Double"],"identifiers":{"scryfallOracleId":"ad225ec2-ff3a-48f6-81a7-dfdd1b75e1f7"}}"###,
    r###"{"name":"Okoye, Mighty and Adored","manaCost":"{2}{G}{W}","manaValue":4.0,"type":"Legendary Creature — Human Warrior Hero","types":["Creature"],"subtypes":["Human","Warrior","Hero"],"supertypes":["Legendary"],"colors":["G","W"],"colorIdentity":["G","W"],"power":"3","toughness":"3","layout":"normal","text":"When Okoye enters, you become the monarch.\nAt the beginning of combat on your turn, put a +1/+1 counter on target creature. Whenever that creature attacks the monarch this turn, it gains double strike and trample until end of turn.","identifiers":{"scryfallOracleId":"8fbfc5b1-2146-4d07-a5fe-5f025da31bd6"}}"###,
    r###"{"name":"Perch Protection","manaCost":"{4}{W}{W}","manaValue":6.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Gift an extra turn (You may promise an opponent a gift as you cast this spell. If you do, they take an extra turn after this one.)\nCreate four 2/2 blue Bird creature tokens with flying. If the gift was promised, all permanents you control phase out, and until your next turn, your life total can't change and you gain protection from everything.\nExile Perch Protection.","keywords":["Gift"],"identifiers":{"scryfallOracleId":"295fe873-ecb5-4d5e-a73c-3af6c478afc5"}}"###,
    r###"{"name":"Rediscover the Way","manaCost":"{U}{R}{W}","manaValue":3.0,"type":"Enchantment — Saga","types":["Enchantment"],"subtypes":["Saga"],"supertypes":[],"colors":["R","U","W"],"colorIdentity":["R","U","W"],"layout":"saga","text":"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II — Look at the top three cards of your library. Put one of them into your hand and the rest on the bottom of your library in any order.\nIII — Whenever you cast a noncreature spell this turn, target creature you control gains double strike until end of turn.","identifiers":{"scryfallOracleId":"680a8e14-053f-4cde-a060-e1b36873cf14"}}"###,
    r###"{"name":"Summon: Brynhildr","manaCost":"{1}{R}","manaValue":2.0,"type":"Enchantment Creature — Saga Knight","types":["Enchantment","Creature"],"subtypes":["Saga","Knight"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"2","toughness":"1","layout":"saga","text":"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Chain — Exile the top card of your library. During any turn you put a lore counter on this Saga, you may play that card.\nII, III — Gestalt Mode — When you next cast a creature spell this turn, it gains haste until end of turn.","identifiers":{"scryfallOracleId":"bab5e9ce-6d55-4d1e-a9ac-c2b954191be9"}}"###,
    r###"{"name":"Teferi's Protection","manaCost":"{2}{W}","manaValue":3.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Until your next turn, your life total can't change and you gain protection from everything. All permanents you control phase out. (While they're phased out, they're treated as though they don't exist. They phase in before you untap during your untap step.)\nExile Teferi's Protection.","identifiers":{"scryfallOracleId":"0d4ecdb1-ec90-497f-a7a4-1c68092b8757"}}"###,
    r###"{"name":"The Last Ronin","manaCost":"{4}{B}{G}","manaValue":6.0,"type":"Enchantment — Saga","types":["Enchantment"],"subtypes":["Saga"],"supertypes":[],"colors":["B","G"],"colorIdentity":["B","G"],"layout":"saga","text":"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Destroy all creatures.\nII — Mill four cards. When you do, return target creature card from your graveyard to your hand.\nIII — Whenever a creature you control attacks alone this turn, put three +1/+1 counters on it. It gains trample, lifelink, and indestructible until end of turn.","keywords":["Mill"],"identifiers":{"scryfallOracleId":"e1865bec-1744-40c2-9031-7d6363be5333"}}"###,
    r###"{"name":"Traitor's Clutch","manaCost":"{4}{B}","manaValue":5.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Target creature gets +1/+0, becomes black, and gains shadow until end of turn. (It can block or be blocked by only creatures with shadow.)\nFlashback {1}{B} (You may cast this card from your graveyard for its flashback cost. Then exile it.)","keywords":["Flashback"],"identifiers":{"scryfallOracleId":"64296644-70f3-4698-923f-0794fe5d0d2b"}}"###,
    r###"{"name":"World War Hulk","manaCost":"{3}{G}{G}","manaValue":5.0,"type":"Enchantment — Saga","types":["Enchantment"],"subtypes":["Saga"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"saga","text":"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — The next red or green creature spell you cast this turn can be cast without paying its mana cost.\nII — Put three +1/+1 counters on target creature you control.\nIII — Choose target creature you control. Until end of turn, double its power and toughness and it gains trample.","keywords":["Double"],"identifiers":{"scryfallOracleId":"8c4da760-8913-46ca-b00c-bba6d877d482"}}"###,
    r###"{"name":"Cycle of Life","manaCost":"{1}{G}{G}","manaValue":3.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"Return this enchantment to its owner's hand: Target creature you cast this turn has base power and toughness 0/1 until your next upkeep. At the beginning of your next upkeep, put a +1/+1 counter on that creature.","identifiers":{"scryfallOracleId":"5941374b-eeb8-4d0e-9367-f7f0f8b79739"}}"###,
    r###"{"name":"Divine Sacrament","manaCost":"{1}{W}{W}","manaValue":3.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"White creatures get +1/+1.\nThreshold — White creatures get an additional +1/+1 as long as there are seven or more cards in your graveyard.","keywords":["Threshold"],"identifiers":{"scryfallOracleId":"92bd7fe2-bb70-40fb-984b-26e461347c56"}}"###,
    r###"{"name":"Study Hall","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"{T}: Add {C}.\n{1}, {T}: Add one mana of any color. When you spend this mana to cast your commander, scry X, where X is the number of times it's been cast from the command zone this game.","keywords":["Scry"],"identifiers":{"scryfallOracleId":"eb735501-19e7-4910-aa6a-6667fff6f4e5"}}"###,
    r###"{"name":"Sap Vitality","manaCost":"{B}{B}","manaValue":2.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Sap Vitality deals 3 damage to target creature or planeswalker. Choose a creature card in your hand. It perpetually gets +3/+0.","identifiers":{"scryfallOracleId":"b4cfcb3e-6f62-424f-b7c5-f9ba456e47c5"}}"###,
    r###"{"name":"Prairie Survivalist","manaCost":"{1}{W}","manaValue":2.0,"type":"Creature — Human Scout","types":["Creature"],"subtypes":["Human","Scout"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"power":"0","toughness":"1","layout":"normal","text":"Flying\nAt the beginning of combat on your turn, target creature you control perpetually gets +1/+0. Then choose a creature card in your hand. It perpetually gets +1/+0.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"e5991685-3bf0-48d4-9f11-8a21f2914c4f"}}"###,
    r###"{"name":"Veteran Charger","manaCost":"{2}{G}","manaValue":3.0,"type":"Creature — Centaur Soldier","types":["Creature"],"subtypes":["Centaur","Soldier"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"2","toughness":"2","layout":"normal","text":"When Veteran Charger enters the battlefield, choose a creature card in your hand. It perpetually gets +2/+2.","identifiers":{"scryfallOracleId":"28a92014-c911-4e64-9072-7f7d9c3d7544"}}"###,
    r###"{"name":"Perennial Gravewarden","manaCost":"{B}{G}","manaValue":2.0,"type":"Creature — Elf Warlock","types":["Creature"],"subtypes":["Elf","Warlock"],"supertypes":[],"colors":["B","G"],"colorIdentity":["B","G"],"power":"1","toughness":"1","layout":"normal","text":"Menace\nWhen this creature enters, it perpetually gets +1/+1.\nAt the beginning of your end step, if a creature entered and a creature died this turn, return this card from your graveyard to the battlefield tapped.","keywords":["Menace"],"identifiers":{"scryfallOracleId":"fc8c4819-5cec-4dab-ae2d-90964ec71ac9"}}"###,
    r###"{"name":"Tomakul Phoenix","manaCost":"{1}{R}{R}","manaValue":3.0,"type":"Creature — Phoenix","types":["Creature"],"subtypes":["Phoenix"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"2","toughness":"2","layout":"normal","text":"Flying, haste\nWhen Tomakul Phoenix dies, it perpetually gets +2/+2.\nAt the beginning of combat on your turn, you may pay {X}{R}, where X is Tomakul Phoenix's power. If you do, return it from your graveyard to the battlefield.","keywords":["Flying","Haste"],"identifiers":{"scryfallOracleId":"ee853994-e3e6-4b9c-84fd-7c5cb9a8db88"}}"###,
    r###"{"name":"Hook Horror","manaCost":"{4}{B}","manaValue":5.0,"type":"Creature — Horror","types":["Creature"],"subtypes":["Horror"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"3","layout":"normal","text":"Molting Exoskeleton — When Hook Horror dies, it perpetually gets -1/-1. Then if that card's toughness is 1 or greater, return it to the battlefield under its owner's control.","keywords":["Molting Exoskeleton"],"identifiers":{"scryfallOracleId":"ac81863b-10fe-44cb-bcd8-d704f89ff836"}}"###,
    r###"{"name":"Alora, Cheerful Scout","manaCost":"{3}{G}{U}","manaValue":5.0,"type":"Legendary Creature — Halfling Rogue Scout","types":["Creature"],"subtypes":["Halfling","Rogue","Scout"],"supertypes":["Legendary"],"colors":["G","U"],"colorIdentity":["G","U"],"power":"4","toughness":"4","layout":"normal","text":"Whenever you attack, up to one target attacking creature can't be blocked this turn. At the beginning of the next end step, return that creature to its owner's hand. If you do, it perpetually gets +1/+1.","identifiers":{"scryfallOracleId":"73a2ed6f-6506-4287-9eb0-62269f5b2166"}}"###,
    r###"{"name":"Angel of Unity","manaCost":"{1}{W}","manaValue":2.0,"type":"Creature — Angel Cleric","types":["Creature"],"subtypes":["Angel","Cleric"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"power":"1","toughness":"3","layout":"normal","text":"Flying, lifelink\nWhenever this creature enters and whenever you cast a party spell, choose a party creature card in your hand. It perpetually gets +1/+1. (A party card or spell is a Cleric, Rogue, Warrior, or Wizard.)","keywords":["Flying","Lifelink"],"identifiers":{"scryfallOracleId":"b7416785-651a-403b-ae64-8bc7251b5d3a"}}"###,
    r###"{"name":"Darigaaz's Whelp","manaCost":"{1}{B}","manaValue":2.0,"type":"Creature — Dragon","types":["Creature"],"subtypes":["Dragon"],"supertypes":[],"colors":["B"],"colorIdentity":["B","R"],"power":"2","toughness":"1","layout":"normal","text":"Kicker {1}{R}\nFlying\nWhenever you draw a Dragon card, it perpetually gets +1/+1.\nWhen Darigaaz's Whelp enters, if it was kicked, seek a Dragon card. Darigaaz's Whelp and that Dragon card each perpetually get +1/+1.","keywords":["Flying","Kicker","Seek"],"identifiers":{"scryfallOracleId":"84aefd30-87fd-4b33-8192-5eb8bc0ada08"}}"###,
    r###"{"name":"Penregon Besieged","manaCost":"{1}{B}","manaValue":2.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"At the beginning of your end step, choose a creature with the least toughness among creatures your opponents control. It perpetually gets -1/-1.\nWhen your opponents control no creatures, sacrifice Penregon Besieged.","identifiers":{"scryfallOracleId":"2c483e01-7e07-46f3-be41-0dd987b76b5b"}}"###,
    r###"{"name":"Scour the Scene","manaCost":"{W}{U}","manaValue":2.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["U","W"],"colorIdentity":["U","W"],"layout":"normal","text":"When Scour the Scene enters, create a 2/2 white and blue Detective creature token.\nWhenever you sacrifice an artifact, choose a creature card in your hand or a creature you control. It perpetually gets +1/+0.","identifiers":{"scryfallOracleId":"6c7d857c-25cc-4970-9cbc-c05f5e40a35a"}}"###,
    r###"{"name":"Skyline Savior","manaCost":"{1}{W}{W}","manaValue":3.0,"type":"Creature — Angel","types":["Creature"],"subtypes":["Angel"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"power":"3","toughness":"4","layout":"normal","text":"Flying\nWhen this creature enters, return a permanent you control to its owner's hand. If it's a non-Angel creature card, it perpetually gets +1/+1, gains flying, and becomes an Angel in addition to its other types.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"7a10f50a-69dc-48a6-96c0-da4172eb8b81"}}"###,
    r###"{"name":"Ghostfire","manaCost":"{2}{R}","manaValue":3.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["R"],"layout":"normal","text":"Ghostfire is colorless.\nGhostfire deals 3 damage to any target.","identifiers":{"scryfallOracleId":"3479a4e6-c67d-4958-8a47-43d3a2f69bb1"}}"###,
    r###"{"name":"The Swamp Kitten","manaCost":"{3}{B}","manaValue":4.0,"type":"Legendary Creature — Cat","types":["Creature"],"subtypes":["Cat"],"supertypes":["Legendary"],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"Land creatures and/or creatures you control with menace get +1/+1.\nAt the beginning of combat on your turn, target Swamp you control becomes a 0/0 black land creature and gains menace. It's still a land. Put a +1/+1 counter on it. (It can't be blocked except by two or more creatures. It's dark and scary.)","identifiers":{"scryfallOracleId":"eba38613-bb48-4d96-b1c8-c87f1a1f9446"}}"###,
    r###"{"name":"Sword of the Meek","manaCost":"{2}","manaValue":2.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"Equipped creature gets +1/+2.\nEquip {2}\nWhenever a 1/1 creature you control enters, you may return this card from your graveyard to the battlefield, then attach it to that creature.","keywords":["Equip"],"identifiers":{"scryfallOracleId":"215c287d-56a5-46da-b49e-8524b6d320a4"}}"###,
    r###"{"name":"Check for Traps","manaCost":"{1}{B}","manaValue":2.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Target opponent reveals their hand. You choose a nonland card from it. Exile that card. If an instant card or a card with flash is exiled this way, they lose 1 life. Otherwise, you lose 1 life.","identifiers":{"scryfallOracleId":"41b2ea88-b3b9-44f0-8886-7b2f37b22ebf"}}"###,
    r###"{"name":"Diplomacy of the Wastes","manaCost":"{2}{B}","manaValue":3.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Target opponent reveals their hand. You choose a nonland card from it. That player discards that card. If you control a Warrior, that player loses 2 life.","identifiers":{"scryfallOracleId":"8b4670b9-6701-4ca6-afd7-256742267e86"}}"###,
    r###"{"name":"Down for Repairs","manaCost":"{2}{B}","manaValue":3.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Target opponent reveals their hand. You choose a nonland card from it. That player discards that card. Destroy up to one target Attraction that player controls. (It's put into their junkyard.)","identifiers":{"scryfallOracleId":"455dc001-3f6d-4d44-afa7-0f83a2520baf"}}"###,
    r###"{"name":"Oildeep Gearhulk","manaCost":"{U}{U}{B}{B}","manaValue":4.0,"type":"Artifact Creature — Construct","types":["Artifact","Creature"],"subtypes":["Construct"],"supertypes":[],"colors":["B","U"],"colorIdentity":["B","U"],"power":"4","toughness":"4","layout":"normal","text":"Lifelink, ward {1}\nWhen this creature enters, look at target player's hand. You may choose a card from it. If you do, that player discards that card, then draws a card.","keywords":["Lifelink","Ward"],"identifiers":{"scryfallOracleId":"aff0740f-51d5-41c7-ac13-97c298cfd743"}}"###,
    r###"{"name":"Concealing Curtains // Revealing Eye","faceName":"Revealing Eye","side":"b","manaValue":1.0,"type":"Creature — Eye Horror","types":["Creature"],"subtypes":["Eye","Horror"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"4","layout":"transform","text":"Menace\nWhen this creature transforms into Revealing Eye, target opponent reveals their hand. You may choose a nonland card from it. If you do, that player discards that card, then draws a card.","keywords":["Defender","Menace","Transform"],"identifiers":{"scryfallOracleId":"a3a56b02-7290-4352-a862-f2de608f2a86"}}"###,
    r###"{"name":"Salt Vampire","manaCost":"{1}{B}","manaValue":2.0,"type":"Creature — Vampire Shapeshifter","types":["Creature"],"subtypes":["Vampire","Shapeshifter"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"Lifelink\nWhen this creature enters, look at target opponent's hand. You may choose a nonland card from it. If you do, that player exiles that card, then draws a card.","keywords":["Lifelink"],"identifiers":{"scryfallOracleId":"31c7453e-33b8-4eb6-abac-28cdb35fb993"}}"###,
    r###"{"name":"Tourach's Canticle","manaCost":"{3}{B}","manaValue":4.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Target opponent reveals their hand. You choose a card from it. That player discards that card, then discards a card at random.","identifiers":{"scryfallOracleId":"738d2be9-c40b-4a9b-95ed-18d659080918"}}"###,
    r###"{"name":"Vendilion Clique","manaCost":"{1}{U}{U}","manaValue":3.0,"type":"Legendary Creature — Faerie Wizard","types":["Creature"],"subtypes":["Faerie","Wizard"],"supertypes":["Legendary"],"colors":["U"],"colorIdentity":["U"],"power":"3","toughness":"1","layout":"normal","text":"Flash\nFlying\nWhen Vendilion Clique enters, look at target player's hand. You may choose a nonland card from it. If you do, that player reveals the chosen card, puts it on the bottom of their library, then draws a card.","keywords":["Flash","Flying"],"identifiers":{"scryfallOracleId":"244d4807-0802-41bc-9460-55ac38a28a72"}}"###,
    r###"{"name":"Crude Abattoir // Unsavory Kitchen","faceName":"Unsavory Kitchen","side":"b","manaCost":"{2}{R}","manaValue":4.0,"type":"Enchantment — Room","types":["Enchantment"],"subtypes":["Room"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"split","text":"Whenever a source you control deals noncombat damage to a creature, choose a creature card in your hand. It perpetually gets +2/+0 and gains haste.","identifiers":{"scryfallOracleId":"52963d93-a90c-41a6-984d-fbc49ebfb20a"}}"###,
    r###"{"name":"Rowan, Scion of War","manaCost":"{1}{B}{R}","manaValue":3.0,"type":"Legendary Creature — Human Wizard","types":["Creature"],"subtypes":["Human","Wizard"],"supertypes":["Legendary"],"colors":["B","R"],"colorIdentity":["B","R"],"power":"4","toughness":"2","layout":"normal","text":"Menace\n{T}: Spells you cast this turn that are black and/or red cost {X} less to cast, where X is the amount of life you lost this turn. Activate only as a sorcery.","keywords":["Menace"],"identifiers":{"scryfallOracleId":"333c15c6-c8fd-4cdd-89d2-06b921edeb8b"}}"###,
    r###"{"name":"Will, Scion of Peace","manaCost":"{1}{W}{U}","manaValue":3.0,"type":"Legendary Creature — Human Wizard","types":["Creature"],"subtypes":["Human","Wizard"],"supertypes":["Legendary"],"colors":["U","W"],"colorIdentity":["U","W"],"power":"2","toughness":"4","layout":"normal","text":"Vigilance\n{T}: Spells you cast this turn that are white and/or blue cost {X} less to cast, where X is the amount of life you gained this turn. Activate only as a sorcery.","keywords":["Vigilance"],"identifiers":{"scryfallOracleId":"4ecb8653-db8c-468a-853d-420ff62a7a8a"}}"###,
    r###"{"name":"Scavenging Ooze","manaCost":"{1}{G}","manaValue":2.0,"type":"Creature — Ooze","types":["Creature"],"subtypes":["Ooze"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"2","toughness":"2","layout":"normal","text":"{G}: Exile target card from a graveyard. If it was a creature card, put a +1/+1 counter on this creature and you gain 1 life.","identifiers":{"scryfallOracleId":"1ff25f67-36a7-4cfa-a2b1-2135b5b6fb67"}}"###,
    r###"{"name":"Mishra's Bauble","manaCost":"{0}","manaValue":0.0,"type":"Artifact","types":["Artifact"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"{T}, Sacrifice this artifact: Look at the top card of target player's library. Draw a card at the beginning of the next turn's upkeep.","identifiers":{"scryfallOracleId":"63afc3d1-7653-476e-838f-fc18d4a62a21"}}"###,
    r###"{"name":"Wrenn and Six","manaCost":"{R}{G}","manaValue":2.0,"type":"Legendary Planeswalker — Wrenn","types":["Planeswalker"],"subtypes":["Wrenn"],"supertypes":["Legendary"],"colors":["G","R"],"colorIdentity":["G","R"],"loyalty":"3","layout":"normal","text":"[+1]: Return up to one target land card from your graveyard to your hand.\n[−1]: Wrenn and Six deals 1 damage to any target.\n[−7]: You get an emblem with \"Instant and sorcery cards in your graveyard have retrace.\" (You may cast instant and sorcery cards from your graveyard by discarding a land card in addition to paying their other costs.)","identifiers":{"scryfallOracleId":"108ae90a-50fa-4cfd-b751-d630e41425fe"}}"###,
];

pub(super) fn export_route_repair_atomic(name: &str) -> engine::database::mtgjson::AtomicCard {
    EXPORT_ROUTE_PRINTED
        .iter()
        .map(|record| {
            serde_json::from_str::<engine::database::mtgjson::AtomicCard>(record).unwrap()
        })
        .find(|card| card.face_name.as_deref().unwrap_or(&card.name) == name)
        .unwrap_or_else(|| panic!("missing pinned route fixture: {name}"))
}

fn export_route_repair_json() -> String {
    use engine::database::synthesis::{build_oracle_face, build_oracle_face_multi};
    let mut export = Map::new();
    for record in EXPORT_ROUTE_PRINTED {
        let card: engine::database::mtgjson::AtomicCard = serde_json::from_str(record).unwrap();
        let face = if card.face_name.is_some() {
            build_oracle_face_multi(&card, card.identifiers.scryfall_oracle_id.clone())
        } else {
            build_oracle_face(&card, card.identifiers.scryfall_oracle_id.clone())
        };
        let mut value = serde_json::to_value(&face).unwrap();
        value["layout"] = serde_json::json!(card.layout);
        value["face_index"] = serde_json::json!(usize::from(card.side.as_deref() == Some("b")));
        export.insert(face.name.to_lowercase(), value);
    }
    serde_json::to_string(&export).unwrap()
}

pub(super) fn export_route_repair_db() -> CardDatabase {
    CardDatabase::from_json_str(&export_route_repair_json()).unwrap()
}

#[test]
fn replacement_priority_preserves_all_39_complete_pinned_faces_through_normal_loaders() {
    let serialized = export_route_repair_json();
    let from_string = CardDatabase::from_json_str(&serialized).unwrap();
    let path = std::env::temp_dir().join(format!(
        "phase-replacement-routes-{}.json",
        std::process::id()
    ));
    std::fs::write(&path, &serialized).unwrap();
    let from_file = CardDatabase::from_export(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    for db in [&from_string, &from_file] {
        for (name, expected_replacements) in [
            ("Abandoned Campground", 1),
            ("Arlinn, Voice of the Pack", 1),
            ("Ashling's Prerogative", 2),
            ("Bleeding Woods", 1),
            ("Bloatfly Swarm", 2),
            ("Blood Scrivener", 1),
            ("Bountiful Promenade", 1),
            ("Buzzing Whack-a-Doodle", 1),
            ("Camato Scout", 1),
            ("Etched Cornfield", 1),
            ("Jace, Wielder of Mysteries", 1),
            ("Laboratory Maniac", 1),
            ("Lakeside Shack", 1),
            ("Living Conundrum", 1),
            ("Luxury Suite", 1),
            ("Melira's Keepers", 1),
            ("Morphic Pool", 1),
            ("Murky Sewer", 1),
            ("Neglected Manor", 1),
            ("Oathsworn Knight", 2),
            ("Ormos, Archive Keeper", 1),
            ("Out of the Tombs", 1),
            ("Peculiar Lighthouse", 1),
            ("Phial of Galadriel", 2),
            ("Polukranos, Unchained", 3),
            ("Raucous Carnival", 1),
            ("Razortrap Gorge", 1),
            ("Rejuvenating Springs", 1),
            ("Sea of Clouds", 1),
            ("Spectator Seating", 1),
            ("Spire Garden", 1),
            ("Strangled Cemetery", 1),
            ("Tatterkite", 1),
            ("Thought Sponge", 1),
            ("Training Center", 1),
            ("Ugin's Conjurant", 2),
            ("Undergrowth Champion", 1),
            ("Undergrowth Stadium", 1),
            ("Vault of Champions", 1),
        ] {
            let atomic = export_route_repair_atomic(name);
            let face = db.get_face_by_name(name).unwrap();
            assert_eq!(
                face.oracle_text.as_deref(),
                atomic.text.as_deref(),
                "{name}"
            );
            assert_eq!(
                face.replacements.len()
                    + face
                        .static_abilities
                        .iter()
                        .filter(|definition| matches!(
                            definition.mode,
                            engine::types::statics::StaticMode::EntersWithAdditionalCounters { .. }
                        ))
                        .count(),
                expected_replacements,
                "complete replacement route: {name}: {face:#?}"
            );
        }
        let ashling = db.get_face_by_name("Ashling's Prerogative").unwrap();
        assert_eq!(ashling.static_abilities.len(), 1);
        assert!(
            ashling.abilities.is_empty(),
            "all enters/parity clauses must have their standing owners"
        );
        let cycle = db.get_face_by_name("Cycle of Life").unwrap();
        assert_eq!(cycle.abilities.len(), 1);
        assert!(matches!(
            cycle.abilities[0].effect.as_ref(),
            Effect::Unimplemented { .. }
        ));
        assert!(
            cycle.abilities[0].sub_ability.is_none(),
            "a delayed rider cannot survive its unrepresented target restriction"
        );
        assert!(cycle.abilities[0]
            .effect
            .unimplemented_description()
            .unwrap()
            .contains("you cast this turn"));
        let divine = db.get_face_by_name("Divine Sacrament").unwrap();
        assert_eq!(
            divine.static_abilities.len(),
            2,
            "both standing bonuses survive"
        );
        assert!(
            divine.abilities.is_empty(),
            "threshold is a live standing predicate"
        );
        assert!(divine
            .static_abilities
            .iter()
            .any(|definition| definition.condition.is_some()));
        let ghostfire = db.get_face_by_name("Ghostfire").unwrap();
        assert!(ghostfire
            .static_abilities
            .iter()
            .any(|definition| definition.characteristic_defining
                && definition.affected == Some(TargetFilter::SelfRef)));
        assert!(matches!(
            ghostfire.abilities[0].effect.as_ref(),
            Effect::DealDamage { .. }
        ));
        for name in ["Rowan, Scion of War", "Will, Scion of Peace"] {
            let face = db.get_face_by_name(name).unwrap();
            assert_eq!(face.abilities.len(), 1);
            assert!(
                matches!(
                    face.abilities[0].effect.as_ref(),
                    Effect::GenericEffect {
                        duration: Some(Duration::UntilEndOfTurn),
                        ..
                    }
                ),
                "the complete temporary reducer must reach its resolving owner: {name}"
            );
        }
        let wrenn = db.get_face_by_name("Wrenn and Six").unwrap();
        assert_eq!(wrenn.abilities.len(), 3);
        assert!(wrenn
            .abilities
            .iter()
            .all(|ability| ability.kind == engine::types::ability::AbilityKind::Activated));
        assert!(
            matches!(
                wrenn.abilities[2].effect.as_ref(),
                Effect::CreateEmblem { .. }
            ),
            "the quoted graveyard keyword belongs to an emblem created by the loyalty ability"
        );
    }
}

#[test]
fn all_17_printed_bounded_grants_keep_their_windows_or_explicit_gaps_in_normal_export() {
    fn inspect(value: &Value, expected: &Value, seen: &mut usize) {
        match value {
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("GenericEffect") {
                    let relevant =
                        object["static_abilities"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|definition| {
                                definition["mode"] == "CantBeBlocked"
                                    || definition["modifications"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .any(|modification| modification["type"] == "AddKeyword")
                            });
                    if relevant {
                        *seen += 1;
                        assert_eq!(
                            &object["duration"], expected,
                            "the grant has its own printed window: {value}"
                        );
                    }
                }
                for child in object.values() {
                    inspect(child, expected, seen);
                }
            }
            Value::Array(children) => {
                for child in children {
                    inspect(child, expected, seen);
                }
            }
            _ => {}
        }
    }
    let db = export_route_repair_db();
    for name in [
        "Besmirch",
        "Blufferfish",
        "Garruk, Curse Breaker",
        "Giant Slug",
        "Grab the Reins",
        "Haunted One",
        "Invisible Woman",
        "Kang Dynasty",
        "Legion Leadership",
        "Okoye, Mighty and Adored",
        "Perch Protection",
        "Rediscover the Way",
        "Summon: Brynhildr",
        "Teferi's Protection",
        "The Last Ronin",
        "Traitor's Clutch",
        "World War Hulk",
    ] {
        let face = db.get_face_by_name(name).unwrap();
        if name == "Giant Slug" {
            let value = serde_json::to_value(face).unwrap();
            let mut nodes = vec![&value];
            let mut gap = false;
            while let Some(node) = nodes.pop() {
                match node {
                    Value::Object(object) => {
                        gap |= object.get("type").and_then(Value::as_str) == Some("Unimplemented")
                            && object.get("name").and_then(Value::as_str)
                                == Some("chosen_landwalk");
                        assert!(
                            !object.contains_key("Landwalk"),
                            "an unbound chosen subtype cannot become a fabricated landwalk keyword"
                        );
                        nodes.extend(object.values());
                    }
                    Value::Array(children) => nodes.extend(children),
                    _ => {}
                }
            }
            assert!(
                gap,
                "the unimplemented chosen-land-type grant must remain an explicit gap"
            );
            continue;
        }
        let mut seen = 0;
        let expected = if matches!(name, "Perch Protection" | "Teferi's Protection") {
            serde_json::json!({"UntilNextTurnOf": {"player": {"type": "Controller"}}})
        } else {
            serde_json::json!("UntilEndOfTurn")
        };
        inspect(&serde_json::to_value(face).unwrap(), &expected, &mut seen);
        assert!(seen > 0, "positive bounded-grant owner reach guard: {name}");
    }
}

#[test]
fn hand_reveal_player_references_keep_their_declared_identity_in_normal_export() {
    fn inspect(
        value: &Value,
        declarations: &mut std::collections::BTreeSet<u64>,
        references: &mut Vec<u64>,
    ) {
        match value {
            Value::Object(object) => {
                if let Some(group) = object.get("declares_chosen_group").and_then(Value::as_u64) {
                    declarations.insert(group);
                }
                if object.get("type").and_then(Value::as_str) == Some("DeclaredPlayer") {
                    references.push(object["group"].as_u64().unwrap());
                }
                if let Some(reference) = object.get("DeclaredPlayer") {
                    references.push(reference["group"].as_u64().unwrap());
                }
                for child in object.values() {
                    inspect(child, declarations, references);
                }
            }
            Value::Array(children) => {
                for child in children {
                    inspect(child, declarations, references);
                }
            }
            _ => {}
        }
    }
    let db = export_route_repair_db();
    for name in [
        "Check for Traps",
        "Diplomacy of the Wastes",
        "Down for Repairs",
        "Oildeep Gearhulk",
        "Revealing Eye",
        "Salt Vampire",
        "Tourach's Canticle",
        "Vendilion Clique",
    ] {
        let face = db.get_face_by_name(name).unwrap();
        let mut declarations = std::collections::BTreeSet::new();
        let mut references = Vec::new();
        inspect(
            &serde_json::to_value(face).unwrap(),
            &mut declarations,
            &mut references,
        );
        assert!(
            !declarations.is_empty() && !references.is_empty(),
            "positive player-binding reach guard: {name}: {declarations:?}/{references:?}"
        );
        assert_eq!(
            declarations.len(),
            1,
            "one printed player target remains one declaration across its hand choice: {name}"
        );
        assert!(
            references.iter().all(|group| declarations.contains(group)),
            "orphan player binding on {name}: {declarations:?}/{references:?}"
        );
    }
}

#[test]
fn complete_selected_and_self_perpetual_subjects_survive_normal_export() {
    fn inspect(value: &Value, edits: &mut usize, choices: &mut usize) {
        match value {
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("ApplyPerpetual") {
                    *edits += 1;
                    assert_ne!(
                        object["target"]["type"], "Any",
                        "a selected edit cannot become a broadcast"
                    );
                }
                if object.get("type").and_then(Value::as_str) == Some("ChooseFromZone") {
                    *choices += 1;
                }
                for child in object.values() {
                    inspect(child, edits, choices);
                }
            }
            Value::Array(children) => {
                for child in children {
                    inspect(child, edits, choices);
                }
            }
            _ => {}
        }
    }
    let db = export_route_repair_db();
    for name in [
        "Sap Vitality",
        "Prairie Survivalist",
        "Veteran Charger",
        "Perennial Gravewarden",
        "Tomakul Phoenix",
        "Hook Horror",
        "Alora, Cheerful Scout",
        "Unsavory Kitchen",
    ] {
        let face = db.get_face_by_name(name).unwrap();
        let mut edits = 0;
        let mut choices = 0;
        inspect(
            &serde_json::to_value(face).unwrap(),
            &mut edits,
            &mut choices,
        );
        assert!(edits > 0, "positive complete edit owner: {name}");
        if matches!(
            name,
            "Sap Vitality" | "Prairie Survivalist" | "Veteran Charger" | "Unsavory Kitchen"
        ) {
            assert!(
                choices > 0,
                "the selected hand authority must survive: {name}"
            );
        }
    }
}

#[test]
fn complete_actual_owner_refusals_and_explicit_operands_survive_both_normal_loaders() {
    let serialized = export_route_repair_json();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &serialized).unwrap();
    for db in [
        CardDatabase::from_json_str(&serialized).unwrap(),
        CardDatabase::from_export(file.path()).unwrap(),
    ] {
        let angelic = db.get_face_by_name("Angelic Renewal").unwrap();
        assert_eq!(
            angelic.triggers.len(),
            1,
            "the complete death trigger is retained"
        );
        let body = angelic.triggers[0].execute.as_deref().unwrap();
        assert!(
            matches!(*body.effect, Effect::Unimplemented { .. }),
            "the complete source-payment/death-card dependency must remain honest: {body:#?}"
        );
        assert!(body.sub_ability.is_none());
        let whelp = db.get_face_by_name("Darigaaz's Whelp").unwrap();
        assert_eq!(
            whelp.triggers.len(),
            2,
            "both printed triggers are retained"
        );
        assert!(
            matches!(
                whelp.triggers[0].mode,
                engine::types::triggers::TriggerMode::Unknown(_)
            ),
            "the drawn-card type predicate has no represented event matcher"
        );
        for trigger in &whelp.triggers {
            let walk = engine::types::ability_visit::visit_ability_def(
                trigger.execute.as_deref().unwrap(),
                &mut |effect| {
                    if matches!(effect, Effect::ApplyPerpetual { .. }) {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                },
            );
            assert!(walk.is_continue(), "an unsupported typed draw/source-and-seek dependency cannot publish a guessed source edit");
        }
        let skyline = db.get_face_by_name("Skyline Savior").unwrap();
        let body = skyline.triggers[0].execute.as_deref().unwrap();
        assert!(
            matches!(*body.effect, Effect::Bounce { .. }),
            "the independent return survives"
        );
        assert!(
            matches!(
                *body.sub_ability.as_deref().unwrap().effect,
                Effect::Unimplemented { .. }
            ),
            "all dependent perpetual/type conjuncts stay refused"
        );
        assert!(body.sub_ability.as_deref().unwrap().sub_ability.is_none());
        let unity = db.get_face_by_name("Angel of Unity").unwrap();
        assert!(!unity.triggers.is_empty());
        for trigger in &unity.triggers {
            assert!(
                engine::types::ability_visit::visit_ability_def(
                    trigger.execute.as_deref().unwrap(),
                    &mut |effect| if matches!(effect, Effect::ApplyPerpetual { .. }) {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                )
                .is_continue(),
                "an unsupported party hand choice publishes no event/source edit"
            );
        }
        for name in ["Armored Skyhunter", "Vault 101: Birthday Party"] {
            let face = db.get_face_by_name(name).unwrap();
            let has_explicit_attachment = face.triggers.iter().any(|trigger| {
                trigger.execute.as_deref().is_some_and(|body| {
                    engine::types::ability_visit::visit_ability_def(body, &mut |effect| {
                        if matches!(
                            effect,
                            Effect::Attach {
                                attachment: TargetFilter::ParentTarget,
                                target: TargetFilter::Typed(_),
                                ..
                            }
                        ) {
                            std::ops::ControlFlow::Break(())
                        } else {
                            std::ops::ControlFlow::Continue(())
                        }
                    })
                    .is_break()
                })
            });
            assert!(has_explicit_attachment, "an explicit selected Equipment and creature host remain represented: {name}: {face:#?}");
        }
        let dragon = db.get_face_by_name("Dragonstorm Globe").unwrap();
        let definition = dragon
            .static_abilities
            .iter()
            .find(|definition| {
                matches!(
                    definition.mode,
                    engine::types::statics::StaticMode::EntersWithAdditionalCounters { .. }
                )
            })
            .unwrap();
        let Some(TargetFilter::Typed(filter)) = definition.affected.as_ref() else {
            panic!("controlled Dragon population must be typed")
        };
        assert!(filter
            .type_filters
            .contains(&TypeFilter::Subtype("Dragon".into())));
        assert!(
            !filter.type_filters.contains(&TypeFilter::Creature),
            "a bare subtype includes noncreature Kindreds"
        );
        for name in ["Breach the Multiverse", "Ghouls' Night Out"] {
            let face = db.get_face_by_name(name).unwrap();
            assert!(
                engine::types::ability_visit::visit_ability_def(
                    &face.abilities[0],
                    &mut |effect| if matches!(
                        effect,
                        Effect::ChooseFromZone {
                            zone_owner: engine::types::ability::ZoneOwner::Each(_),
                            ..
                        }
                    ) {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                )
                .is_break(),
                "per-player choice reach guard: {name}"
            );
            assert!(
                engine::types::ability_visit::visit_ability_def(
                    &face.abilities[0],
                    &mut |effect| if matches!(
                        effect,
                        Effect::ChangeZoneAll {
                            target: TargetFilter::TrackedSet { .. },
                            ..
                        }
                    ) {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                )
                .is_break(),
                "the accumulated selected set remains the mass move authority: {name}"
            );
        }
    }
}
