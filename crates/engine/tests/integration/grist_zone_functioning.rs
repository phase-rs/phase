//! Grist, the Hunger Tide: a zone-declared self static functions in the zones it
//! declares.
//!
//! Oracle: "As long as Grist isn't on the battlefield, it's a 1/1 Insect creature
//! in addition to its other types." CR 113.6c: an ability that states which zones
//! it doesn't function in functions everywhere except for the specified zones.
//! Rulings: anywhere but on the battlefield, Grist is a Legendary Planeswalker
//! Creature; opponents can't make you discard her with Duress; a Grist in your
//! graveyard is a creature card and counts for her own -5.
//!
//! Every row drives a production entry (`GameRunner::cast`, `activate`, `act`)
//! with the verbatim Oracle text. A milled Grist is checked as a card that has just
//! reached the graveyard (row I9), not through the +1.

use crate::support::shared_card_db;
use engine::game::layers::flush_layers;
use engine::game::printed_cards::{
    rehydrate_game_from_card_db_with_finalization, CardDbRehydrationFinalization,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::stickers::{
    apply_selected_sticker, available_sticker_candidates, set_player_sticker_sheets,
};
use engine::game::zones::{apply_resolved_zone_change, move_to_zone};
use engine::types::ability::{
    ChosenAttribute, Effect, ManaSpendRestriction, QuantityExpr, ResolvedAbility, StaticCondition,
    StaticDefinition, StickerTicketCostPayment, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, GameState, LayersDirty, ManaChoice, ManaChoicePrompt,
    PersistedGameState, PersistedRestoreFinalization, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::layers::Layer;
use engine::types::mana::{
    ManaColor, ManaCost, ManaCostShard, ManaRestriction, ManaSourceOutput, ManaSpellGrant,
    ManaType, ManaUnit, SpellMeta,
};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::resolved_commands::{ResolvedRulesCommand, ResolvedZoneChangeCommand};
use engine::types::statics::StaticMode;
use engine::types::stickers::{AppliedSticker, StickerKind};
use engine::types::zones::Zone;
use std::sync::Arc;

const GRIST_ORACLE: &str = "As long as Grist isn't on the battlefield, it's a 1/1 Insect creature in addition to its other types.\n+1: Create a 1/1 black and green Insect creature token, then mill a card. If an Insect card was milled this way, put a loyalty counter on Grist and repeat this process.\n\u{2212}2: You may sacrifice a creature. When you do, destroy target creature or planeswalker.\n\u{2212}5: Each opponent loses life equal to the number of creature cards in your graveyard.";
const DURESS: &str = "Target opponent reveals their hand. You choose a noncreature, nonland card from it. That player discards that card.";
const GREEN_SUNS_ZENITH: &str = "Search your library for a green creature card with mana value X or less, put it onto the battlefield, then shuffle. Shuffle Green Sun's Zenith into its owner's library.";
const GLIMPSE_OF_NATURE: &str = "Whenever you cast a creature spell this turn, draw a card.";
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
const ZOMBIFY: &str = "Return target creature card from your graveyard to the battlefield.";
const HEROS_DOWNFALL: &str = "Destroy target creature or planeswalker.";
const MIND_SCULPT: &str = "Target opponent mills seven cards.";
const PAINTER_ORACLE: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";
const ARCANE_PROXY: &str = "Prototype {1}{U}{U} — 2/1 (You may cast this spell with different mana cost, color, and size. It keeps its abilities and types.)\nWhen this creature enters, if you cast it, exile target instant or sorcery card with mana value less than or equal to this creature's power from your graveyard. Copy that card. You may cast the copy without paying its mana cost.";
const TESTCARD_ORACLE: &str = "As long as Testcard is in your graveyard and you control a Forest, it's a 1/1 Insect creature in addition to its other types.";

fn grist_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Black, ManaCostShard::Green],
    }
}

fn cast_prohibition_count(state: &GameState, id: ObjectId) -> usize {
    let canonical = StaticDefinition::new(StaticMode::CantBeCountered);
    state.objects[&id]
        .static_definitions
        .iter_unchecked()
        .filter(|definition| **definition == canonical)
        .count()
}

fn cast_prohibition_assert_proxy(state: &GameState, id: ObjectId, prototype: bool) {
    let object = &state.objects[&id];
    // CR 718.3b + CR 718.4: the chosen form changes live characteristics,
    // while the printed baseline remains the normal form.
    assert_eq!(object.prototype_form.is_some(), prototype);
    assert_eq!(
        (object.power, object.toughness),
        if prototype {
            (Some(2), Some(1))
        } else {
            (Some(4), Some(3))
        }
    );
    assert_eq!(
        object.mana_cost,
        if prototype {
            ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            }
        } else {
            ManaCost::Cost {
                generic: 7,
                shards: vec![],
            }
        }
    );
    assert_eq!(
        object.color,
        if prototype {
            vec![ManaColor::Blue]
        } else {
            vec![]
        }
    );
    assert_eq!(
        (object.base_power, object.base_toughness),
        (Some(4), Some(3))
    );
    assert_eq!(
        object.base_mana_cost,
        ManaCost::Cost {
            generic: 7,
            shards: vec![]
        }
    );
    assert!(object.base_color.is_empty());
    assert!(object.card_types.core_types.contains(&CoreType::Artifact));
    assert!(object.card_types.core_types.contains(&CoreType::Creature));
    assert!(object
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Wizard"));
    assert!(object.stickers.is_empty());
}

fn cast_prohibition_assert_spell(
    state: &GameState,
    id: ObjectId,
    occurrence: ObjectIncarnationRef,
) {
    let object = &state.objects[&id];
    assert_eq!(object.zone, Zone::Stack);
    assert_eq!(ObjectIncarnationRef::from_object(object), occurrence);
    assert!(object.stickers.is_empty());
    assert!(state.stack.iter().chain(state.resolving_stack_entry.iter()).any(|entry| {
        entry.id == id && entry.source_id == id
            && matches!(entry.kind, StackEntryKind::Spell { card_id, .. } if card_id == object.card_id)
    }), "current object must have an actual matching Spell entry");
}

fn cast_prohibition_restore(state: &GameState, trusted: bool) -> GameState {
    let db = shared_card_db().expect("restoration requires the integration CardDB");
    let persisted = if trusted {
        PersistedGameState::capture(state.clone())
    } else {
        PersistedGameState::Raw(Box::new(state.clone()))
    };
    let encoded = serde_json::to_string(&persisted).expect("serialize actual persisted payload");
    let decoded: PersistedGameState =
        serde_json::from_str(&encoded).expect("decode persisted payload");
    decoded
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("prepare actual persisted restore")
        .finalize_after_rehydration(|restored| {
            rehydrate_game_from_card_db_with_finalization(
                restored,
                db,
                CardDbRehydrationFinalization::Defer,
            );
            Ok(())
        })
        .expect("rehydrate and finalize actual persisted restore")
}

fn cast_prohibition_paid_proxy(prototype: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let (runner, cavern, proxy, counterspell, _) = cast_prohibition_paid_proxy_with(prototype, &[]);
    (runner, cavern, proxy, counterspell)
}

type PaidProxy = (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>);

/// The accepted paid Cavern/Proxy route, with `extra` real cards added to the
/// scenario before it is built (returned in order).
fn cast_prohibition_paid_proxy_with(
    prototype: bool,
    extra: &[(PlayerId, &str, Zone)],
) -> PaidProxy {
    let db = shared_card_db().expect("paid Cavern test requires the integration CardDB");
    let mut scenario = main_phase();
    let cavern = scenario.add_real_card(P0, "Cavern of Souls", Zone::Hand, db);
    let proxy = scenario.add_real_card(P0, "Arcane Proxy", Zone::Hand, db);
    let counterspell = scenario.add_real_card(P1, "Counterspell", Zone::Hand, db);
    scenario.add_real_card(P1, "Unsummon", Zone::Hand, db);
    scenario.add_real_card(P1, "Unsubstantiate", Zone::Hand, db);
    let extra: Vec<ObjectId> = extra
        .iter()
        .map(|&(player, name, zone)| scenario.add_real_card(player, name, zone, db))
        .collect();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&cavern].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: cavern,
            card_id,
        })
        .expect("play actual Cavern");
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Cavern must require its actual creature-type choice: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(options.iter().any(|option| option == "Wizard"));
    runner
        .act(GameAction::ChooseOption {
            choice: "Wizard".to_string(),
        })
        .expect("choose Wizard");
    assert!(runner.state().objects[&cavern]
        .chosen_attributes
        .contains(&ChosenAttribute::CreatureType("Wizard".to_string())));
    let object = &runner.state().objects[&proxy];
    assert!(object.card_types.core_types.contains(&CoreType::Creature));
    assert!(object
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Wizard"));
    assert!(object
        .printed_ref
        .as_ref()
        .is_some_and(|reference| db.get_face_by_printed_ref(reference).is_some()));
    let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
    let action = grouped.get(&cavern).into_iter().flatten().find(|action| matches!(action,
        GameAction::TapLandForMana { selection }
            if selection.restrictions == vec![ManaRestriction::OnlyForCreatureType("Wizard".to_string())]
                && selection.mana_type == ManaType::Blue
                && selection.output == ManaSourceOutput::Concrete(ManaType::Blue)
    )).expect("engine must issue Cavern's colored, chosen-type-restricted operation").clone();
    let GameAction::TapLandForMana { selection } = &action else {
        unreachable!()
    };
    let source = &runner.state().objects[&cavern];
    assert_eq!(selection.source, ObjectIncarnationRef::from_object(source));
    assert!(!source.tapped);
    let ability = &source.abilities[selection
        .ability_index
        .expect("actual colored ability index")];
    let Effect::Mana {
        restrictions,
        grants,
        ..
    } = &*ability.effect
    else {
        panic!("selected live colored ability must produce mana");
    };
    assert_eq!(
        restrictions,
        &vec![ManaSpendRestriction::ChosenCreatureType]
    );
    assert!(matches!(
        grants.as_slice(),
        [ManaSpellGrant::CantBeCountered { .. }]
    ));
    let meta = SpellMeta {
        types: object
            .card_types
            .core_types
            .iter()
            .map(ToString::to_string)
            .collect(),
        subtypes: object.card_types.subtypes.clone(),
        ..SpellMeta::default()
    };
    assert!(selection
        .restrictions
        .iter()
        .all(|restriction| restriction.allows_spell(&meta)));
    assert_eq!(runner.state().players[0].mana_pool.units().count(), 0);
    runner
        .act(action)
        .expect("activate engine-issued colored Cavern operation");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "Concrete Blue returns directly to source-controller Priority: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().objects[&cavern].tapped);
    let units: Vec<_> = runner.state().players[0].mana_pool.units().collect();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].source_id, cavern);
    assert_eq!(units[0].color, ManaType::Blue);
    assert_eq!(
        units[0].restrictions,
        vec![ManaRestriction::OnlyForCreatureType("Wizard".to_string())]
    );
    assert!(matches!(
        units[0].grants.as_slice(),
        [ManaSpellGrant::CantBeCountered { .. }]
    ));
    // CR 106.6 + CR 601.2h: exactly two ordinary units force the eligible
    // Cavern unit to be spent on the real prototype cost {1}{U}{U}.
    if prototype {
        add_mana(&mut runner, 0, &[ManaType::Blue, ManaType::Colorless]);
    } else {
        add_mana(&mut runner, 0, &[ManaType::Colorless; 6]);
    }
    assert!(runner.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.source_id != cavern)
        .all(|unit| unit.grants.is_empty() && unit.restrictions.is_empty()));
    if prototype {
        runner
            .cast(proxy)
            .casting_variant(CastingVariant::Prototype)
            .commit();
    } else {
        runner.cast(proxy).commit();
    }
    assert_eq!(
        runner.state().players[0].mana_pool.units().count(),
        0,
        "the granting unit really was spent"
    );
    let object = &runner.state().objects[&proxy];
    cast_prohibition_assert_spell(
        runner.state(),
        proxy,
        ObjectIncarnationRef::from_object(object),
    );
    assert_eq!(object.cast_from_zone, Some(Zone::Hand));
    assert!(
        object.cast_occurrence.is_some(),
        "actual finalized cast must carry its occurrence"
    );
    assert_eq!(object.prototype_form.is_some(), prototype);
    assert_eq!(
        (object.power, object.toughness),
        if prototype {
            (Some(2), Some(1))
        } else {
            (Some(4), Some(3))
        }
    );
    assert_eq!(
        object.color,
        if prototype {
            vec![ManaColor::Blue]
        } else {
            vec![]
        }
    );
    assert_eq!(
        cast_prohibition_count(runner.state(), proxy),
        1,
        "actual payment writer must stamp the live spell"
    );
    assert!(!object
        .base_static_definitions
        .iter()
        .any(|definition| *definition == StaticDefinition::new(StaticMode::CantBeCountered)));
    assert!(
        runner.state().players[0].graveyard.is_empty(),
        "Proxy ETB has no eligible own graveyard card"
    );
    (runner, cavern, proxy, counterspell, extra)
}

fn cast_prohibition_counter(
    runner: &mut GameRunner,
    spell: ObjectId,
    counterspell: ObjectId,
) -> Vec<GameEvent> {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("yield response priority");
    }
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    add_mana(runner, 1, &[ManaType::Blue, ManaType::Blue]);
    runner.cast(counterspell).target_object(spell).commit();
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == spell
            && entry.source_id == spell
            && matches!(entry.kind, StackEntryKind::Spell { .. })),
        "the lower target remains an actual Spell at the counter observation boundary"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == counterspell
                && matches!(&entry.kind, StackEntryKind::Spell { ability, .. }
            if ability.as_ref().is_some_and(|ability| ability.targets == vec![TargetRef::Object(spell)]))),
        "actual Counterspell must target the intended current Spell"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == counterspell
                && matches!(entry.kind, StackEntryKind::Spell { .. })),
        "actual Counterspell committed with legal target"
    );
    let mut events = Vec::new();
    for _ in 0..8 {
        if runner.state().objects[&counterspell].zone != Zone::Stack {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve only Counterspell")
                .events,
        );
    }
    assert_eq!(
        runner.state().objects[&counterspell].zone,
        Zone::Graveyard,
        "Counterspell actually resolved"
    );
    events
}

fn cast_prohibition_priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: current } if current == player)
        {
            return;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("yield response priority");
    }
    panic!("required player did not receive priority");
}

fn cast_prohibition_response(state: &GameState, name: &str) -> ObjectId {
    state
        .objects
        .values()
        .find(|object| object.owner == P1 && object.zone == Zone::Hand && object.name == name)
        .expect("actual response card must be in P1 hand")
        .id
}

#[test]
fn cast_prohibition_restore_cavern_wizard_raw() {
    cast_prohibition_restore_cavern_wizard(false);
}

#[test]
fn cast_prohibition_restore_cavern_wizard_trusted() {
    cast_prohibition_restore_cavern_wizard(true);
}

fn cast_prohibition_restore_cavern_wizard(trusted: bool) {
    let (runner, _, proxy, counterspell) = cast_prohibition_paid_proxy(true);
    let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
    let restored = cast_prohibition_restore(runner.state(), trusted);
    assert_eq!(
        restored.objects[&proxy].cast_occurrence,
        runner.state().objects[&proxy].cast_occurrence
    );
    cast_prohibition_assert_spell(&restored, proxy, occurrence);
    cast_prohibition_assert_proxy(&restored, proxy, true);
    assert_eq!(
        restored.objects[&proxy].prototype_form,
        runner.state().objects[&proxy].prototype_form
    );
    assert_eq!(
        (
            restored.objects[&proxy].power,
            restored.objects[&proxy].toughness
        ),
        (Some(2), Some(1))
    );
    let mut restored = GameRunner::from_state(restored);
    let events = cast_prohibition_counter(&mut restored, proxy, counterspell);
    // CR 112.1 + CR 701.6a: Counterspell resolves, but the paid prohibition
    // preserves this same spell; no pass is made that could resolve Proxy.
    cast_prohibition_assert_spell(restored.state(), proxy, occurrence);
    assert_eq!(cast_prohibition_count(restored.state(), proxy), 1);
    assert!(!events.iter().any(
        |event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)
    ));
}

#[test]
fn cast_prohibition_restore_prototype_characteristics_raw_trusted_immediate_defer() {
    let db = shared_card_db().expect("prototype restore requires CardDB");
    for trusted in [false, true] {
        for immediate in [false, true] {
            // Plain payment isolates form restoration from the prohibition fix.
            let mut scenario = main_phase();
            let first = scenario.add_real_card(P0, "Arcane Proxy", Zone::Hand, db);
            let second = scenario.add_real_card(P0, "Arcane Proxy", Zone::Hand, db);
            let normal = scenario.add_real_card(P0, "Arcane Proxy", Zone::Hand, db);
            scenario.with_mana_pool(
                P0,
                mana(&[ManaType::Blue, ManaType::Blue, ManaType::Colorless]),
            );
            let mut runner = scenario.build();
            runner
                .cast(first)
                .casting_variant(CastingVariant::Prototype)
                .resolve()
                .assert_zone(&[first], Zone::Battlefield);
            add_mana(
                &mut runner,
                0,
                &[ManaType::Blue, ManaType::Blue, ManaType::Colorless],
            );
            runner
                .cast(second)
                .casting_variant(CastingVariant::Prototype)
                .commit();
            let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&second]);
            let permanent_occurrence =
                ObjectIncarnationRef::from_object(&runner.state().objects[&first]);
            let marker = runner.state().objects[&first].prototype_form.clone();
            // Two actual prototype casts have independent current occurrences;
            // the normal sibling has no marker and still receives refresh.
            for id in [first, second, normal] {
                let object = runner.state_mut().objects.get_mut(&id).unwrap();
                assert!(object
                    .printed_ref
                    .as_ref()
                    .is_some_and(|reference| db.get_face_by_printed_ref(reference).is_some()));
                object.name = "stale display".to_string();
                object.base_power = Some(99);
            }
            let mut restored = if immediate {
                let persisted = if trusted {
                    PersistedGameState::capture(runner.state().clone())
                } else {
                    PersistedGameState::Raw(Box::new(runner.state().clone()))
                };
                let decoded: PersistedGameState =
                    serde_json::from_str(&serde_json::to_string(&persisted).unwrap()).unwrap();
                // This is an ordinary Immediate API call on a checked decoded
                // state, not Immediate publication inside the prepared closure.
                let mut state = decoded.into_game_state().expect("checked immediate decode");
                rehydrate_game_from_card_db_with_finalization(
                    &mut state,
                    db,
                    CardDbRehydrationFinalization::Immediate,
                );
                state
            } else {
                cast_prohibition_restore(runner.state(), trusted)
            };
            for _ in 0..2 {
                cast_prohibition_assert_spell(&restored, second, occurrence);
                assert_eq!(restored.objects[&first].zone, Zone::Battlefield);
                assert!(restored.battlefield.contains(&first));
                assert_eq!(
                    ObjectIncarnationRef::from_object(&restored.objects[&first]),
                    permanent_occurrence
                );
                cast_prohibition_assert_proxy(&restored, first, true);
                assert_eq!(restored.objects[&first].prototype_form, marker);
                assert_eq!(restored.objects[&first].name, "Arcane Proxy");
                cast_prohibition_assert_proxy(&restored, second, true);
                cast_prohibition_assert_proxy(&restored, normal, false);
                assert_eq!(restored.objects[&normal].name, "Arcane Proxy");
                rehydrate_game_from_card_db_with_finalization(
                    &mut restored,
                    db,
                    CardDbRehydrationFinalization::Immediate,
                );
                restored.layers_dirty = LayersDirty::Full;
                flush_layers(&mut restored);
            }
        }
    }
}

fn cast_prohibition_recorded_departure(state: &GameState, to: Zone) -> ResolvedZoneChangeCommand {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .rev()
        .filter_map(|entry| entry.command.as_ref())
        .find_map(|command| match command {
            ResolvedRulesCommand::ZoneChange(command)
                if command.from == Zone::Battlefield && command.to == to =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .expect("ordinary Battlefield departure must journal its resolved command")
}

#[test]
fn cast_prohibition_restore_replayed_prototype_departure_has_printed_characteristics() {
    let db = shared_card_db().expect("prototype departure replay requires CardDB");
    for to in [Zone::Hand, Zone::Graveyard] {
        let mut scenario = main_phase();
        let proxy = scenario.add_real_card(P0, "Arcane Proxy", Zone::Hand, db);
        scenario.with_mana_pool(
            P0,
            mana(&[ManaType::Blue, ManaType::Blue, ManaType::Colorless]),
        );
        let mut runner = scenario.build();
        runner
            .cast(proxy)
            .casting_variant(CastingVariant::Prototype)
            .resolve()
            .assert_zone(&[proxy], Zone::Battlefield);
        // CR 718.3b reach guard: the resolved permanent really carries the form.
        cast_prohibition_assert_proxy(runner.state(), proxy, true);
        let permanent = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
        let pre_state = runner.state().clone();

        let mut live = pre_state.clone();
        move_to_zone(&mut live, proxy, to, &mut Vec::new());
        assert_eq!(live.objects[&proxy].zone, to);
        cast_prohibition_assert_proxy(&live, proxy, false);
        let command = cast_prohibition_recorded_departure(&live, to);
        assert_eq!(command.object, permanent);

        // The supported prestate/command/replay contract: apply the exact
        // recorded departure to the state it was recorded from.
        let mut replay = pre_state.clone();
        apply_resolved_zone_change(&mut replay, &command)
            .expect("recorded departure replays against its prestate");
        let object = &replay.objects[&proxy];
        assert_eq!(object.zone, to, "reach guard: replay performed the move");
        assert_eq!(object.incarnation, command.resulting_incarnation);
        assert!(!replay.battlefield.contains(&proxy));
        if to == Zone::Hand {
            assert!(replay.players[0].hand.contains(&proxy));
        } else {
            assert!(replay.players[0].graveyard.contains(&proxy));
        }
        // CR 718.4 + CR 400.7: the replayed departure ends the form exactly
        // as the live one did; the new object has only printed values.
        cast_prohibition_assert_proxy(&replay, proxy, false);
        assert_eq!(object.prototype_form, live.objects[&proxy].prototype_form);

        for trusted in [false, true] {
            let mut restored = cast_prohibition_restore(&replay, trusted);
            for mode in [
                CardDbRehydrationFinalization::Immediate,
                CardDbRehydrationFinalization::Defer,
            ] {
                assert_eq!(restored.objects[&proxy].zone, to);
                assert_eq!(
                    restored.objects[&proxy].incarnation,
                    command.resulting_incarnation
                );
                cast_prohibition_assert_proxy(&restored, proxy, false);
                rehydrate_game_from_card_db_with_finalization(&mut restored, db, mode);
                restored.layers_dirty = LayersDirty::Full;
                flush_layers(&mut restored);
                cast_prohibition_assert_proxy(&restored, proxy, false);
            }

            // Bounded legacy-state supplement: a state saved after the
            // predecessor's replay applier still carries the expired form and
            // its live values on the departed card. Restoring it must yield
            // the card's normal characteristics (CR 718.4).
            let mut legacy = replay.clone();
            {
                let departed = legacy.objects.get_mut(&proxy).unwrap();
                let prestate = &pre_state.objects[&proxy];
                departed.prototype_form = prestate.prototype_form.clone();
                departed.mana_cost = prestate.mana_cost.clone();
                departed.power = prestate.power;
                departed.toughness = prestate.toughness;
                departed.color = prestate.color.clone();
            }
            assert!(
                legacy.objects[&proxy].prototype_form.is_some(),
                "reach guard: the legacy state carries the expired form"
            );
            let restored_legacy = cast_prohibition_restore(&legacy, trusted);
            assert_eq!(restored_legacy.objects[&proxy].zone, to);
            cast_prohibition_assert_proxy(&restored_legacy, proxy, false);

            if to == Zone::Hand {
                // CR 718.4: a later normal cast of that card is a normal
                // spell and permanent; the expired form cannot resurface.
                let mut later = GameRunner::from_state(restored_legacy);
                cast_prohibition_priority(&mut later, P0);
                add_mana(&mut later, 0, &[ManaType::Colorless; 7]);
                later
                    .cast(proxy)
                    .resolve()
                    .assert_zone(&[proxy], Zone::Battlefield);
                cast_prohibition_assert_proxy(later.state(), proxy, false);
            }
        }
    }
}

#[test]
fn cast_prohibition_restore_paused_prototype_cast_keeps_its_form() {
    let db = shared_card_db().expect("paused prototype cast requires CardDB");
    for trusted in [false, true] {
        let mut scenario = main_phase();
        let proxy = scenario.add_real_card(P0, "Arcane Proxy", Zone::Hand, db);
        scenario.with_mana_pool(
            P0,
            mana(&[ManaType::Blue, ManaType::Blue, ManaType::Colorless]),
        );
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&proxy].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: proxy,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Manual,
            })
            .expect("announce actual Proxy cast");
        for _ in 0..4 {
            let index = match &runner.state().waiting_for {
                WaitingFor::ManaPayment { .. } => break,
                WaitingFor::CastingVariantChoice { options, .. } => options
                    .iter()
                    .position(|option| option.variant == CastingVariant::Prototype)
                    .expect("engine offers the prototyped cast"),
                other => panic!("unexpected Proxy cast prompt {other:?}"),
            };
            runner
                .act(GameAction::ChooseCastingVariant { index })
                .expect("choose the prototyped cast");
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        // CR 601.2a reach guard: the announced spell is still being cast; the
        // card has not yet left its origin zone and carries its chosen form.
        let object = &runner.state().objects[&proxy];
        assert_eq!(object.zone, Zone::Hand);
        assert!(object.spell_announcement.is_some());
        assert!(runner.state().stack.iter().any(|entry| entry.id == proxy
            && matches!(
                entry.kind,
                StackEntryKind::Spell {
                    casting_variant: CastingVariant::Prototype,
                    ..
                }
            )));
        cast_prohibition_assert_proxy(runner.state(), proxy, true);

        // CR 718.3a + CR 718.3b: the restored paused cast keeps the form.
        let restored = cast_prohibition_restore(runner.state(), trusted);
        assert_eq!(restored.objects[&proxy].zone, Zone::Hand);
        cast_prohibition_assert_proxy(&restored, proxy, true);
        let mut restored = GameRunner::from_state(restored);
        restored
            .act(GameAction::PassPriority)
            .expect("finish the paused prototype payment");
        assert!(matches!(
            restored.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(restored.state().players[0].mana_pool.units().count(), 0);
        let spell = ObjectIncarnationRef::from_object(&restored.state().objects[&proxy]);
        cast_prohibition_assert_spell(restored.state(), proxy, spell);
        cast_prohibition_assert_proxy(restored.state(), proxy, true);
    }
}

/// Whether `branch` is, or chooses among, an `ApplySticker` whose sticker `pick` accepts.
fn branch_applies_sticker(
    branch: &engine::types::ability::AbilityDefinition,
    pick: &dyn Fn(&AppliedSticker) -> bool,
) -> bool {
    match branch.effect.as_ref() {
        Effect::ApplySticker { sticker, .. } => pick(sticker),
        Effect::ChooseOneOf { branches, .. } => branches
            .iter()
            .any(|inner| branch_applies_sticker(inner, pick)),
        _ => false,
    }
}

/// Resolves a one-sticker `PutSticker` instruction on `recipient` through the
/// sticker resolver and answers its public placement prompts with `pick`.
fn put_sticker_through_resolver(
    runner: &mut GameRunner,
    source: ObjectId,
    recipient: ObjectId,
    kind: StickerKind,
    pick: &dyn Fn(&AppliedSticker) -> bool,
) {
    let instruction = ResolvedAbility::new(
        Effect::PutSticker {
            target: TargetFilter::SpecificObject { id: recipient },
            kind: Some(kind),
            count: QuantityExpr::Fixed { value: 1 },
            max_ticket_cost: None,
            ticket_cost_payment: StickerTicketCostPayment::PayNormally,
        },
        vec![],
        source,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &instruction, &mut events, 0)
        .expect("the sticker instruction resolves");
    let mut placed = events
        .iter()
        .filter(
            |e| matches!(e, GameEvent::StickerPlaced { object_id, .. } if *object_id == recipient),
        )
        .count();
    for _ in 0..4 {
        let WaitingFor::ChooseOneOfBranch {
            player, branches, ..
        } = runner.state().waiting_for.clone()
        else {
            break;
        };
        assert_eq!(player, P0);
        let index = branches
            .iter()
            .position(|branch| branch_applies_sticker(branch, pick))
            .expect("the wanted sticker is offered");
        let outcome = runner
            .act(GameAction::ChooseBranch { index })
            .expect("placement answer is legal");
        placed += outcome
            .events
            .iter()
            .filter(|e| matches!(e, GameEvent::StickerPlaced { object_id, .. } if *object_id == recipient))
            .count();
    }
    assert_eq!(placed, 1, "exactly one sticker reaches the spell");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        runner.state().waiting_for
    );
}

/// CR 702.33h + CR 123.6b + CR 123.7 + CR 718.3b: a sticker put on a spell
/// (sticker kicker's "put a sticker on this spell") modifies the spell as it
/// is. Arcane Proxy, cast prototyped with Cavern of Souls mana, keeps its
/// prototype mana cost, colour and P/T and the cast-bound effect of Cavern's
/// "that spell can't be countered"; it gains the name sticker's word and the ability sticker's keyword,
/// both of which survive full layer passes, and Counterspell still can't
/// counter it. Supplementary staging (labeled): the only printed Stack placer
/// (sticker kicker, Wicker Picker) is Unimplemented, so the placement is a
/// `PutSticker` instruction run through the sticker resolver.
#[test]
fn stickers_put_on_a_prototyped_uncounterable_spell_keep_its_cast_state() {
    let (mut runner, cavern, proxy, counterspell) = cast_prohibition_paid_proxy(true);
    let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
    set_player_sticker_sheets(
        runner.state_mut(),
        P0,
        &["Ancestral Hot Dog Minotaur".to_string()],
    );
    runner.state_mut().players[0].add_player_counters(&PlayerCounterKind::Ticket, 10);
    assert_eq!(runner.state().objects[&proxy].name, "Arcane Proxy");
    assert!(!runner.state().objects[&proxy]
        .keywords
        .contains(&Keyword::Flying));

    put_sticker_through_resolver(
        &mut runner,
        cavern,
        proxy,
        StickerKind::Name,
        &|sticker| matches!(sticker, AppliedSticker::Name { text, position: 0, .. } if text == "Ancestral"),
    );
    put_sticker_through_resolver(
        &mut runner,
        cavern,
        proxy,
        StickerKind::Ability,
        &|sticker| matches!(sticker, AppliedSticker::Ability { text, .. } if text == "Flying"),
    );
    assert_eq!(
        tickets(runner.state()),
        7,
        "the Flying sticker's ticket cost was paid"
    );

    for pass in 0..3 {
        flush_layers_full(&mut runner);
        assert_same_stack_spell(runner.state(), proxy, occurrence);
        cast_prohibition_assert_spell_form(runner.state(), proxy);
        let object = &runner.state().objects[&proxy];
        assert_eq!(object.stickers.len(), 2, "pass {pass}");
        assert_eq!(object.name, "Ancestral Arcane Proxy", "pass {pass}");
        assert!(
            object.keywords.contains(&Keyword::Flying),
            "pass {pass}: the ability sticker's keyword survives the stack reset"
        );
        assert_eq!(
            cast_prohibition_count(runner.state(), proxy),
            1,
            "pass {pass}"
        );
    }

    let events = cast_prohibition_counter(&mut runner, proxy, counterspell);
    assert_same_stack_spell(runner.state(), proxy, occurrence);
    assert!(!events.iter().any(
        |event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)
    ));
}

/// CR 123.5 + CR 123.7: an ability sticker on a graveyard card is retained as the
/// card is cast to the stack and keeps granting its keyword there, across full
/// layer passes, and then on the battlefield. Marang River Prowler's own
/// permission ("You may cast this card from your graveyard as long as you
/// control a black or green permanent") moves it graveyard -> stack. The Flying
/// sticker is placed on the graveyard card through the sticker resolver
/// (supplementary staging: Scampire's printed ETB is exercised above).
#[test]
fn ability_sticker_retained_from_the_graveyard_keeps_its_keyword_on_the_stack() {
    let (mut runner, prowler) = graveyard_prowler_with_stickers(&[StickerKind::Ability], &[]);
    let card = &runner.state().objects[&prowler];
    assert_eq!(card.zone, Zone::Graveyard);
    assert!(card.keywords.contains(&Keyword::Flying));

    runner.cast(prowler).commit();
    let spell = &runner.state().objects[&prowler];
    assert_eq!(spell.zone, Zone::Stack);
    assert_eq!(spell.cast_from_zone, Some(Zone::Graveyard));
    assert_eq!(spell.stickers.len(), 1, "CR 123.5: retained into the stack");
    for pass in 0..2 {
        flush_layers_full(&mut runner);
        assert!(
            runner.state().objects[&prowler]
                .keywords
                .contains(&Keyword::Flying),
            "pass {pass}: the retained ability sticker's keyword survives the stack reset"
        );
    }

    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve the spell");
    }
    assert_sticker_idle(runner.state());
    let permanent = &runner.state().objects[&prowler];
    assert_eq!(permanent.zone, Zone::Battlefield);
    assert_eq!(permanent.stickers.len(), 1);
    assert!(permanent.keywords.contains(&Keyword::Flying));
}

/// A real Marang River Prowler in P0's graveyard (Grizzly Bears on the
/// battlefield satisfies its graveyard-cast permission; {U}{C}{C} pays for it),
/// carrying one sticker of each `kinds` entry from the "Ancestral Hot Dog
/// Minotaur" sheet placed through the sticker resolver: the "Ancestral" name
/// sticker at the front, the Flying ability sticker, the 1/4 P/T sticker.
/// `hand` cards are added to P0's hand.
fn graveyard_prowler_with_stickers(kinds: &[StickerKind], hand: &[&str]) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("graveyard-cast sticker row requires the CardDB");
    let mut scenario = main_phase();
    let prowler = scenario.add_real_card(P0, "Marang River Prowler", Zone::Graveyard, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    for name in hand {
        scenario.add_real_card(P0, name, Zone::Hand, db);
    }
    scenario.with_mana_pool(
        P0,
        mana(&[ManaType::Blue, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    set_player_sticker_sheets(
        runner.state_mut(),
        P0,
        &["Ancestral Hot Dog Minotaur".to_string()],
    );
    runner.state_mut().players[0].add_player_counters(&PlayerCounterKind::Ticket, 10);
    let card = &runner.state().objects[&prowler];
    assert!(!card.base_keywords.contains(&Keyword::Flying));
    assert_eq!(card.name, "Marang River Prowler");
    assert_eq!((card.base_power, card.base_toughness), (Some(2), Some(1)));

    for kind in kinds {
        let pick: &dyn Fn(&AppliedSticker) -> bool = match kind {
            StickerKind::Name => {
                &|sticker| matches!(sticker, AppliedSticker::Name { text, position: 0, .. } if text == "Ancestral")
            }
            StickerKind::Ability => {
                &|sticker| matches!(sticker, AppliedSticker::Ability { text, .. } if text == "Flying")
            }
            StickerKind::PowerToughness => &|sticker| {
                matches!(
                    sticker,
                    AppliedSticker::PowerToughness {
                        power: 1,
                        toughness: 4,
                        ..
                    }
                )
            },
            StickerKind::Art => panic!("no art sticker row"),
        };
        put_sticker_through_resolver(&mut runner, bears, prowler, *kind, pick);
    }
    flush_layers_full(&mut runner);
    assert_eq!(runner.state().objects[&prowler].stickers.len(), kinds.len());
    (runner, prowler)
}

/// CR 123.1 + CR 707.2 + CR 707.10: a copy of a stickered spell acquires only
/// the spell's copiable values; stickers and their changes aren't copiable.
/// A Marang River Prowler carrying name, Flying and 1/4 P/T stickers is cast
/// from the graveyard and copied with Double Major: the spell keeps all three,
/// while the copy has no stickers, its printed name, no Flying and its printed
/// 2/1 — on the stack and as the token it becomes.
#[test]
fn copy_of_a_stickered_spell_has_none_of_its_stickers() {
    let (mut runner, prowler) = graveyard_prowler_with_stickers(
        &[
            StickerKind::Name,
            StickerKind::Ability,
            StickerKind::PowerToughness,
        ],
        &["Double Major"],
    );
    let double_major = runner.state().players[0]
        .hand
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Double Major")
        .expect("Double Major in hand");
    let assert_stickered = |state: &GameState, id: ObjectId| {
        let object = &state.objects[&id];
        assert_eq!(object.stickers.len(), 3);
        assert_eq!(object.name, "Ancestral Marang River Prowler");
        assert!(object.keywords.contains(&Keyword::Flying));
        assert_eq!((object.power, object.toughness), (Some(1), Some(4)));
    };
    assert_stickered(runner.state(), prowler);

    runner.cast(prowler).commit();
    assert_eq!(runner.state().objects[&prowler].zone, Zone::Stack);
    flush_layers_full(&mut runner);
    assert_stickered(runner.state(), prowler);

    add_mana(&mut runner, 0, &[ManaType::Green, ManaType::Blue]);
    runner.cast(double_major).target_object(prowler).commit();
    cast_prohibition_priority(&mut runner, P0);
    let before: Vec<ObjectId> = runner.state().stack.iter().map(|e| e.id).collect();
    for _ in 0..4 {
        if runner.state().objects[&double_major].zone != Zone::Stack {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Double Major");
    }
    assert_eq!(runner.state().objects[&double_major].zone, Zone::Graveyard);
    let copy = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.id)
        .find(|id| !before.contains(id))
        .expect("Double Major put a copy of the spell on the stack");
    let assert_unstickered = |state: &GameState, id: ObjectId, context: &str| {
        let object = &state.objects[&id];
        assert!(object.stickers.is_empty(), "{context}");
        assert_eq!(object.name, "Marang River Prowler", "{context}");
        assert!(!object.keywords.contains(&Keyword::Flying), "{context}");
        assert_eq!(
            (object.power, object.toughness),
            (Some(2), Some(1)),
            "{context}"
        );
    };
    for pass in 0..2 {
        assert_unstickered(
            runner.state(),
            copy,
            &format!("copy on the stack, pass {pass}"),
        );
        assert_stickered(runner.state(), prowler);
        flush_layers_full(&mut runner);
    }

    // The copy resolves first and becomes a token (CR 707.10f), still unstickered.
    for _ in 0..4 {
        if runner.state().objects[&copy].zone == Zone::Battlefield {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve the copy");
    }
    assert_eq!(runner.state().objects[&copy].zone, Zone::Battlefield);
    assert_unstickered(runner.state(), copy, "token on the battlefield");
    assert_eq!(runner.state().objects[&prowler].zone, Zone::Stack);
    assert_stickered(runner.state(), prowler);
}

/// The sticker pickers the copy rows share: the "Ancestral" name sticker at the
/// front, the Flying ability sticker, and the Hydra sheet's non-keyword
/// activated-ability sticker "{T}: Draw a card, then discard a card."
fn pick_ancestral_name(sticker: &AppliedSticker) -> bool {
    matches!(sticker, AppliedSticker::Name { text, position: 0, .. } if text == "Ancestral")
}
fn pick_flying(sticker: &AppliedSticker) -> bool {
    matches!(sticker, AppliedSticker::Ability { text, .. } if text == "Flying")
}
fn pick_looting(sticker: &AppliedSticker) -> bool {
    matches!(sticker, AppliedSticker::Ability { text, .. } if text.starts_with("{T}: Draw a card"))
}

/// CR 123.5 + CR 123.8 + CR 613.4b: a creature card returned to the battlefield
/// keeps its P/T sticker, and that layer-7b setter can change a
/// population read by another permanent. Grizzly Bears in the graveyard carries
/// the 8/6 sticker (placed through the sticker resolver); the real Zombify
/// returns it. A pre-existing creature whose static reads "a creature with power
/// 4 or greater" (labeled staging: an Oracle-text card) must see the 8/6 entrant
/// at the entry flush, not its printed 2/2.
#[test]
fn entering_pt_stickered_creature_updates_population_reads_of_other_permanents() {
    let db = shared_card_db().expect("entry-flush sticker row requires the CardDB");
    let mut scenario = main_phase();
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let zombify = scenario.add_real_card(P0, "Zombify", Zone::Hand, db);
    let watcher = scenario
        .add_creature_from_oracle(
            P0,
            "Power Watcher",
            1,
            1,
            "As long as you control a creature with power 4 or greater, this creature gets +2/+2.",
        )
        .id();
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    set_player_sticker_sheets(
        runner.state_mut(),
        P0,
        &["Ancestral Hot Dog Minotaur".to_string()],
    );
    runner.state_mut().players[0].add_player_counters(&PlayerCounterKind::Ticket, 10);
    put_sticker_through_resolver(
        &mut runner,
        watcher,
        bears,
        StickerKind::PowerToughness,
        &|sticker| {
            matches!(
                sticker,
                AppliedSticker::PowerToughness {
                    power: 8,
                    toughness: 6,
                    ..
                }
            )
        },
    );
    flush_layers_full(&mut runner);
    assert_eq!(
        pt(runner.state(), bears),
        (Some(8), Some(6)),
        "graveyard 8/6"
    );
    assert_eq!(
        pt(runner.state(), watcher),
        (Some(1), Some(1)),
        "no 4-power creature yet"
    );

    runner.cast(zombify).target_object(bears).commit();
    for _ in 0..4 {
        if runner.state().objects[&zombify].zone != Zone::Stack {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Zombify");
    }
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&bears].stickers.len(), 1);
    assert_eq!(pt(runner.state(), bears), (Some(8), Some(6)), "entrant 8/6");
    assert_eq!(
        pt(runner.state(), watcher),
        (Some(3), Some(3)),
        "the pre-existing watcher reads the 8/6 entrant at the entry flush"
    );
}

fn copy_sticker_sheets(runner: &mut GameRunner) {
    set_player_sticker_sheets(
        runner.state_mut(),
        P0,
        &[
            "Ancestral Hot Dog Minotaur".to_string(),
            "Playable Delusionary Hydra".to_string(),
        ],
    );
    runner.state_mut().players[0].add_player_counters(&PlayerCounterKind::Ticket, 10);
}

fn flying_count(object: &engine::game::game_object::GameObject) -> usize {
    object
        .keywords
        .iter()
        .filter(|keyword| **keyword == Keyword::Flying)
        .count()
}

/// CR 123.1 + CR 707.2 + CR 707.10: a copy of a spell with a non-keyword ability
/// sticker and a sticker duplicating a printed keyword. Storm Crow (printed
/// Flying) is cast, then gets the name, Flying and looting-ability stickers on
/// the stack (supplementary staging: no printed card stickers a spell). Double
/// Major's copy has the printed name, exactly the printed Flying, and none of
/// the sticker's activated ability; the original keeps everything.
#[test]
fn copy_of_a_stickered_spell_keeps_printed_abilities_and_drops_granted_ones() {
    let db = shared_card_db().expect("copy row requires the CardDB");
    let mut scenario = main_phase();
    let crow = scenario.add_real_card(P0, "Storm Crow", Zone::Hand, db);
    let double_major = scenario.add_real_card(P0, "Double Major", Zone::Hand, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_mana_pool(P0, mana(&[ManaType::Blue, ManaType::Colorless]));
    let mut runner = scenario.build();
    copy_sticker_sheets(&mut runner);
    runner.cast(crow).commit();
    assert_eq!(runner.state().objects[&crow].zone, Zone::Stack);
    for (kind, pick) in [
        (
            StickerKind::Name,
            pick_ancestral_name as fn(&AppliedSticker) -> bool,
        ),
        (StickerKind::Ability, pick_flying),
        (StickerKind::Ability, pick_looting),
    ] {
        put_sticker_through_resolver(&mut runner, bears, crow, kind, &pick);
    }
    flush_layers_full(&mut runner);
    let assert_original = |state: &GameState| {
        let object = &state.objects[&crow];
        assert_eq!(object.stickers.len(), 3);
        assert_eq!(object.name, "Ancestral Storm Crow");
        assert!(object.keywords.contains(&Keyword::Flying));
        assert_eq!(
            object.abilities.len(),
            object.base_abilities.len() + 1,
            "the original keeps the looting sticker's ability"
        );
    };
    assert_original(runner.state());
    assert_eq!(
        flying_count(&runner.state().objects[&crow]),
        2,
        "reach guard: printed + sticker"
    );

    add_mana(&mut runner, 0, &[ManaType::Green, ManaType::Blue]);
    runner.cast(double_major).target_object(crow).commit();
    let before: Vec<ObjectId> = runner.state().stack.iter().map(|e| e.id).collect();
    for _ in 0..4 {
        if runner.state().objects[&double_major].zone != Zone::Stack {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Double Major");
    }
    let copy = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.id)
        .find(|id| !before.contains(id))
        .expect("Double Major put a copy of the spell on the stack");
    for pass in 0..2 {
        let object = &runner.state().objects[&copy];
        assert!(object.stickers.is_empty(), "pass {pass}");
        assert_eq!(object.name, "Storm Crow", "pass {pass}");
        assert_eq!(
            flying_count(object),
            1,
            "pass {pass}: exactly the printed Flying"
        );
        assert_eq!(
            object.abilities.len(),
            object.base_abilities.len(),
            "pass {pass}: no sticker-granted activated ability"
        );
        assert_original(runner.state());
        flush_layers_full(&mut runner);
    }
}

/// CR 123.1 + CR 707.2 + CR 707.12: a cast copy of a stickered card has none of
/// its stickers. Divination in the graveyard carries the name, Flying and
/// looting-ability stickers (supplementary staging through the sticker
/// resolver); Mizzix's Mastery exiles it and casts a copy, which has no
/// stickers, the printed name, no Flying and no sticker ability.
#[test]
fn cast_copy_of_a_stickered_card_has_none_of_its_stickers() {
    let db = shared_card_db().expect("cast-copy row requires the CardDB");
    let mut scenario = main_phase();
    let divination = scenario.add_real_card(P0, "Divination", Zone::Graveyard, db);
    let mastery = scenario.add_real_card(P0, "Mizzix's Mastery", Zone::Hand, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    for _ in 0..4 {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    }
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    copy_sticker_sheets(&mut runner);
    for (kind, pick) in [
        (
            StickerKind::Name,
            pick_ancestral_name as fn(&AppliedSticker) -> bool,
        ),
        (StickerKind::Ability, pick_flying),
        (StickerKind::Ability, pick_looting),
    ] {
        put_sticker_through_resolver(&mut runner, bears, divination, kind, &pick);
    }
    flush_layers_full(&mut runner);
    let card = &runner.state().objects[&divination];
    assert_eq!(card.stickers.len(), 3);
    assert_eq!(card.name, "Ancestral Divination");
    assert!(card.keywords.contains(&Keyword::Flying));

    runner.cast(mastery).target_object(divination).commit();
    let mut copy = None;
    for _ in 0..12 {
        copy = runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .find(|id| *id != mastery && *id != divination);
        if copy.is_some() {
            break;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0);
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                assert_eq!(player, P0);
                GameAction::SelectCards { cards }
            }
            other => panic!("unexpected Mizzix's Mastery prompt: {other:?}"),
        };
        runner
            .act(action)
            .expect("Mizzix's Mastery answer is legal");
    }
    let copy = copy.expect("Mizzix's Mastery cast a copy of the exiled card");
    assert_eq!(runner.state().objects[&divination].zone, Zone::Exile);
    for pass in 0..2 {
        let object = &runner.state().objects[&copy];
        assert_eq!(object.zone, Zone::Stack, "pass {pass}");
        assert!(object.stickers.is_empty(), "pass {pass}");
        assert_eq!(object.name, "Divination", "pass {pass}");
        assert!(!object.keywords.contains(&Keyword::Flying), "pass {pass}");
        // The cast-time keyword snapshot is taken at copy birth and never
        // re-derived by a layer pass: the strip ran before it.
        assert!(
            !object.cast_spell_keywords.contains(&Keyword::Flying),
            "pass {pass}"
        );
        assert_eq!(
            object.abilities.len(),
            object.base_abilities.len(),
            "pass {pass}: no sticker-granted activated ability"
        );
        flush_layers_full(&mut runner);
    }
    // CR 123.5: the exiled card itself keeps its stickers.
    assert_eq!(runner.state().objects[&divination].stickers.len(), 3);
}

/// CR 718.3b: the prototyped Arcane Proxy spell's alternative characteristics.
fn cast_prohibition_assert_spell_form(state: &GameState, id: ObjectId) {
    let object = &state.objects[&id];
    assert!(object.prototype_form.is_some());
    assert_eq!((object.power, object.toughness), (Some(2), Some(1)));
    assert_eq!(
        object.mana_cost,
        ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        }
    );
    assert_eq!(object.color, vec![ManaColor::Blue]);
}

#[test]
fn cast_prohibition_restore_insist_real_next_spell_writer() {
    let db = shared_card_db().expect("Insist requires the regenerated integration fixture");
    let mut scenario = main_phase();
    let insist = scenario.add_real_card(P0, "Insist", Zone::Hand, db);
    let proxy = scenario.add_real_card(P0, "Arcane Proxy", Zone::Hand, db);
    let counterspell = scenario.add_real_card(P1, "Counterspell", Zone::Hand, db);
    let later_counter = scenario.add_real_card(P1, "Counterspell", Zone::Hand, db);
    let bounce = scenario.add_real_card(P1, "Unsubstantiate", Zone::Hand, db);
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.with_mana_pool(P0, mana(&[ManaType::Green]));
    let mut runner = scenario.build();
    let outcome = runner.cast(insist).resolve();
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_zone(&[insist], Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().pending_next_spell_modifiers.len(), 1);
    add_mana(&mut runner, 0, &[ManaType::Colorless; 7]);
    runner.cast(proxy).commit();
    assert!(runner.state().pending_next_spell_modifiers.is_empty());
    assert_eq!(cast_prohibition_count(runner.state(), proxy), 1);
    let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
    let mut restored = GameRunner::from_state(cast_prohibition_restore(runner.state(), true));
    cast_prohibition_assert_spell(restored.state(), proxy, occurrence);
    let events = cast_prohibition_counter(&mut restored, proxy, counterspell);
    cast_prohibition_assert_spell(restored.state(), proxy, occurrence);
    assert_eq!(cast_prohibition_count(restored.state(), proxy), 1);
    assert!(!events.iter().any(
        |event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)
    ));
    cast_prohibition_priority(&mut restored, P1);
    add_mana(&mut restored, 1, &[ManaType::Blue, ManaType::Colorless]);
    restored
        .cast(bounce)
        .target_object(proxy)
        .resolve()
        .assert_zone(&[proxy], Zone::Hand);
    assert_eq!(cast_prohibition_count(restored.state(), proxy), 0);
    cast_prohibition_priority(&mut restored, P0);
    add_mana(&mut restored, 0, &[ManaType::Colorless; 7]);
    restored.cast(proxy).commit();
    assert!(restored.state().pending_next_spell_modifiers.is_empty());
    assert_eq!(cast_prohibition_count(restored.state(), proxy), 0);
    let later_events = cast_prohibition_counter(&mut restored, proxy, later_counter);
    assert_eq!(restored.state().objects[&proxy].zone, Zone::Graveyard);
    assert!(later_events.iter().any(
        |event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)
    ));
}

#[test]
fn cast_prohibition_restore_source_departure_and_live_type_loss_latch() {
    let db = shared_card_db().expect("latch tests require CardDB");
    for prototype in [false, true] {
        for depart_source in [false, true] {
            let (mut runner, cavern, proxy, counterspell) = cast_prohibition_paid_proxy(prototype);
            let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
            let source_occurrence =
                ObjectIncarnationRef::from_object(&runner.state().objects[&cavern]);
            if depart_source {
                move_to_zone(runner.state_mut(), cavern, Zone::Graveyard, &mut Vec::new());
                assert_eq!(runner.state().objects[&cavern].zone, Zone::Graveyard);
                assert!(!runner.state().battlefield.contains(&cavern));
                assert!(runner.state().players[0].graveyard.contains(&cavern));
                assert_ne!(
                    ObjectIncarnationRef::from_object(&runner.state().objects[&cavern]),
                    source_occurrence
                );
            }
            let mut restored = cast_prohibition_restore(runner.state(), true);
            for mode in [
                CardDbRehydrationFinalization::Immediate,
                CardDbRehydrationFinalization::Defer,
            ] {
                for _ in 0..2 {
                    rehydrate_game_from_card_db_with_finalization(&mut restored, db, mode);
                    restored.layers_dirty = LayersDirty::Full;
                    flush_layers(&mut restored);
                    cast_prohibition_assert_spell(&restored, proxy, occurrence);
                    cast_prohibition_assert_proxy(&restored, proxy, prototype);
                    assert_eq!(cast_prohibition_count(&restored, proxy), 1);
                    assert!(!restored.objects[&proxy]
                        .base_static_definitions
                        .contains(&StaticDefinition::new(StaticMode::CantBeCountered)));
                }
            }
            if !depart_source {
                // Bounded post-payment isolation: the rider is latched to the
                // spell, so a later live type change cannot recheck its payment.
                restored
                    .objects
                    .get_mut(&proxy)
                    .unwrap()
                    .card_types
                    .core_types
                    .retain(|ty| *ty != CoreType::Creature);
                assert!(!restored.objects[&proxy]
                    .card_types
                    .core_types
                    .contains(&CoreType::Creature));
            }
            let mut restored = GameRunner::from_state(restored);
            let events = cast_prohibition_counter(&mut restored, proxy, counterspell);
            cast_prohibition_assert_spell(restored.state(), proxy, occurrence);
            assert_eq!(cast_prohibition_count(restored.state(), proxy), 1);
            assert!(!events.iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)));
            if !depart_source {
                assert!(!restored.state().objects[&proxy]
                    .card_types
                    .core_types
                    .contains(&CoreType::Creature));
            }
        }
    }
}

#[test]
fn cast_prohibition_restore_actual_resolution_unsummon_normal_recast_countered() {
    let db = shared_card_db().expect("lifetime tests require CardDB");
    for prototype in [false, true] {
        let (mut runner, _, proxy, counterspell) = cast_prohibition_paid_proxy(prototype);
        let spell = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().objects[&proxy].zone, Zone::Battlefield);
        assert!(runner.state().battlefield.contains(&proxy));
        // CR 112.1 + CR 400.7: resolution ends this spell's prohibition, while
        // CR 718.3b retains the chosen prototype characteristics on its permanent.
        let permanent = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
        assert_ne!(permanent, spell);
        assert_eq!(cast_prohibition_count(runner.state(), proxy), 0);
        cast_prohibition_assert_proxy(runner.state(), proxy, prototype);
        for trusted in [false, true] {
            let mut restored = cast_prohibition_restore(runner.state(), trusted);
            for mode in [
                CardDbRehydrationFinalization::Immediate,
                CardDbRehydrationFinalization::Defer,
            ] {
                rehydrate_game_from_card_db_with_finalization(&mut restored, db, mode);
                flush_layers(&mut restored);
                assert_eq!(
                    ObjectIncarnationRef::from_object(&restored.objects[&proxy]),
                    permanent
                );
                cast_prohibition_assert_proxy(&restored, proxy, prototype);
                assert_eq!(cast_prohibition_count(&restored, proxy), 0);
            }
        }
        let unsummon = cast_prohibition_response(runner.state(), "Unsummon");
        cast_prohibition_priority(&mut runner, P1);
        add_mana(&mut runner, 1, &[ManaType::Blue]);
        let outcome = runner.cast(unsummon).target_object(proxy).resolve();
        outcome.assert_zone(&[unsummon], Zone::Graveyard);
        outcome.assert_zone(&[proxy], Zone::Hand);
        assert!(runner.state().players[0].hand.contains(&proxy));
        assert!(!runner.state().players[1].hand.contains(&proxy));
        assert_ne!(
            ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]),
            permanent
        );
        cast_prohibition_assert_proxy(runner.state(), proxy, false);
        cast_prohibition_priority(&mut runner, P0);
        add_mana(&mut runner, 0, &[ManaType::Colorless; 7]);
        runner.cast(proxy).commit();
        let later = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
        assert_ne!(later, spell);
        cast_prohibition_assert_spell(runner.state(), proxy, later);
        cast_prohibition_assert_proxy(runner.state(), proxy, false);
        assert_eq!(cast_prohibition_count(runner.state(), proxy), 0);
        let events = cast_prohibition_counter(&mut runner, proxy, counterspell);
        assert_eq!(runner.state().objects[&proxy].zone, Zone::Graveyard);
        assert!(events.iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)));
    }
}

#[test]
fn cast_prohibition_restore_actual_unsubstantiate_stack_exit() {
    let (mut runner, _, proxy, _) = cast_prohibition_paid_proxy(true);
    let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
    runner = GameRunner::from_state(cast_prohibition_restore(runner.state(), false));
    assert_eq!(cast_prohibition_count(runner.state(), proxy), 1);
    let bounce = cast_prohibition_response(runner.state(), "Unsubstantiate");
    cast_prohibition_priority(&mut runner, P1);
    add_mana(&mut runner, 1, &[ManaType::Blue, ManaType::Colorless]);
    let outcome = runner.cast(bounce).target_object(proxy).resolve();
    outcome.assert_zone(&[bounce], Zone::Graveyard);
    outcome.assert_zone(&[proxy], Zone::Hand);
    assert!(outcome.events().iter().all(
        |event| !matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)
    ));
    assert!(runner.state().players[0].hand.contains(&proxy));
    assert_ne!(
        ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]),
        occurrence
    );
    assert_eq!(cast_prohibition_count(runner.state(), proxy), 0);
    cast_prohibition_assert_proxy(runner.state(), proxy, false);
}

#[test]
fn cast_prohibition_restore_current_spell_authority_and_exact_definition_isolation() {
    let db = shared_card_db().expect("authority isolation requires CardDB");
    let canonical = StaticDefinition::new(StaticMode::CantBeCountered);
    for prototype in [false, true] {
        let (runner, _, proxy, _) = cast_prohibition_paid_proxy(prototype);
        // These are bounded engine-shape supplements to the public paid/counter
        // tests. Every case reaches a successful ordinary printed-face refresh.
        for mode in [
            CardDbRehydrationFinalization::Immediate,
            CardDbRehydrationFinalization::Defer,
        ] {
            for carrier in [false, true] {
                for hostile in [
                    None,
                    Some("phantom"),
                    Some("ability"),
                    Some("entry"),
                    Some("source"),
                    Some("card"),
                    Some("zone"),
                ] {
                    let mut state = runner.state().clone();
                    let occurrence = ObjectIncarnationRef::from_object(&state.objects[&proxy]);
                    if carrier {
                        // The same transfer performed by begin_resolving_stack_entry:
                        // an already-popped Spell, no TriggerFiring, one carrier.
                        assert!(state.resolving_stack_entry.is_none());
                        state.resolving_stack_entry =
                            Some(state.stack.pop_back().expect("actual paid Spell"));
                    }
                    let entry = if carrier {
                        state.resolving_stack_entry.as_mut().unwrap()
                    } else {
                        state
                            .stack
                            .iter_mut()
                            .find(|entry| entry.id == proxy)
                            .unwrap()
                    };
                    match hostile {
                        Some("ability") => {
                            entry.kind = StackEntryKind::ActivatedAbility {
                                source_id: proxy,
                                ability: Box::new(ResolvedAbility::new(
                                    Effect::NoOp,
                                    vec![],
                                    proxy,
                                    P0,
                                )),
                            }
                        }
                        Some("entry") => entry.id = ObjectId(proxy.0 + 1000),
                        Some("source") => entry.source_id = ObjectId(proxy.0 + 1000),
                        Some("card") => {
                            let StackEntryKind::Spell { card_id, .. } = &mut entry.kind else {
                                unreachable!()
                            };
                            *card_id = CardId(card_id.0 + 1000);
                        }
                        Some("zone") => state.objects.get_mut(&proxy).unwrap().zone = Zone::Hand,
                        Some("phantom") => {
                            state.stack.clear();
                            state.resolving_stack_entry = None;
                        }
                        None => {}
                        Some(other) => panic!("unhandled isolation fixture {other}"),
                    }
                    let object = state.objects.get_mut(&proxy).unwrap();
                    assert!(object
                        .printed_ref
                        .as_ref()
                        .is_some_and(|reference| db.get_face_by_printed_ref(reference).is_some()));
                    object.name = "stale display".to_string();
                    object.base_power = Some(99);
                    assert_eq!(cast_prohibition_count(&state, proxy), 1);
                    for _ in 0..2 {
                        rehydrate_game_from_card_db_with_finalization(&mut state, db, mode);
                        assert_eq!(state.objects[&proxy].name, "Arcane Proxy");
                        assert_eq!(state.objects[&proxy].base_power, Some(4));
                        assert_eq!(
                            ObjectIncarnationRef::from_object(&state.objects[&proxy]),
                            occurrence
                        );
                        assert_eq!(
                            cast_prohibition_count(&state, proxy),
                            usize::from(hostile.is_none()),
                            "{hostile:?}, carrier={carrier}, mode={mode:?}"
                        );
                        assert!(!state.objects[&proxy]
                            .base_static_definitions
                            .contains(&canonical));
                    }
                }
            }
            for payload in [
                "absent",
                "affected",
                "condition",
                "description",
                "zone",
                "base",
                "canonical_and_unrelated",
                "duplicate",
            ] {
                let mut state = runner.state().clone();
                let object = state.objects.get_mut(&proxy).unwrap();
                let mut modified = canonical.clone();
                match payload {
                    "affected" => modified.affected = Some(TargetFilter::SelfRef),
                    "condition" => modified.condition = Some(StaticCondition::DuringYourTurn),
                    "description" => modified.description = Some("different payload".to_string()),
                    "zone" => modified.active_zones = vec![Zone::Stack],
                    "base" => object.base_static_definitions = Arc::new(vec![canonical.clone()]),
                    "absent" | "canonical_and_unrelated" | "duplicate" => {}
                    other => panic!("unhandled payload fixture {other}"),
                }
                object.static_definitions = if payload == "absent" {
                    vec![]
                } else if payload == "canonical_and_unrelated" {
                    vec![
                        canonical.clone(),
                        StaticDefinition::new(StaticMode::CantBeCopied),
                    ]
                } else if payload == "duplicate" {
                    vec![canonical.clone(), canonical.clone()]
                } else {
                    vec![modified.clone()]
                }
                .into();
                object.name = "stale display".to_string();
                let expected =
                    usize::from(matches!(payload, "canonical_and_unrelated" | "duplicate"));
                for _ in 0..2 {
                    rehydrate_game_from_card_db_with_finalization(&mut state, db, mode);
                    assert_eq!(state.objects[&proxy].name, "Arcane Proxy");
                    assert_eq!(
                        cast_prohibition_count(&state, proxy),
                        expected,
                        "payload={payload}"
                    );
                    assert_eq!(state.objects[&proxy].static_definitions.len(), expected);
                    assert!(state.objects[&proxy].base_static_definitions.is_empty());
                }
            }
        }
    }
}

#[test]
fn cast_prohibition_restore_plain_colorless_and_unspent_mana_are_counterable() {
    let db = shared_card_db().expect("counter controls require CardDB");
    for selected_color in [None, Some(ManaType::Colorless), Some(ManaType::Blue)] {
        let mut scenario = main_phase();
        let cavern = scenario.add_real_card(P0, "Cavern of Souls", Zone::Hand, db);
        let proxy = scenario.add_real_card(P0, "Arcane Proxy", Zone::Hand, db);
        let counterspell = scenario.add_real_card(P1, "Counterspell", Zone::Hand, db);
        let second_source = scenario.add_real_card(P0, "Delighted Halfling", Zone::Battlefield, db);
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&cavern].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: cavern,
                card_id,
            })
            .expect("play actual Cavern");
        let WaitingFor::NamedChoice {
            player, options, ..
        } = &runner.state().waiting_for
        else {
            panic!("actual Cavern choice required")
        };
        assert_eq!(*player, P0);
        assert!(options.iter().any(|option| option == "Wizard"));
        runner
            .act(GameAction::ChooseOption {
                choice: "Wizard".to_string(),
            })
            .unwrap();
        if let Some(color) = selected_color {
            let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
            let action = grouped[&cavern].iter().find(|action| matches!(action, GameAction::TapLandForMana { selection }
                if selection.mana_type == color && selection.output == ManaSourceOutput::Concrete(color)
                    && (color != ManaType::Colorless || selection.restrictions.is_empty())))
                .expect("real selected Cavern operation").clone();
            runner.act(action).expect("produce actual source mana");
            assert!(runner.state().objects[&cavern].tapped);
            let units: Vec<_> = runner.state().players[0].mana_pool.units().collect();
            assert_eq!(units.len(), 1);
            assert_eq!(units[0].source_id, cavern);
            assert_eq!(units[0].color, color);
            assert_eq!(!units[0].grants.is_empty(), color == ManaType::Blue);
        }
        if selected_color == Some(ManaType::Blue) {
            let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
            let action = grouped[&second_source].iter().find(|action| match action {
                GameAction::ActivateAbility { source_id, ability_index, .. } if *source_id == second_source => {
                    matches!(&*runner.state().objects[&second_source].abilities[*ability_index].effect,
                        Effect::Mana { grants, .. } if matches!(grants.as_slice(), [ManaSpellGrant::CantBeCountered { .. }]))
                }
                _ => false,
            }).expect("engine issues the second source's ordinary colored mana activation").clone();
            runner
                .act(action)
                .expect("activate actual second-source ability");
            let WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("Halfling's ordinary activation genuinely requires a color choice");
            };
            assert_eq!(*player, P0);
            assert!(options.contains(&ManaType::Blue));
            // The primary Cavern Concrete Blue action has no second answer.
            // This separate ordinary nonland activation owns its real prompt.
            let (actions, _, _) = engine::ai_support::legal_actions_full(runner.state());
            let answer = actions
                .into_iter()
                .find(|action| {
                    matches!(
                        action,
                        GameAction::ChooseManaColor {
                            choice: ManaChoice::SingleColor(ManaType::Blue),
                            count: 1
                        }
                    )
                })
                .expect("engine offers the required secondary Blue answer");
            runner
                .act(answer)
                .expect("complete actual second-source production");
            assert!(runner.state().objects[&second_source].tapped);
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
            );
            let units: Vec<_> = runner.state().players[0].mana_pool.units().collect();
            assert_eq!(units.len(), 2);
            assert!(units.iter().any(|unit| unit.source_id == second_source
                && matches!(
                    unit.grants.as_slice(),
                    [ManaSpellGrant::CantBeCountered { .. }]
                )));
        }
        add_mana(&mut runner, 0, &[ManaType::Colorless; 7]);
        runner.cast(proxy).commit();
        assert_eq!(cast_prohibition_count(runner.state(), proxy), 0);
        if selected_color == Some(ManaType::Blue) {
            let units: Vec<_> = runner.state().players[0].mana_pool.units().collect();
            assert_eq!(
                units.len(),
                2,
                "unrestricted generic payment leaves both actual granting units unspent"
            );
            for source in [cavern, second_source] {
                assert!(units.iter().any(|unit| unit.source_id == source
                    && matches!(
                        unit.grants.as_slice(),
                        [ManaSpellGrant::CantBeCountered { .. }]
                    )));
            }
        }
        let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
        let mut restored = GameRunner::from_state(cast_prohibition_restore(runner.state(), true));
        cast_prohibition_assert_spell(restored.state(), proxy, occurrence);
        assert_eq!(cast_prohibition_count(restored.state(), proxy), 0);
        let events = cast_prohibition_counter(&mut restored, proxy, counterspell);
        assert_eq!(restored.state().objects[&proxy].zone, Zone::Graveyard);
        assert!(events.iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)));
    }
}

#[test]
fn cast_prohibition_restore_printed_intrinsic_shepherd_remains_protected() {
    let db = shared_card_db().expect("intrinsic control requires CardDB");
    let mut scenario = main_phase();
    let shepherd = scenario.add_real_card(P0, "Allosaurus Shepherd", Zone::Hand, db);
    let counterspell = scenario.add_real_card(P1, "Counterspell", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(&[ManaType::Green]));
    let mut runner = scenario.build();
    runner.cast(shepherd).commit();
    let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&shepherd]);
    let intrinsic = runner.state().objects[&shepherd]
        .base_static_definitions
        .clone();
    assert!(intrinsic
        .iter()
        .any(|definition| definition.mode == StaticMode::CantBeCountered
            && definition.affected == Some(TargetFilter::SelfRef)));
    for trusted in [false, true] {
        let restored = cast_prohibition_restore(runner.state(), trusted);
        assert_eq!(
            restored.objects[&shepherd].base_static_definitions,
            intrinsic
        );
        let mut restored = GameRunner::from_state(restored);
        let events = cast_prohibition_counter(&mut restored, shepherd, counterspell);
        cast_prohibition_assert_spell(restored.state(), shepherd, occurrence);
        assert_eq!(
            restored.state().objects[&shepherd].base_static_definitions,
            intrinsic
        );
        assert!(!events.iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == shepherd)));
    }
}

fn mana(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|&t| ManaUnit::new(t, ObjectId(0), false, vec![]))
        .collect()
}

fn add_mana(runner: &mut GameRunner, player: usize, types: &[ManaType]) {
    for unit in mana(types) {
        runner.state_mut().players[player].mana_pool.add(unit);
    }
}

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn grist_in_hand(scenario: &mut GameScenario, player: engine::types::player::PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, "Grist, the Hunger Tide", false, GRIST_ORACLE)
        .as_planeswalker_with_loyalty("Grist", 3)
        .as_legendary()
        .with_mana_cost(grist_cost())
        .id()
}

fn grist_in_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_graveyard(P0, "Grist, the Hunger Tide", false)
        .from_oracle_text(GRIST_ORACLE)
        .as_planeswalker_with_loyalty("Grist", 3)
        .as_legendary()
        .with_mana_cost(grist_cost())
        .id()
}

fn types_of(runner: &GameRunner, id: ObjectId) -> Vec<CoreType> {
    runner.state().objects[&id].card_types.core_types.clone()
}

fn assert_creature_card_grist(state: &engine::types::game_state::GameState, id: ObjectId) {
    let obj = &state.objects[&id];
    assert_eq!(
        obj.card_types.core_types,
        vec![CoreType::Planeswalker, CoreType::Creature],
        "zone {:?}",
        obj.zone
    );
    assert_eq!(
        obj.card_types.subtypes,
        vec!["Grist".to_string(), "Insect".to_string()]
    );
    assert_eq!((obj.power, obj.toughness), (Some(1), Some(1)));
}

fn assert_only_a_planeswalker(state: &engine::types::game_state::GameState, id: ObjectId) {
    let obj = &state.objects[&id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.card_types.core_types, vec![CoreType::Planeswalker]);
    assert_eq!(obj.card_types.subtypes, vec!["Grist".to_string()]);
    assert!(obj.card_types.supertypes.contains(&Supertype::Legendary));
    assert_eq!((obj.power, obj.toughness), (None, None));
    assert_eq!(obj.counters.get(&CounterType::Loyalty).copied(), Some(3));
}

/// I1: Grist in hand is a creature card; Duress cannot take her (CR 113.6c).
#[test]
fn hand_grist_is_a_creature_card_so_duress_cannot_take_it() {
    let mut scenario = main_phase();
    let duress = scenario
        .add_spell_to_hand_from_oracle(P0, "Duress", false, DURESS)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let grist = grist_in_hand(&mut scenario, P1);
    let other = scenario
        .add_spell_to_hand(P1, "Other Sorcery", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(P0, mana(&[ManaType::Black]));
    let mut runner = scenario.build();

    let outcome = runner.cast(duress).target_player(P1).resolve();
    let WaitingFor::RevealChoice { cards, .. } = outcome.final_waiting_for() else {
        panic!(
            "expected RevealChoice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    // Reach guard: the noncreature nonland card is offered; Grist is a creature.
    assert_eq!(cards, &vec![other]);
    assert_eq!(outcome.zone_of(grist), Zone::Hand);
    assert!(outcome.state().objects[&grist]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
}

/// I1b: a hand holding only Grist leaves Duress nothing to take.
#[test]
fn duress_against_a_hand_of_only_grist_takes_nothing() {
    let mut scenario = main_phase();
    let duress = scenario
        .add_spell_to_hand_from_oracle(P0, "Duress", false, DURESS)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let grist = grist_in_hand(&mut scenario, P1);
    scenario.with_mana_pool(P0, mana(&[ManaType::Black]));
    let mut runner = scenario.build();

    let outcome = runner.cast(duress).target_player(P1).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no choice to make, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(outcome.zone_of(grist), Zone::Hand);
    assert_eq!(outcome.zone_count(P1, Zone::Graveyard), 0);
    // Reach guard: Duress itself resolved.
    assert_eq!(outcome.zone_of(duress), Zone::Graveyard);
}

fn glimpse_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = main_phase();
    let glimpse = scenario
        .add_spell_to_hand_from_oracle(P0, "Glimpse of Nature", false, GLIMPSE_OF_NATURE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    scenario.with_library_top(P0, &["Lib A", "Lib B", "Lib C"]);
    scenario.with_mana_pool(P0, mana(&[ManaType::Green]));
    (scenario, glimpse)
}

/// I2: Grist on the stack is a creature spell (Glimpse of Nature draws).
#[test]
fn grist_on_the_stack_is_a_creature_spell() {
    let (mut scenario, glimpse) = glimpse_scenario();
    let grist = grist_in_hand(&mut scenario, P0);
    let mut runner = scenario.build();
    runner.cast(glimpse).resolve();
    add_mana(
        &mut runner,
        0,
        &[ManaType::Black, ManaType::Green, ManaType::Colorless],
    );

    let outcome = runner.cast(grist).resolve();
    outcome.assert_hand_drawn(P0, 1);
    assert_only_a_planeswalker(outcome.state(), grist);
}

/// I2 paired controls: under the same delayed trigger a Bear draws and a
/// noncreature spell does not.
#[test]
fn glimpse_of_nature_draws_for_creature_spells_only() {
    let (mut scenario, glimpse) = glimpse_scenario();
    let bear = scenario
        .add_creature_to_hand(P0, "Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let sorcery = scenario
        .add_spell_to_hand(P0, "Plain Sorcery", false)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    runner.cast(glimpse).resolve();
    add_mana(&mut runner, 0, &[ManaType::Colorless, ManaType::Colorless]);
    runner.cast(sorcery).resolve().assert_hand_drawn(P0, 0);
    runner.cast(bear).resolve().assert_hand_drawn(P0, 1);
}

/// I3: Grist in the library is a green creature card of mana value 3 (Green
/// Sun's Zenith, X = 3); she arrives as a planeswalker.
#[test]
fn library_grist_is_a_green_creature_card_green_suns_zenith_finds() {
    let mut scenario = main_phase();
    let zenith = scenario
        .add_spell_to_hand_from_oracle(P0, "Green Sun's Zenith", false, GREEN_SUNS_ZENITH)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
        })
        .id();
    let red_bear = scenario
        .add_spell_to_library_top(P0, "Red Bear", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(1))
        .with_color(vec![ManaColor::Red])
        .id();
    let grist = scenario
        .add_spell_to_library_top(P0, "Grist, the Hunger Tide", false)
        .from_oracle_text(GRIST_ORACLE)
        .as_planeswalker_with_loyalty("Grist", 3)
        .as_legendary()
        .with_mana_cost(grist_cost())
        .id();
    let warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    let outcome = runner.cast(zenith).x(3).resolve();
    let WaitingFor::SearchChoice { cards, .. } = outcome.final_waiting_for() else {
        panic!(
            "expected SearchChoice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    // Reach guard: the prompt exists; the red creature card is not offered.
    assert_eq!(cards, &vec![grist]);
    assert!(!cards.contains(&red_bear));

    runner
        .act(GameAction::SelectCards { cards: vec![grist] })
        .expect("select Grist");
    runner.advance_until_stack_empty();
    assert_only_a_planeswalker(runner.state(), grist);
    // A planeswalker entering is not "another creature": Soul Warden stays quiet.
    assert!(runner.state().objects.contains_key(&warden));
    assert_eq!(runner.state().players[0].life, life_before);
}

/// I4: on the battlefield Grist is only a planeswalker, however she got there
/// (cast from hand), and a Bear entering under the same Soul Warden is the
/// paired positive.
#[test]
fn battlefield_grist_is_only_a_planeswalker() {
    let mut scenario = main_phase();
    let grist = grist_in_hand(&mut scenario, P0);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    scenario.with_mana_pool(
        P0,
        mana(&[ManaType::Black, ManaType::Green, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(grist).resolve();
    outcome.assert_life_delta(P0, 0);
    assert_only_a_planeswalker(outcome.state(), grist);

    let mut scenario = main_phase();
    let bear = scenario
        .add_creature_to_hand(P0, "Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    scenario.with_mana_pool(P0, mana(&[ManaType::Colorless]));
    let mut runner = scenario.build();
    runner.cast(bear).resolve().assert_life_delta(P0, 1);
}

fn minus_five_scenario(loyalty: u32) -> (GameRunner, ObjectId) {
    let mut scenario = main_phase();
    let grist = scenario
        .add_planeswalker_from_oracle(P0, "Grist, the Hunger Tide", "Grist", loyalty, GRIST_ORACLE)
        .as_legendary()
        .with_mana_cost(grist_cost())
        .id();
    grist_in_graveyard(&mut scenario);
    (scenario.build(), grist)
}

/// I5: the -5 counts a Grist placed directly in the graveyard as a creature card.
#[test]
fn minus_five_counts_a_graveyard_grist_as_a_creature_card() {
    let mut scenario = main_phase();
    let grist = scenario
        .add_planeswalker_from_oracle(P0, "Grist, the Hunger Tide", "Grist", 6, GRIST_ORACLE)
        .as_legendary()
        .with_mana_cost(grist_cost())
        .id();
    grist_in_graveyard(&mut scenario);
    scenario.add_creature_to_graveyard(P0, "Bear", 2, 2);
    scenario.add_spell_to_graveyard(P0, "Sorcery", false);
    let mut runner = scenario.build();
    flush_layers_full(&mut runner);

    let outcome = runner.activate(grist, 2).resolve();
    // Grist + Bear; the sorcery is not counted.
    outcome.assert_life_delta(P1, -2);
    assert_eq!(outcome.counters(grist, CounterType::Loyalty), 1);
    assert_eq!(outcome.zone_of(grist), Zone::Battlefield);
}

/// I6: the ruling example. Paying -5 from loyalty 5 leaves Grist at 0; she dies
/// to state-based actions before the ability resolves and counts herself.
#[test]
fn minus_five_from_loyalty_five_counts_the_dying_grist() {
    let (mut runner, grist) = minus_five_scenario(5);
    flush_layers_full(&mut runner);
    let outcome = runner.activate(grist, 2).resolve();
    outcome.assert_life_delta(P1, -2);
    assert_eq!(outcome.zone_of(grist), Zone::Graveyard);
}

fn flush_layers_full(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    flush_layers(runner.state_mut());
}

/// I7: Zombify legally targets the graveyard Grist (a creature card) and she
/// arrives as a planeswalker; Hero's Downfall puts her back as an Insect creature card.
#[test]
fn zombified_grist_is_a_planeswalker_again_and_a_creature_card_after_dying() {
    let mut scenario = main_phase();
    let grist = grist_in_graveyard(&mut scenario);
    let zombify = scenario
        .add_spell_to_hand_from_oracle(P0, "Zombify", false, ZOMBIFY)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let downfall = scenario
        .add_spell_to_hand_from_oracle(P0, "Hero's Downfall", true, HEROS_DOWNFALL)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(P0, mana(&[ManaType::Black; 7]));
    let mut runner = scenario.build();
    flush_layers_full(&mut runner);
    assert_creature_card_grist(runner.state(), grist);

    let outcome = runner.cast(zombify).target_objects(&[grist]).resolve();
    assert_only_a_planeswalker(outcome.state(), grist);

    let outcome = runner.cast(downfall).target_objects(&[grist]).resolve();
    assert_eq!(outcome.zone_of(grist), Zone::Graveyard);
    assert_creature_card_grist(outcome.state(), grist);
}

/// I8: a commander Grist in the command zone is a creature card, and a planeswalker
/// again once she is on the battlefield.
#[test]
fn command_zone_grist_is_a_creature_card() {
    let mut scenario = main_phase();
    let grist = grist_in_hand(&mut scenario, P0);
    scenario.with_commander(grist);
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&grist].zone, Zone::Command);
    flush_layers_full(&mut runner);
    assert_creature_card_grist(runner.state(), grist);

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), grist, Zone::Battlefield, &mut events);
    flush_layers_full(&mut runner);
    assert_eq!(types_of(&runner, grist), vec![CoreType::Planeswalker]);
}

/// I9: a milled Grist is an Insect creature
/// card in the graveyard at once.
#[test]
fn milled_grist_is_an_insect_creature_card_at_once() {
    let mut scenario = main_phase();
    let mind_sculpt = scenario
        .add_spell_to_hand_from_oracle(P0, "Mind Sculpt", false, MIND_SCULPT)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let grist = scenario
        .add_spell_to_library_top(P1, "Grist, the Hunger Tide", false)
        .from_oracle_text(GRIST_ORACLE)
        .as_planeswalker_with_loyalty("Grist", 3)
        .as_legendary()
        .with_mana_cost(grist_cost())
        .id();
    scenario.add_spell_to_library_top(P1, "Filler", false);
    scenario.with_mana_pool(P0, mana(&[ManaType::Blue, ManaType::Colorless]));
    let mut runner = scenario.build();
    let library_before = runner.state().players[1].library.len();

    let outcome = runner.cast(mind_sculpt).target_player(P1).resolve();
    // Reach guard: the library was milled.
    assert!(library_before >= 2 && outcome.state().players[1].library.is_empty());
    assert_eq!(outcome.zone_of(grist), Zone::Graveyard);
    assert!(matches!(outcome.state().layers_dirty, LayersDirty::Clean));
    assert_creature_card_grist(outcome.state(), grist);
}

/// I10: the static lapses when its condition stops holding while the card stays in
/// the graveyard; types and power/toughness return to the printed values.
#[test]
fn declared_zone_static_lapses_when_the_forest_leaves() {
    let mut scenario = main_phase();
    let card = scenario
        .add_spell_to_graveyard(P0, "Testcard", false)
        .from_oracle_text(TESTCARD_ORACLE)
        .as_planeswalker_with_loyalty("Test", 3)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    flush_layers_full(&mut runner);
    let obj = &runner.state().objects[&card];
    assert_eq!(
        obj.card_types.core_types,
        vec![CoreType::Planeswalker, CoreType::Creature]
    );
    assert_eq!((obj.power, obj.toughness), (Some(1), Some(1)));

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), forest, Zone::Graveyard, &mut events);
    flush_layers_full(&mut runner);
    let obj = &runner.state().objects[&card];
    assert_eq!(obj.card_types.core_types, vec![CoreType::Planeswalker]);
    assert_eq!((obj.power, obj.toughness), (None, None));
}

/// I11: when Painter's Servant leaves, a hand card goes back to its own colour
/// (CR 611.3b + CR 613.1e: the colour effect applies only while its source is on the battlefield).
#[test]
fn painters_servant_leaving_returns_a_hand_card_to_its_own_colour() {
    let mut scenario = main_phase();
    let servant = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTER_ORACLE)
        .as_artifact()
        .id();
    let card = scenario
        .add_spell_to_hand(P0, "Blue Card", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&servant)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));
    flush_layers_full(&mut runner);
    assert_eq!(
        runner.state().objects[&card].color,
        vec![ManaColor::Blue, ManaColor::Red]
    );

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), servant, Zone::Graveyard, &mut events);
    flush_layers_full(&mut runner);
    assert_eq!(runner.state().objects[&card].color, vec![ManaColor::Blue]);
}

/// I12 (CR 718.3b): a really prototype-cast creature spell keeps its prototype
/// colours and power/toughness across layer passes under Painter's Servant.
#[test]
fn prototype_cast_spell_keeps_its_prototype_characteristics_under_painters_servant() {
    let mut scenario = main_phase();
    let servant = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTER_ORACLE)
        .as_artifact()
        .id();
    let proxy = scenario
        .add_creature_to_hand(P0, "Arcane Proxy", 4, 3)
        .as_artifact()
        .as_creature()
        .with_mana_cost(ManaCost::generic(7))
        .from_oracle_text(ARCANE_PROXY)
        .id();
    scenario.with_mana_pool(
        P0,
        mana(&[ManaType::Blue, ManaType::Blue, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&servant)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Green));
    {
        let committed = runner
            .cast(proxy)
            .casting_variant(CastingVariant::Prototype)
            .commit();
        assert_eq!(committed.state().objects[&proxy].zone, Zone::Stack);
    }
    for pass in 0..3 {
        if pass > 0 {
            flush_layers_full(&mut runner);
        }
        let obj = &runner.state().objects[&proxy];
        // Reach guards: it really is the prototyped spell, and Green was gained.
        assert!(obj.prototype_form.is_some(), "pass {pass}");
        assert!(obj.color.contains(&ManaColor::Green), "pass {pass}");
        assert_eq!(
            obj.color,
            vec![ManaColor::Blue, ManaColor::Green],
            "pass {pass}"
        );
        assert_eq!(
            (obj.power, obj.toughness),
            (Some(2), Some(1)),
            "pass {pass}"
        );
    }
}

// Sticker rows. Every CardDB consumer below (Raise Dead included) requires the
// canonical gzip fixture regenerated with scripts/gen-test-fixture.py from the
// production export; the regenerated JSON must be re-gzipped.
fn sticker_sheet(runner: &mut GameRunner) {
    set_player_sticker_sheets(
        runner.state_mut(),
        P0,
        &["Playable Delusionary Hydra".to_string()],
    );
    runner.state_mut().players[0].add_player_counters(&PlayerCounterKind::Ticket, 10);
}

fn tickets(state: &GameState) -> u32 {
    state.players[0].player_counter(&PlayerCounterKind::Ticket)
}

fn pt(state: &GameState, id: ObjectId) -> (Option<i32>, Option<i32>) {
    (state.objects[&id].power, state.objects[&id].toughness)
}

/// card-test completion: Priority, empty stack and resolution stack, and no
/// pending or deferred trigger.
fn assert_sticker_idle(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    assert!(state.pending_trigger.is_none());
    assert!(state.pending_trigger_entry.is_none());
    assert!(state.deferred_triggers.is_empty());
}

fn is_hydra_one_five(sticker: &AppliedSticker) -> bool {
    matches!(sticker, AppliedSticker::PowerToughness { locator, ticket_cost: 2, power: 1, toughness: 5, .. }
        if locator.sheet == "Playable Delusionary Hydra" && locator.index == 8)
}

/// Settles a real "you get {TK}, then you may put a sticker on ..." ETB through
/// its public prompts, choosing `target` and the cost-2 1/5 P/T sticker. Returns
/// (optional prompts, branch prompts, recipient options offered).
fn settle_sticker_etb(
    runner: &mut GameRunner,
    source: ObjectId,
    target: Option<ObjectId>,
    accept: bool,
) -> (usize, usize, Vec<ObjectId>) {
    let mut trigger_seen = false;
    let mut optional_seen = 0;
    let mut branch_seen = 0;
    let mut placed = 0;
    let mut offered = Vec::new();
    for _ in 0..64 {
        trigger_seen |= runner.state().stack.iter().any(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        });
        if runner.state().objects[&source].zone == Zone::Battlefield
            && runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                up_to,
                ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(count, 1);
                assert!(!up_to);
                assert!(offered.is_empty(), "exactly one recipient prompt");
                offered = cards.clone();
                let selected = target.expect("recipient choice requires an actual recipient");
                assert!(cards.contains(&selected));
                GameAction::SelectCards {
                    cards: vec![selected],
                }
            }
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(source_id, source);
                // CR 608.2c: the ticket instruction precedes the optional placement.
                assert_eq!(tickets(runner.state()), 11);
                optional_seen += 1;
                GameAction::DecideOptionalEffect { accept }
            }
            WaitingFor::ChooseOneOfBranch {
                player, branches, ..
            } => {
                assert_eq!(player, P0);
                branch_seen += 1;
                let index = branches
                    .iter()
                    .position(|branch| matches!(branch.effect.as_ref(),
                        Effect::ApplySticker { sticker, pay_ticket: true, .. } if is_hydra_one_five(sticker)))
                    .expect("the real cost-2 1/5 P/T sticker must be offered");
                GameAction::ChooseBranch { index }
            }
            other => panic!("unexpected real sticker ETB prompt: {other:?}"),
        };
        let outcome = runner
            .act(action)
            .expect("real sticker ETB answer is legal");
        placed += outcome
            .events
            .iter()
            .filter(|event| {
                matches!(event,
            GameEvent::StickerPlaced { object_id, player_id: P0, .. } if Some(*object_id) == target)
            })
            .count();
    }
    assert!(trigger_seen, "the real sticker ETB must reach the stack");
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_sticker_idle(runner.state());
    assert_eq!(placed, branch_seen);
    (optional_seen, branch_seen, offered)
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

type ScampireScenario = (
    GameRunner,
    ObjectId,
    Option<ObjectId>,
    ObjectId,
    Option<ObjectId>,
);

fn scampire_scenario(with_target: bool, sheets: bool) -> ScampireScenario {
    let db = shared_card_db().expect("production sticker fixture loads");
    let mut scenario = main_phase();
    let source = scenario.add_real_card(P0, "Scampire", Zone::Hand, db);
    let target = with_target
        .then(|| scenario.add_real_card(P0, "Grist, the Hunger Tide", Zone::Graveyard, db));
    let owned_bears =
        with_target.then(|| scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db));
    let other_owner = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    scenario.with_mana_pool(
        P0,
        mana(&[ManaType::Black, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    if sheets {
        sticker_sheet(&mut runner);
    } else {
        runner.state_mut().players[0].add_player_counters(&PlayerCounterKind::Ticket, 10);
    }
    runner.cast(source).commit();
    (runner, source, target, other_owner, owned_bears)
}

/// P1 - CR 123.8 + CR 613.4b + CR 613.7a + CR 613.7k: Scampire's real ETB puts
/// the 1/5 P/T sticker on a graveyard Grist. Grist's own 1/1 static has Grist's
/// object timestamp; the sticker's later placement timestamp wins in layer 7b.
#[test]
fn scampire_real_etb_orders_graveyard_sticker_after_grist_static_and_restores() {
    let (mut runner, source, target, other_owner, owned_bears) = scampire_scenario(true, true);
    let (target, owned_bears) = (target.unwrap(), owned_bears.unwrap());
    let (optional, branches, offered) = settle_sticker_etb(&mut runner, source, Some(target), true);
    assert_eq!((optional, branches), (1, 1));
    // CR 123.3b: only the owner's creature cards are offered.
    assert_eq!(sorted(offered), sorted(vec![target, owned_bears]));
    assert_eq!(tickets(runner.state()), 9);
    assert!(runner.state().objects[&other_owner].stickers.is_empty());
    assert!(runner.state().objects[&owned_bears].stickers.is_empty());
    let grist = &runner.state().objects[&target];
    assert_eq!(grist.zone, Zone::Graveyard);
    assert_eq!(grist.stickers.len(), 1);
    assert!(is_hydra_one_five(&grist.stickers[0]));
    assert!(grist.card_types.core_types.contains(&CoreType::Creature));
    assert!(grist.card_types.subtypes.iter().any(|s| s == "Insect"));
    assert_eq!(pt(runner.state(), target), (Some(1), Some(5)));
    for _ in 0..2 {
        flush_layers_full(&mut runner);
        assert_eq!(
            pt(runner.state(), target),
            (Some(1), Some(5)),
            "the newer sticker wins over the older declared-zone 1/1 static"
        );
        assert!(runner.state().objects[&target]
            .card_types
            .core_types
            .contains(&CoreType::Creature));
    }
    for trusted in [false, true] {
        // Production restore finalization settles the sticker without a forced pass.
        let restored = cast_prohibition_restore(runner.state(), trusted);
        assert_eq!(
            pt(&restored, target),
            (Some(1), Some(5)),
            "trusted={trusted}"
        );
        assert_sticker_idle(&restored);
        let mut restored = GameRunner::from_state(restored);
        flush_layers_full(&mut restored);
        assert_eq!(pt(restored.state(), target), (Some(1), Some(5)));
        assert_sticker_idle(restored.state());
    }
}

fn add_mana_and_resolve_on(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
    payment: &[ManaType],
) -> Vec<GameEvent> {
    add_mana(runner, 0, payment);
    let events = runner
        .cast(spell)
        .target_object(target)
        .resolve()
        .events()
        .to_vec();
    assert_sticker_idle(runner.state());
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    events
}

/// P2b - CR 123.8 + CR 123.5 + CR 400.7m + CR 613.7d + CR 613.7k: Carnival
/// Carnivore's real ETB puts the 1/5 P/T sticker on battlefield Grist. On the
/// battlefield Grist is only a planeswalker, so the sticker sets nothing; in the
/// graveyard it is a creature card and the retained, renewed sticker wins over
/// Grist's own static after each death, including after a Zombify round trip.
#[test]
fn carnival_sticker_on_battlefield_grist_survives_death_zombify_and_second_death() {
    let db = shared_card_db().expect("production sticker fixture loads");
    let mut scenario = main_phase();
    let grist = scenario.add_real_card(P0, "Grist, the Hunger Tide", Zone::Battlefield, db);
    let carnival = scenario.add_real_card(P0, "Carnival Carnivore", Zone::Hand, db);
    let first_downfall = scenario.add_real_card(P0, "Hero's Downfall", Zone::Hand, db);
    let zombify = scenario.add_real_card(P0, "Zombify", Zone::Hand, db);
    let second_downfall = scenario.add_real_card(P0, "Hero's Downfall", Zone::Hand, db);
    let mut runner = scenario.build();
    sticker_sheet(&mut runner);
    assert_only_a_planeswalker(runner.state(), grist);

    add_mana(
        &mut runner,
        0,
        &[
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner.cast(carnival).commit();
    let (optional, branches, offered) =
        settle_sticker_etb(&mut runner, carnival, Some(grist), true);
    assert_eq!((optional, branches), (1, 1));
    // CR 123.3b: the owned nonland permanents are Carnival itself and Grist.
    assert_eq!(sorted(offered), sorted(vec![carnival, grist]));
    assert_eq!(tickets(runner.state()), 9);
    assert_eq!(runner.state().objects[&grist].stickers.len(), 1);
    assert!(is_hydra_one_five(
        &runner.state().objects[&grist].stickers[0]
    ));
    assert!(runner.state().objects[&carnival].stickers.is_empty());
    // CR 123.8: battlefield Grist is not a creature, so the sticker sets no P/T.
    assert_only_a_planeswalker(runner.state(), grist);

    let death = [ManaType::Black, ManaType::Black, ManaType::Colorless];
    let reanimate = [
        ManaType::Black,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ];
    for (step, spell, payment, destination) in [
        ("first death", first_downfall, &death[..], Zone::Graveyard),
        ("zombify", zombify, &reanimate[..], Zone::Battlefield),
        ("second death", second_downfall, &death[..], Zone::Graveyard),
    ] {
        let previous = runner.state().objects[&grist].stickers[0].clone();
        let incarnation = runner.state().objects[&grist].incarnation;
        let events = add_mana_and_resolve_on(&mut runner, spell, grist, payment);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::StickerPlaced { .. })));
        let object = &runner.state().objects[&grist];
        assert_eq!(object.zone, destination, "{step}");
        assert!(is_hydra_one_five(&object.stickers[0]), "{step}");
        // CR 613.7k: renewed immediately after the new object's timestamp.
        assert_renewed_after_move(runner.state(), grist, &previous, incarnation);
        if destination == Zone::Battlefield {
            assert_only_a_planeswalker(runner.state(), grist);
        } else {
            assert!(runner.state().players[0].graveyard.contains(&grist));
            assert!(object.card_types.core_types.contains(&CoreType::Creature));
            assert_eq!(pt(runner.state(), grist), (Some(1), Some(5)), "{step}");
        }
    }
}

/// CR 613.7d + CR 613.7k: after a real move the single retained sticker keeps
/// its locator and is renewed immediately after the new object's timestamp.
fn assert_renewed_after_move(
    state: &GameState,
    id: ObjectId,
    previous: &AppliedSticker,
    incarnation: u64,
) {
    let object = &state.objects[&id];
    assert!(object.incarnation > incarnation);
    assert_eq!(object.stickers.len(), 1);
    assert_eq!(object.stickers[0].locator(), previous.locator());
    let mut expected = previous.clone();
    expected.set_timestamp(object.stickers[0].timestamp());
    assert_eq!(object.stickers[0], expected, "kind and payload survive");
    assert_eq!(object.stickers[0].timestamp(), object.timestamp + 1);
    assert!(object.stickers[0].timestamp() > previous.timestamp());
}

/// CR 613.7d + CR 613.7k: every retained sticker keeps its payload and is
/// renewed after the new object's timestamp in its prior relative order.
fn assert_mixed_renewed(
    state: &GameState,
    id: ObjectId,
    previous: &[AppliedSticker],
    incarnation: u64,
) {
    let object = &state.objects[&id];
    assert!(object.incarnation > incarnation);
    assert_eq!(previous.len(), 5);
    assert_eq!(object.stickers.len(), previous.len());
    let mut ordered = previous.to_vec();
    ordered.sort_by_key(AppliedSticker::timestamp);
    for (offset, prior) in ordered.iter().enumerate() {
        let current = object
            .stickers
            .iter()
            .find(|sticker| sticker.locator() == prior.locator())
            .expect("the retained sticker is still carried");
        let mut expected = prior.clone();
        expected.set_timestamp(current.timestamp());
        assert_eq!(current, &expected, "locator, kind and full payload survive");
        assert_eq!(current.timestamp(), object.timestamp + offset as u64 + 1);
        assert!(current.timestamp() > prior.timestamp());
    }
}

/// Supplementary staging (labeled): places the registry's cost-2 1/5 P/T
/// sticker through the placement owner. The printed placement routes are the
/// Scampire and Carnival rows above.
fn stage_real_pt_sticker(runner: &mut GameRunner, target: ObjectId) {
    sticker_sheet(runner);
    let candidate = available_sticker_candidates(
        runner.state(),
        P0,
        Some(StickerKind::PowerToughness),
        Some(2),
        false,
    )
    .into_iter()
    .next()
    .expect("the cost-2 P/T sticker exists");
    assert!(is_hydra_one_five(&candidate.sticker));
    apply_selected_sticker(
        runner.state_mut(),
        P0,
        target,
        candidate.sticker,
        candidate.pay_ticket,
        &mut Vec::new(),
    );
    flush_layers_full(runner);
}

/// Supplementary staging (labeled): one registry sticker of every kind plus a
/// second P/T sticker, placed through the placement owner, then stored in
/// reverse timestamp order.
fn stage_mixed_stickers(runner: &mut GameRunner, target: ObjectId) {
    sticker_sheet(runner);
    let candidates = available_sticker_candidates(runner.state(), P0, None, None, false);
    for (kind, index) in [
        (StickerKind::Name, 0),
        (StickerKind::Ability, 6),
        (StickerKind::PowerToughness, 9),
        (StickerKind::Art, 3),
        (StickerKind::PowerToughness, 8),
    ] {
        let candidate = candidates
            .iter()
            .find(|candidate| {
                candidate.sticker.kind() == kind && candidate.sticker.locator().index == index
            })
            .expect("the registry supplies every retained sticker kind")
            .clone();
        let locator = candidate.sticker.locator().clone();
        let mut events = Vec::new();
        apply_selected_sticker(
            runner.state_mut(),
            P0,
            target,
            candidate.sticker,
            candidate.pay_ticket,
            &mut events,
        );
        assert_eq!(
            runner.state().objects[&target]
                .stickers
                .last()
                .unwrap()
                .locator(),
            &locator
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::StickerPlaced { object_id, .. } if *object_id == target))
                .count(),
            1
        );
    }
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .stickers
        .reverse();
    flush_layers_full(runner);
}

/// P1d - CR 608.2c + CR 123.3b: the {TK} instruction always completes; the
/// optional placement is declined, has no sticker sheet, or has no recipient.
#[test]
fn scampire_real_etb_decline_no_sheet_and_empty_graveyard_complete() {
    for (with_target, sheets, accept) in [
        (true, true, false),
        (true, false, true),
        (false, true, true),
    ] {
        let (mut runner, source, target, _, _) = scampire_scenario(with_target, sheets);
        let (_, branches, _) = settle_sticker_etb(&mut runner, source, target, accept);
        assert_eq!(branches, 0);
        assert_eq!(
            tickets(runner.state()),
            11,
            "{with_target} {sheets} {accept}"
        );
        assert!(runner
            .state()
            .objects
            .values()
            .all(|object| object.stickers.is_empty()));
    }
}

/// P2 - CR 123.8 + CR 613.4b: Carnival Carnivore's real ETB puts the chosen
/// P/T sticker on a battlefield creature.
#[test]
fn carnival_real_etb_places_battlefield_pt_sticker() {
    let db = shared_card_db().expect("production sticker fixture loads");
    let mut scenario = main_phase();
    let source = scenario.add_real_card(P0, "Carnival Carnivore", Zone::Hand, db);
    let target = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    sticker_sheet(&mut runner);
    runner.cast(source).commit();
    let (optional, branches, offered) = settle_sticker_etb(&mut runner, source, Some(target), true);
    assert_eq!((optional, branches), (1, 1));
    // Carnival itself is an owned nonland permanent too.
    assert_eq!(sorted(offered), sorted(vec![source, target]));
    assert!(is_hydra_one_five(
        &runner.state().objects[&target].stickers[0]
    ));
    assert_eq!(pt(runner.state(), target), (Some(1), Some(5)));
    assert_eq!(tickets(runner.state()), 9);
}

/// T1 - CR 123.5 + CR 400.7m + CR 613.7k: real public moves keep and renew
/// Grist's retained sticker; Raise Dead's hidden destination discards it.
#[test]
fn real_public_spells_renew_grist_stickers_and_raise_dead_discards_them() {
    let db = shared_card_db().expect("production sticker fixture loads");
    for (spell_name, origin, destination) in [
        ("Hero's Downfall", Zone::Battlefield, Zone::Graveyard),
        ("Cremate", Zone::Graveyard, Zone::Exile),
        ("Zombify", Zone::Graveyard, Zone::Battlefield),
        ("Raise Dead", Zone::Graveyard, Zone::Hand),
    ] {
        let mut scenario = main_phase();
        let grist = scenario.add_real_card(P0, "Grist, the Hunger Tide", origin, db);
        let spell = scenario.add_real_card(P0, spell_name, Zone::Hand, db);
        scenario.with_library_top(P0, &["Draw fodder"]);
        scenario.with_mana_pool(
            P0,
            mana(&[
                ManaType::Black,
                ManaType::Black,
                ManaType::Colorless,
                ManaType::Colorless,
            ]),
        );
        let mut runner = scenario.build();
        stage_real_pt_sticker(&mut runner, grist);
        let previous = runner.state().objects[&grist].stickers[0].clone();
        let incarnation = runner.state().objects[&grist].incarnation;
        let library_before = runner.state().players[0].library.len();
        add_mana_and_resolve_on(&mut runner, spell, grist, &[]);
        if spell_name == "Cremate" {
            assert_eq!(runner.state().players[0].library.len(), library_before - 1);
            assert!(runner.state().players[0]
                .hand
                .iter()
                .any(|id| runner.state().objects[id].name == "Draw fodder"));
        }
        let object = &runner.state().objects[&grist];
        assert_eq!(object.zone, destination, "{spell_name}");
        if destination == Zone::Hand {
            assert!(object.stickers.is_empty());
            assert_eq!(
                pt(runner.state(), grist),
                (Some(1), Some(1)),
                "Grist's static still functions in hand after the sticker is lost"
            );
        } else {
            assert_renewed_after_move(runner.state(), grist, &previous, incarnation);
            if destination == Zone::Battlefield {
                assert_only_a_planeswalker(runner.state(), grist);
            } else {
                assert_eq!(
                    pt(runner.state(), grist),
                    (Some(1), Some(5)),
                    "{spell_name}"
                );
            }
        }
    }
}

/// T3 - CR 903.8 + CR 903.9a + CR 112.1 + CR 613.7d + CR 613.7k: a commander
/// Grist's retained stickers are renewed through the shared Stack/Command
/// timestamp owner on its cast, resolution, return to the command zone and
/// taxed recast; an unrelated carrier is never renewed.
#[test]
fn real_commander_cast_death_choice_and_recast_share_retained_timestamp_owner() {
    let db = shared_card_db().expect("production sticker fixture loads");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 0x9968);
    scenario.at_phase(Phase::PreCombatMain);
    let grist = scenario.add_real_card(P0, "Grist, the Hunger Tide", Zone::Hand, db);
    scenario.with_commander(grist);
    let downfall = scenario.add_real_card(P1, "Hero's Downfall", Zone::Hand, db);
    let unrelated = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    scenario.with_mana_pool(
        P0,
        mana(&[ManaType::Black, ManaType::Green, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    assert!(runner.state().command_zone.contains(&grist));
    stage_mixed_stickers(&mut runner, grist);
    let mut remote_sticker = runner.state().objects[&grist].stickers[0].clone();
    remote_sticker.set_timestamp(runner.state_mut().next_timestamp());
    runner
        .state_mut()
        .objects
        .get_mut(&unrelated)
        .unwrap()
        .stickers
        .push(remote_sticker);
    let unrelated_stickers = runner.state().objects[&unrelated].stickers.clone();
    let unrelated_stamp = runner.state().objects[&unrelated].timestamp;
    let assert_unrelated = |state: &GameState| {
        assert_eq!(state.objects[&unrelated].stickers, unrelated_stickers);
        assert_eq!(state.objects[&unrelated].timestamp, unrelated_stamp);
    };

    let previous = runner.state().objects[&grist].stickers.clone();
    let incarnation = runner.state().objects[&grist].incarnation;
    runner.cast(grist).commit();
    assert!(runner.state().stack.iter().any(
        |entry| entry.source_id == grist && matches!(entry.kind, StackEntryKind::Spell { .. })
    ));
    assert_eq!(
        runner.state().objects[&grist].cast_from_zone,
        Some(Zone::Command)
    );
    assert_eq!(runner.state().commander_cast_count.get(&grist), Some(&1));
    assert_eq!(runner.state().players[0].mana_pool.units().count(), 0);
    assert!(!runner.state().command_zone.contains(&grist));
    assert_mixed_renewed(runner.state(), grist, &previous, incarnation);
    assert_unrelated(runner.state());
    // CR 112.1 + CR 123.8: a creature spell; the newest P/T sticker wins.
    assert_eq!(pt(runner.state(), grist), (Some(1), Some(5)));

    let previous = runner.state().objects[&grist].stickers.clone();
    let incarnation = runner.state().objects[&grist].incarnation;
    runner.pass_both_players();
    assert_sticker_idle(runner.state());
    assert_eq!(runner.state().objects[&grist].zone, Zone::Battlefield);
    assert_mixed_renewed(runner.state(), grist, &previous, incarnation);
    assert_unrelated(runner.state());
    assert_eq!(pt(runner.state(), grist), (None, None));

    add_mana(
        &mut runner,
        1,
        &[ManaType::Black, ManaType::Black, ManaType::Colorless],
    );
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    let previous = runner.state().objects[&grist].stickers.clone();
    let incarnation = runner.state().objects[&grist].incarnation;
    runner.cast(downfall).target_object(grist).resolve();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CommanderZoneChoice { player: P0, commander_id, current_zone: Zone::Graveyard } if commander_id == grist),
        "{:?}",
        runner.state().waiting_for
    );
    assert_mixed_renewed(runner.state(), grist, &previous, incarnation);
    assert_unrelated(runner.state());

    let previous = runner.state().objects[&grist].stickers.clone();
    let incarnation = runner.state().objects[&grist].incarnation;
    let mut declined = GameRunner::from_state(runner.state().clone());
    declined
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("owner declines the command-zone move");
    assert_eq!(declined.state().objects[&grist].zone, Zone::Graveyard);
    assert!(!declined.state().command_zone.contains(&grist));
    assert_eq!(declined.state().objects[&grist].stickers, previous);
    assert_eq!(pt(declined.state(), grist), (Some(1), Some(5)));
    assert_sticker_idle(declined.state());
    assert_unrelated(declined.state());

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("owner accepts the command-zone move");
    assert!(runner.state().command_zone.contains(&grist));
    assert_mixed_renewed(runner.state(), grist, &previous, incarnation);
    assert_eq!(pt(runner.state(), grist), (Some(1), Some(5)));
    assert_sticker_idle(runner.state());
    assert_unrelated(runner.state());

    // CR 903.8: the recast pays the commander tax.
    add_mana(
        &mut runner,
        0,
        &[
            ManaType::Black,
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    let previous = runner.state().objects[&grist].stickers.clone();
    let incarnation = runner.state().objects[&grist].incarnation;
    runner.cast(grist).commit();
    assert_eq!(runner.state().commander_cast_count.get(&grist), Some(&2));
    assert_eq!(runner.state().players[0].mana_pool.units().count(), 0);
    assert!(!runner.state().command_zone.contains(&grist));
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == grist));
    assert_eq!(
        runner.state().objects[&grist].cast_from_zone,
        Some(Zone::Command)
    );
    assert_mixed_renewed(runner.state(), grist, &previous, incarnation);
    assert_eq!(pt(runner.state(), grist), (Some(1), Some(5)));
    assert_unrelated(runner.state());

    let previous = runner.state().objects[&grist].stickers.clone();
    let incarnation = runner.state().objects[&grist].incarnation;
    runner.pass_both_players();
    assert_sticker_idle(runner.state());
    assert_eq!(runner.state().objects[&grist].zone, Zone::Battlefield);
    assert_mixed_renewed(runner.state(), grist, &previous, incarnation);
    assert_eq!(pt(runner.state(), grist), (None, None));
    assert_unrelated(runner.state());
}

/// Same Stack spell occurrence with an actual Spell entry, written inline
/// because the Phase 3 helpers also assert the pre-sticker contract.
fn assert_same_stack_spell(state: &GameState, id: ObjectId, occurrence: ObjectIncarnationRef) {
    let object = &state.objects[&id];
    assert_eq!(object.zone, Zone::Stack);
    assert_eq!(ObjectIncarnationRef::from_object(object), occurrence);
    assert!(state.stack.iter().any(|entry| entry.id == id
        && entry.source_id == id
        && matches!(entry.kind, StackEntryKind::Spell { card_id, .. } if card_id == object.card_id)));
}

/// P9 - CR 718.3b + CR 123.8 + CR 112.1 + CR 611.2c + CR 701.6a: a prototype
/// spell cast with Cavern's Wizard mana uses its prototype P/T as the baseline
/// beneath a newer P/T sticker, keeps the cast-bound prohibition, and returns
/// to the prototype 2/1 once it stops being a creature.
#[test]
fn real_prototype_spell_uses_prototype_pt_baseline_under_newer_sticker() {
    let db = shared_card_db().expect("prototype sticker row requires CardDB");
    let (mut runner, _, proxy, counterspell, extra) = cast_prohibition_paid_proxy_with(
        true,
        &[
            (P0, "Painter's Servant", Zone::Exile),
            (P0, "Arcane Proxy", Zone::Exile),
        ],
    );
    let (painter, normal) = (extra[0], extra[1]);
    let occurrence = ObjectIncarnationRef::from_object(&runner.state().objects[&proxy]);
    // CR 718.4: the normal card elsewhere has its normal characteristics.
    assert_eq!(pt(runner.state(), normal), (Some(4), Some(3)));

    // Fixture staging of Painter's chosen colour; Painter's own enters-choice
    // is not under test.
    move_to_zone(
        runner.state_mut(),
        painter,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    runner
        .state_mut()
        .objects
        .get_mut(&painter)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Green));
    assert_eq!(runner.state().objects[&painter].zone, Zone::Battlefield);
    // One settled Painter pass before the sticker exists.
    flush_layers_full(&mut runner);
    cast_prohibition_assert_spell(runner.state(), proxy, occurrence);
    assert_eq!(pt(runner.state(), proxy), (Some(2), Some(1)));
    assert!(runner.state().objects[&proxy]
        .color
        .contains(&ManaColor::Blue));
    assert!(runner.state().objects[&proxy]
        .color
        .contains(&ManaColor::Green));
    assert_eq!(cast_prohibition_count(runner.state(), proxy), 1);
    assert!(
        runner.state().attribution[&proxy]
            .by_layer
            .contains_key(&Layer::Color),
        "reach guard: Painter's colour effect reached the Stack Proxy"
    );

    // Registry-derived Stack sticker fixture: the only printed Stack placer
    // (sticker kicker, Wicker Picker) is Unimplemented; apply_selected_sticker's
    // off-zone rebuild is not exercised on a Stack spell. No ticket payment or
    // StickerPlaced event is claimed.
    sticker_sheet(&mut runner);
    let candidate = available_sticker_candidates(
        runner.state(),
        P0,
        Some(StickerKind::PowerToughness),
        Some(2),
        false,
    )
    .into_iter()
    .next()
    .expect("the cost-2 P/T sticker exists");
    assert!(is_hydra_one_five(&candidate.sticker));
    let mut sticker = candidate.sticker;
    sticker.set_timestamp(runner.state_mut().next_timestamp());
    runner
        .state_mut()
        .objects
        .get_mut(&proxy)
        .unwrap()
        .stickers
        .push(sticker);
    runner.state_mut().layers_dirty.mark_full();
    for pass in 0..3 {
        flush_layers_full(&mut runner);
        assert_same_stack_spell(runner.state(), proxy, occurrence);
        assert_eq!(pt(runner.state(), proxy), (Some(1), Some(5)), "pass {pass}");
        let color = &runner.state().objects[&proxy].color;
        assert!(color.contains(&ManaColor::Blue) && color.contains(&ManaColor::Green));
        assert_eq!(cast_prohibition_count(runner.state(), proxy), 1);
    }

    for trusted in [false, true] {
        let persisted = if trusted {
            PersistedGameState::capture(runner.state().clone())
        } else {
            PersistedGameState::Raw(Box::new(runner.state().clone()))
        };
        let restored =
            serde_json::from_value::<PersistedGameState>(serde_json::to_value(persisted).unwrap())
                .unwrap()
                .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
                .unwrap()
                .finalize_after_rehydration(|state| {
                    rehydrate_game_from_card_db_with_finalization(
                        state,
                        db,
                        CardDbRehydrationFinalization::Defer,
                    );
                    assert!(!state.attribution[&proxy].by_layer.is_empty());
                    // Painter's effect ends before the first restored settle.
                    move_to_zone(state, painter, Zone::Graveyard, &mut Vec::new());
                    state.layers_dirty.mark_full();
                    Ok(())
                })
                .unwrap();
        assert_same_stack_spell(&restored, proxy, occurrence);
        assert_eq!(
            pt(&restored, proxy),
            (Some(1), Some(5)),
            "trusted={trusted}"
        );
        assert_eq!(restored.objects[&proxy].color, vec![ManaColor::Blue]);
        assert_eq!(cast_prohibition_count(&restored, proxy), 1);
        let mut countered = GameRunner::from_state(restored.clone());

        // Labeled type-loss fixture: the spell stops being a creature.
        let mut restored = GameRunner::from_state(restored);
        restored
            .state_mut()
            .objects
            .get_mut(&proxy)
            .unwrap()
            .card_types
            .core_types
            .retain(|core_type| *core_type != CoreType::Creature);
        flush_layers_full(&mut restored);
        assert_eq!(
            pt(restored.state(), proxy),
            (Some(2), Some(1)),
            "the prototype 2/1, not the normal 4/3 or the sticker's 1/5"
        );
        assert_eq!(cast_prohibition_count(restored.state(), proxy), 1);

        let events = cast_prohibition_counter(&mut countered, proxy, counterspell);
        assert_same_stack_spell(countered.state(), proxy, occurrence);
        assert_eq!(cast_prohibition_count(countered.state(), proxy), 1);
        assert!(!events.iter().any(
            |event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == proxy)
        ));
    }
    assert_eq!(pt(runner.state(), normal), (Some(4), Some(3)));
}

/// P9f - CR 702.102b + CR 709.4d: expiring a genuine remote colour effect
/// restores both fused halves' combined characteristics, live and after restore.
#[test]
fn real_fused_spell_restores_combined_characteristics_after_painter_expiry_and_restore() {
    let db = shared_card_db().expect("fused row requires CardDB");
    let mut scenario = main_phase();
    let painter = scenario.add_real_card(P0, "Painter's Servant", Zone::Battlefield, db);
    let breaking = scenario.add_real_card(P0, "Breaking", Zone::Hand, db);
    let creature = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Blue,
            ManaType::Black,
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&painter)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Green));
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .cast(breaking)
        .casting_variant(CastingVariant::Fuse)
        .target_player(P1)
        .target_object(creature)
        .commit();
    for _ in 0..2 {
        flush_layers_full(&mut runner);
    }
    let spell = &runner.state().objects[&breaking];
    assert!(spell.fused_split_spell);
    assert!(runner.state().stack.iter().any(|e| e.source_id == breaking));
    assert_eq!(spell.effective_mana_value(), 8);
    for color in [
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ] {
        assert!(spell.color.contains(&color));
    }
    assert!(!runner.state().attribution[&breaking].by_layer.is_empty());
    let painted = runner.state().clone();
    for trusted in [false, true] {
        let saved = if trusted {
            PersistedGameState::capture(painted.clone())
        } else {
            PersistedGameState::Raw(Box::new(painted.clone()))
        };
        let restored =
            serde_json::from_value::<PersistedGameState>(serde_json::to_value(saved).unwrap())
                .unwrap()
                .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
                .unwrap()
                .finalize_after_rehydration(|state| {
                    assert!(!state.attribution[&breaking].by_layer.is_empty());
                    rehydrate_game_from_card_db_with_finalization(
                        state,
                        db,
                        CardDbRehydrationFinalization::Defer,
                    );
                    move_to_zone(state, painter, Zone::Graveyard, &mut Vec::new());
                    state.layers_dirty.mark_full();
                    Ok(())
                })
                .unwrap();
        let spell = &restored.objects[&breaking];
        assert!(spell.fused_split_spell);
        assert_eq!(spell.zone, Zone::Stack);
        assert!(restored.stack.iter().any(|e| e.source_id == breaking));
        assert_eq!(spell.effective_mana_value(), 8);
        assert_eq!(spell.color.len(), 3);
        for color in [ManaColor::Blue, ManaColor::Black, ManaColor::Red] {
            assert!(spell.color.contains(&color));
        }
    }
    let painter_incarnation = runner.state().objects[&painter].incarnation;
    move_to_zone(
        runner.state_mut(),
        painter,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    flush_layers_full(&mut runner);
    assert!(runner.state().objects[&painter].incarnation > painter_incarnation);
    let spell = &runner.state().objects[&breaking];
    assert_eq!(spell.effective_mana_value(), 8);
    assert_eq!(spell.color.len(), 3);
    for color in [ManaColor::Blue, ManaColor::Black, ManaColor::Red] {
        assert!(spell.color.contains(&color));
    }
}
