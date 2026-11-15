//! CR733 P2 coverage for exact single-object zone-transition commands.

use crate::support::shared_card_db;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::stickers::{available_sticker_candidates, set_player_sticker_sheets};
use engine::game::zones::{apply_resolved_zone_change, move_to_zone};
use engine::types::game_state::{
    GameState, PersistedGameState, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedRulesJournal, ResolvedZoneChangeCommand,
    ResolvedZoneChangeReplayInvariantError, RetainedStickerTimestamp,
};
use engine::types::stickers::{AppliedSticker, StickerKind, StickerLocator};
use engine::types::zones::Zone;

/// A labeled mutation of a recorded zone-change command.
type StructuralCase = (&'static str, fn(&mut ResolvedZoneChangeCommand));
/// A labeled mutation of the live prestate a command replays against.
type PrestateCase = (&'static str, fn(&mut GameState, ObjectId));

fn recorded_zone_change(
    state: &GameState,
    from: Zone,
    to: Zone,
) -> engine::types::resolved_commands::ResolvedZoneChangeCommand {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .find_map(|command| match command {
            ResolvedRulesCommand::ZoneChange(command)
                if command.from == from && command.to == to =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .expect("ordinary move must journal the expected zone-change command")
}

fn battlefield_reentry_states() -> (GameState, GameState) {
    let mut scenario = GameScenario::new_n_player(2, 0x733);
    let creature = scenario
        .add_creature_from_oracle(P0, "Journal Entrant", 2, 2, "")
        .id();
    let runner = scenario.build();
    let mut state = runner.state().clone();
    let mut events = Vec::new();
    move_to_zone(&mut state, creature, Zone::Graveyard, &mut events);
    let pre_state = state.clone();
    move_to_zone(&mut state, creature, Zone::Battlefield, &mut events);
    (pre_state, state)
}

#[test]
fn zone_change_command_round_trips_and_replays_the_exact_transition_core() {
    let (pre_state, ordinary_state) = battlefield_reentry_states();
    let command = recorded_zone_change(&ordinary_state, Zone::Graveyard, Zone::Battlefield);

    let wire = serde_json::to_value(ResolvedRulesCommand::ZoneChange(Box::new(command.clone())))
        .expect("zone-change command serializes");
    assert_eq!(
        serde_json::from_value::<ResolvedRulesCommand>(wire)
            .expect("zone-change command deserializes"),
        ResolvedRulesCommand::ZoneChange(Box::new(command.clone()))
    );

    let journal_wire = serde_json::to_value(&ordinary_state.resolved_rules_journal)
        .expect("journal serializes with a zone-change command");
    assert_eq!(
        serde_json::from_value::<ResolvedRulesJournal>(journal_wire)
            .expect("journal validates and deserializes"),
        ordinary_state.resolved_rules_journal
    );

    let mut replay = pre_state;
    apply_resolved_zone_change(&mut replay, &command).expect("exact transition replays");

    let object = &replay.objects[&command.object.object_id];
    assert_eq!(object.zone, command.to);
    assert_eq!(object.incarnation, command.resulting_incarnation);
    assert_eq!(object.timestamp, command.entry_timestamp.unwrap());
    assert_eq!(
        replay.battlefield[command.destination_position], command.object.object_id,
        "replay installs the recorded destination position"
    );
    assert_eq!(
        replay.zone_changes_this_turn[command.turn_zone_change_index], command.zone_change_record,
        "replay appends the exact recorded per-turn zone-change entry"
    );

    // CR 613.7: installing the recorded entry timestamp is only half the
    // contract — the allocator must also be carried past it. `next_timestamp`
    // is the draw counter, so an applier that installs a value while leaving
    // the counter behind hands that same timestamp to a second object later in
    // the replay, and CR 613.7 orders effects within a layer by timestamp
    // alone. Asserted by DRAWING, so this pins the consequence, not the field.
    let installed = command.entry_timestamp.expect("a battlefield entry draws");
    let next_drawn = replay.next_timestamp();
    assert!(
        next_drawn > installed,
        "CR 613.7d: replay installed entry timestamp {installed} but the next draw handed \
         out {next_drawn}; two objects sharing a timestamp are unordered within their layer"
    );
}

/// The other side of the CR 613.7d allocator contract: a move to a zone that
/// grants no timestamp records none, so replaying it must advance no allocator.
/// This is what keeps the advance bound to a recorded draw rather than applied
/// blanket to every zone change, which would silently inflate the counter.
#[test]
fn a_zone_change_that_drew_no_timestamp_advances_no_allocator_on_replay() {
    let mut scenario = GameScenario::new_n_player(2, 0x733);
    let creature = scenario
        .add_creature_from_oracle(P0, "Journal Entrant", 2, 2, "")
        .id();
    let runner = scenario.build();

    // The predecessor is the state the graveyard move was recorded FROM, so the
    // command's own incarnation and occurrence guards are satisfied on replay.
    let pre_state = runner.state().clone();
    let mut state = pre_state.clone();
    let mut events = Vec::new();
    move_to_zone(&mut state, creature, Zone::Graveyard, &mut events);

    let to_graveyard = recorded_zone_change(&state, Zone::Battlefield, Zone::Graveyard);
    assert_eq!(
        to_graveyard.entry_timestamp, None,
        "CR 613.7d grants a timestamp on entering the battlefield; a graveyard move draws none"
    );

    let mut replay = pre_state;
    let before = replay.next_timestamp;
    apply_resolved_zone_change(&mut replay, &to_graveyard)
        .expect("the recorded graveyard move replays against the state it was recorded from");
    assert_eq!(
        replay.objects[&creature].zone,
        Zone::Graveyard,
        "reach guard: the replay actually performed the move being asserted about"
    );
    assert_eq!(
        replay.next_timestamp, before,
        "a zone change that drew no timestamp must advance no allocator on replay"
    );
}

#[test]
fn zone_change_rejects_occurrence_and_destination_position_mismatches() {
    let (pre_state, ordinary_state) = battlefield_reentry_states();
    let command = recorded_zone_change(&ordinary_state, Zone::Graveyard, Zone::Battlefield);

    let mut replay = pre_state.clone();
    apply_resolved_zone_change(&mut replay, &command).expect("first exact application succeeds");
    assert!(matches!(
        apply_resolved_zone_change(&mut replay, &command),
        Err(ResolvedZoneChangeReplayInvariantError::OccurrenceMismatch { .. })
    ));

    let mut wrong_position = command.clone();
    wrong_position.destination_position += 1;
    assert!(matches!(
        apply_resolved_zone_change(&mut pre_state.clone(), &wrong_position),
        Err(ResolvedZoneChangeReplayInvariantError::DestinationPositionMismatch { .. })
    ));
}

#[test]
fn zone_change_replay_validates_the_nested_records_turn_number() {
    let (pre_state, ordinary_state) = battlefield_reentry_states();
    let command = recorded_zone_change(&ordinary_state, Zone::Graveyard, Zone::Battlefield);

    let mut replay = pre_state.clone();
    apply_resolved_zone_change(&mut replay, &command)
        .expect("an exact nested record replays successfully");

    let mut tampered = command;
    tampered.zone_change_record.recorded_turn_number += 1;
    assert!(matches!(
        apply_resolved_zone_change(&mut pre_state.clone(), &tampered),
        Err(ResolvedZoneChangeReplayInvariantError::RecordedTurnMismatch { .. })
    ));
}

#[test]
fn zone_change_journal_rejects_an_unrelated_cause() {
    let (_, ordinary_state) = battlefield_reentry_states();
    let mut wire = serde_json::to_value(&ordinary_state.resolved_rules_journal)
        .expect("journal serializes before malformed-cause test");
    let entries = wire["entries"]
        .as_array_mut()
        .expect("journal stores command entries");
    let command = entries
        .iter_mut()
        .find_map(|entry| entry["command"]["ZoneChange"].as_object_mut())
        .expect("journal contains a zone-change command");
    command.insert("cause".to_string(), serde_json::json!({ "Proposal": 99 }));

    assert!(serde_json::from_value::<ResolvedRulesJournal>(wire).is_err());
}

#[test]
fn production_change_zone_pipeline_records_the_transition_command() {
    let mut scenario = GameScenario::new_n_player(2, 0x733);
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature_from_oracle(P0, "Target Creature", 2, 2, "")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Exile Test", true, "Exile target creature.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(spell).target_object(target).resolve();

    let command = recorded_zone_change(runner.state(), Zone::Battlefield, Zone::Exile);
    assert_eq!(command.object.object_id, target);
    assert_eq!(
        command.resulting_incarnation,
        command.object.incarnation + 1
    );
    assert_eq!(runner.state().objects[&target].zone, Zone::Exile);
}

const TREASURE_CRUISE_ORACLE: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\nDraw three cards.";

/// CR 733.1 + CR 601.2h: a cancelled delve cast journals no zone change for the
/// selected cards, so replaying every journaled command from the pre-cast state
/// reproduces the live turn ledger and zones, including a later real move.
#[test]
fn cancelled_delve_cast_leaves_a_replayable_zone_change_journal() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::CastPaymentMode;
    use engine::types::mana::ManaType;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Delve Cruise", false, TREASURE_CRUISE_ORACLE)
        .from_oracle_text_with_keywords(&["Delve"], TREASURE_CRUISE_ORACLE)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let fuel: Vec<_> = ["Fuel A", "Fuel B", "Fuel C"]
        .into_iter()
        .map(|name| scenario.add_spell_to_graveyard(P0, name, true).id())
        .collect();
    let later = scenario.add_spell_to_hand(P0, "Later Card", true).id();
    let mut runner = scenario.build();
    let pre_cast = runner.state().clone();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin the delve cast");
    for id in &fuel {
        runner
            .act(GameAction::TapForConvoke {
                object_id: *id,
                mana_type: ManaType::Colorless,
            })
            .expect("select delve fuel");
    }
    runner.act(GameAction::CancelCast).expect("cancel the cast");
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), later, Zone::Graveyard, &mut events);

    let live = runner.state();
    let mut replay = pre_cast;
    for command in live
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
    {
        if let ResolvedRulesCommand::ZoneChange(command) = command {
            apply_resolved_zone_change(&mut replay, command)
                .expect("every journaled zone change replays in order");
        }
    }

    assert_eq!(replay.zone_changes_this_turn, live.zone_changes_this_turn);
    for id in fuel.iter().chain([&spell, &later]) {
        assert_eq!(replay.objects[id].zone, live.objects[id].zone, "{id:?}");
        assert_eq!(replay.objects[id].incarnation, live.objects[id].incarnation);
    }
}

/// A battlefield creature carrying registry stickers of all four kinds (two P/T),
/// stored in reverse timestamp order, before and after one real move to the
/// graveyard.
fn retained_transition() -> (GameState, GameState, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 0x733);
    let id = scenario
        .add_creature(P0, "Journal Sticker Carrier", 2, 2)
        .id();
    let mut state = scenario.build().state().clone();
    set_player_sticker_sheets(&mut state, P0, &["Playable Delusionary Hydra".into()]);
    let candidates = available_sticker_candidates(&state, P0, None, None, true);
    let mut stickers = Vec::new();
    for (kind, index) in [
        (StickerKind::Name, 0),
        (StickerKind::Ability, 6),
        (StickerKind::PowerToughness, 8),
        (StickerKind::Art, 3),
        (StickerKind::PowerToughness, 9),
    ] {
        let mut sticker = candidates
            .iter()
            .find(|c| c.sticker.kind() == kind && c.sticker.locator().index == index)
            .expect("registry supplies the exact discrete sticker")
            .sticker
            .clone();
        sticker.set_timestamp(state.next_timestamp());
        stickers.push(sticker);
    }
    stickers.reverse();
    state.objects.get_mut(&id).unwrap().stickers = stickers;
    let before = state.clone();
    move_to_zone(&mut state, id, Zone::Graveyard, &mut Vec::new());
    flush_layers(&mut state);
    (before, state, id)
}

fn retained_rows(command: &ResolvedZoneChangeCommand) -> &[RetainedStickerTimestamp] {
    command
        .retained_sticker_timestamps
        .as_deref()
        .expect("a stickered public move records retained rows")
}

/// Applies `command` to a clone of `prestate`, requiring a refusal that leaves
/// the full serialized state and the allocator untouched.
fn assert_inert_refusal(
    prestate: &GameState,
    command: &ResolvedZoneChangeCommand,
    label: &str,
) -> ResolvedZoneChangeReplayInvariantError {
    let mut replay = prestate.clone();
    let next = replay.next_timestamp;
    let unchanged = serde_json::to_value(&replay).unwrap();
    let error = apply_resolved_zone_change(&mut replay, command).expect_err(label);
    assert_eq!(
        serde_json::to_value(&replay).unwrap(),
        unchanged,
        "{label} mutated state"
    );
    assert_eq!(replay.next_timestamp, next, "{label} moved the allocator");
    error
}

/// J1 - CR 613.7d + CR 613.7k: the recorded command carries the object stamp
/// and one renewed stamp per retained sticker in prior order; replay installs
/// exactly those stamps by locator and only adopts the allocator past them.
#[test]
fn retained_sticker_transition_replays_exact_locator_stamps_and_allocator() {
    let (before, live, id) = retained_transition();
    let command = recorded_zone_change(&live, Zone::Battlefield, Zone::Graveyard);
    let object_stamp = command.entry_timestamp.expect("renewal records the scalar");
    assert_eq!(object_stamp, before.next_timestamp);
    assert_eq!(live.objects[&id].timestamp, object_stamp);
    let rows = retained_rows(&command).to_vec();
    assert_eq!(rows.len(), 5);
    let mut prior: Vec<_> = before.objects[&id]
        .stickers
        .iter()
        .map(|sticker| (sticker.timestamp(), sticker.locator().clone()))
        .collect();
    prior.sort_by_key(|(timestamp, _)| *timestamp);
    for (offset, row) in rows.iter().enumerate() {
        assert_eq!(
            (row.prior_timestamp, &row.locator),
            (prior[offset].0, &prior[offset].1)
        );
        assert_eq!(row.timestamp, object_stamp + 1 + offset as u64);
        assert_eq!(
            live.objects[&id]
                .stickers
                .iter()
                .find(|sticker| sticker.locator() == &row.locator)
                .unwrap()
                .timestamp(),
            row.timestamp
        );
    }
    assert_eq!(live.next_timestamp, object_stamp + 6);
    // CR 123.8: the newest P/T sticker (the 4/4) wins.
    assert_eq!(
        (live.objects[&id].power, live.objects[&id].toughness),
        (Some(4), Some(4))
    );
    let wire = serde_json::to_value(&live.resolved_rules_journal).unwrap();
    assert_eq!(
        serde_json::from_value::<ResolvedRulesJournal>(wire).unwrap(),
        live.resolved_rules_journal
    );
    let command = serde_json::from_value::<ResolvedZoneChangeCommand>(
        serde_json::to_value(&command).unwrap(),
    )
    .unwrap();
    for higher_allocator in [false, true] {
        let mut replay = before.clone();
        // Binding is by locator, never by vector position.
        replay.objects.get_mut(&id).unwrap().stickers.reverse();
        if higher_allocator {
            replay.next_timestamp = live.next_timestamp + 100;
        }
        let expected_next = replay
            .next_timestamp
            .max(rows.last().unwrap().timestamp + 1);
        apply_resolved_zone_change(&mut replay, &command).unwrap();
        flush_layers(&mut replay);
        let (replayed, original) = (&replay.objects[&id], &live.objects[&id]);
        assert_eq!(replayed.zone, Zone::Graveyard);
        assert_eq!(replayed.timestamp, original.timestamp);
        assert_eq!(replayed.incarnation, original.incarnation);
        for sticker in &original.stickers {
            assert!(replayed.stickers.contains(sticker), "{sticker:?}");
        }
        assert_eq!(replayed.stickers.len(), original.stickers.len());
        assert_eq!((replayed.power, replayed.toughness), (Some(4), Some(4)));
        assert_eq!(replay.zone_changes_this_turn, live.zone_changes_this_turn);
        assert_eq!(replay.next_timestamp(), expected_next);
    }
}

/// J2 + J3 - CR 613.7k: malformed retained operands are refused by both the
/// journal decoder and the direct applier, and a valid command is refused
/// against a mismatched live prestate, all before any mutation.
#[test]
fn retained_sticker_metadata_rejects_malformed_journals_and_live_operands_before_mutation() {
    let (before, live, id) = retained_transition();
    let valid = recorded_zone_change(&live, Zone::Battlefield, Zone::Graveyard);
    let valid_wire = serde_json::to_value(&live.resolved_rules_journal).unwrap();
    assert!(serde_json::from_value::<ResolvedRulesJournal>(valid_wire.clone()).is_ok());
    valid
        .validate_timestamp_operands()
        .expect("positive operand reaches the shared structural validator");
    apply_resolved_zone_change(&mut before.clone(), &valid)
        .expect("the valid command replays against its prestate");
    let structural: [StructuralCase; 11] = [
        ("duplicate locator", |c| {
            let rows = c.retained_sticker_timestamps.as_mut().unwrap();
            rows[1].locator = rows[0].locator.clone();
        }),
        ("duplicate installed stamp", |c| {
            let rows = c.retained_sticker_timestamps.as_mut().unwrap();
            rows[1].timestamp = rows[0].timestamp;
        }),
        ("decreasing installed stamp", |c| {
            let rows = c.retained_sticker_timestamps.as_mut().unwrap();
            let first = rows[0].timestamp;
            rows[0].timestamp = rows[1].timestamp;
            rows[1].timestamp = first;
        }),
        ("decreasing prior order", |c| {
            c.retained_sticker_timestamps.as_mut().unwrap()[1].prior_timestamp = 0;
        }),
        ("swapped rows", |c| {
            c.retained_sticker_timestamps.as_mut().unwrap().swap(0, 1);
        }),
        ("installed not after the scalar", |c| {
            c.retained_sticker_timestamps.as_mut().unwrap()[0].timestamp =
                c.entry_timestamp.unwrap();
        }),
        ("empty rows", |c| {
            c.retained_sticker_timestamps = Some(vec![])
        }),
        ("rows without a scalar", |c| c.entry_timestamp = None),
        ("rows on a hidden move", |c| c.to = Zone::Hand),
        ("rows on a same-zone move", |c| c.to = c.from),
        ("scalar on a hidden move", |c| {
            c.retained_sticker_timestamps = None;
            c.to = Zone::Hand;
        }),
    ];
    for (label, mutate) in structural {
        let mut command = valid.clone();
        mutate(&mut command);
        assert!(command.validate_timestamp_operands().is_err(), "{label}");
        let mut wire = valid_wire.clone();
        let field = wire["entries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find_map(|entry| entry["command"].get_mut("ZoneChange"))
            .unwrap();
        *field = serde_json::to_value(&command).unwrap();
        assert!(
            serde_json::from_value::<ResolvedRulesJournal>(wire).is_err(),
            "journal decode accepted {label}"
        );
        assert_inert_refusal(&before, &command, label);
    }
    // Structurally loadable commands that omit required rows.
    for (label, scalar) in [("rows omitted", true), ("rows and scalar omitted", false)] {
        let mut command = valid.clone();
        command.retained_sticker_timestamps = None;
        if !scalar {
            command.entry_timestamp = None;
        }
        command
            .validate_timestamp_operands()
            .expect("structurally loadable");
        assert!(matches!(
            assert_inert_refusal(&before, &command, label),
            ResolvedZoneChangeReplayInvariantError::RetainedStickerMetadataMismatch {
                recorded: None
            }
        ));
    }
    // Live prestate mismatches against the valid command.
    let prestate: [PrestateCase; 6] = [
        ("missing sticker", |s, id| {
            s.objects.get_mut(&id).unwrap().stickers.pop();
        }),
        ("extra sticker", |s, id| {
            let obj = s.objects.get_mut(&id).unwrap();
            let mut extra = obj.stickers[0].clone();
            if let AppliedSticker::PowerToughness { locator, .. } = &mut extra {
                locator.index = 99;
            }
            obj.stickers.push(extra);
        }),
        ("altered prior stamp", |s, id| {
            s.objects.get_mut(&id).unwrap().stickers[0].set_timestamp(999);
        }),
        ("stale occurrence", |s, id| {
            s.objects.get_mut(&id).unwrap().incarnation += 1;
        }),
        ("wrong locator", |s, id| {
            if let AppliedSticker::PowerToughness { locator, .. } =
                &mut s.objects.get_mut(&id).unwrap().stickers[0]
            {
                locator.index = 99;
            }
        }),
        ("duplicate and missing", |s, id| {
            let obj = s.objects.get_mut(&id).unwrap();
            obj.stickers[0] = obj.stickers[1].clone();
        }),
    ];
    for (label, mutate) in prestate {
        let mut mutated = before.clone();
        mutate(&mut mutated, id);
        assert_ne!(
            serde_json::to_value(&mutated.objects[&id]).unwrap(),
            serde_json::to_value(&before.objects[&id]).unwrap(),
            "{label}: the mutated dimension differs"
        );
        assert_inert_refusal(&mutated, &valid, label);
    }
    // CR 613.7k: a same-zone reorder records no scalar; one added afterwards is
    // refused structurally and by the applier.
    let mut reordered = live.clone();
    move_to_zone(&mut reordered, id, Zone::Graveyard, &mut Vec::new());
    let mut bad_same_zone = recorded_zone_change(&reordered, Zone::Graveyard, Zone::Graveyard);
    assert!(bad_same_zone.entry_timestamp.is_none());
    assert!(bad_same_zone.retained_sticker_timestamps.is_none());
    bad_same_zone.entry_timestamp = Some(live.next_timestamp);
    assert!(bad_same_zone.validate_timestamp_operands().is_err());
    let mut wire = serde_json::to_value(&reordered.resolved_rules_journal).unwrap();
    let entry = wire["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .rev()
        .find_map(|entry| entry["command"].get_mut("ZoneChange"))
        .unwrap();
    *entry = serde_json::to_value(&bad_same_zone).unwrap();
    assert!(serde_json::from_value::<ResolvedRulesJournal>(wire).is_err());
    assert!(matches!(
        assert_inert_refusal(&live, &bad_same_zone, "same-zone scalar"),
        ResolvedZoneChangeReplayInvariantError::UnexpectedNonbattlefieldTimestamp
    ));
}

/// J4 - CR 613.7k: a legacy command without retained rows still loads (alone,
/// in a journal and in a Raw/Trusted state); only a replay that requires the
/// rows is refused, inertly.
#[test]
fn legacy_omitted_retained_metadata_loads_but_only_required_replay_is_refused() {
    let (before, live, _) = retained_transition();
    let command = recorded_zone_change(&live, Zone::Battlefield, Zone::Graveyard);
    let mut wire = serde_json::to_value(&command).unwrap();
    wire.as_object_mut()
        .unwrap()
        .remove("retained_sticker_timestamps");
    let legacy: ResolvedZoneChangeCommand = serde_json::from_value(wire).unwrap();
    assert!(legacy.retained_sticker_timestamps.is_none());
    assert!(legacy.entry_timestamp.is_some());
    assert!(matches!(
        assert_inert_refusal(&before, &legacy, "legacy required replay"),
        ResolvedZoneChangeReplayInvariantError::RetainedStickerMetadataMismatch { recorded: None }
    ));
    let strip = |journal: &mut serde_json::Value| {
        for entry in journal["entries"].as_array_mut().unwrap() {
            if let Some(command) = entry["command"].get_mut("ZoneChange") {
                command
                    .as_object_mut()
                    .unwrap()
                    .remove("retained_sticker_timestamps");
            }
        }
    };
    let mut journal = serde_json::to_value(&live.resolved_rules_journal).unwrap();
    strip(&mut journal);
    serde_json::from_value::<ResolvedRulesJournal>(journal)
        .expect("a journal without the optional rows loads");
    for trusted in [false, true] {
        let persisted = if trusted {
            PersistedGameState::capture(live.clone())
        } else {
            PersistedGameState::Raw(Box::new(live.clone()))
        };
        let mut wire = serde_json::to_value(persisted).unwrap();
        let game = if trusted {
            &mut wire["state"]
        } else {
            &mut wire
        };
        strip(&mut game["resolved_rules_journal"]);
        let mut restored = serde_json::from_value::<PersistedGameState>(wire)
            .expect("legacy metadata omission does not refuse the whole state")
            .prepare_for_restore(PersistedRestoreFinalization::Immediate)
            .unwrap()
            .finalize_immediately()
            .unwrap();
        move_to_zone(
            &mut restored,
            command.object.object_id,
            Zone::Exile,
            &mut Vec::new(),
        );
        assert_eq!(
            restored.objects[&command.object.object_id].zone,
            Zone::Exile,
            "the loaded state continues an ordinary public move"
        );
    }
    // The same shape against an unstickered prestate is an unexpected scalar.
    let mut unstickered = before.clone();
    unstickered
        .objects
        .get_mut(&command.object.object_id)
        .unwrap()
        .stickers
        .clear();
    assert!(matches!(
        assert_inert_refusal(&unstickered, &legacy, "unstickered scalar"),
        ResolvedZoneChangeReplayInvariantError::UnexpectedNonbattlefieldTimestamp
    ));
    let mut no_scalar = legacy.clone();
    no_scalar.entry_timestamp = None;
    apply_resolved_zone_change(&mut unstickered, &no_scalar)
        .expect("a legacy unstickered move without a scalar still replays");
    let (before, live) = battlefield_reentry_states();
    let command = recorded_zone_change(&live, Zone::Graveyard, Zone::Battlefield);
    assert!(command.retained_sticker_timestamps.is_none());
    apply_resolved_zone_change(&mut before.clone(), &command)
        .expect("a legacy unstickered battlefield entry still replays");
}

/// T4 - CR 123.5 + CR 613.7k: an exact hidden replay discards every sticker and
/// its characteristic changes; a same-zone reorder renews nothing.
#[test]
fn hidden_destination_replay_clears_stickers_and_same_zone_reorder_draws_none() {
    let (_, mut state, id) = retained_transition();
    let timestamp = state.next_timestamp;
    let stickers = state.objects[&id].stickers.clone();
    let object_stamp = state.objects[&id].timestamp;
    move_to_zone(&mut state, id, Zone::Graveyard, &mut Vec::new());
    assert_eq!(state.next_timestamp, timestamp);
    assert_eq!(state.objects[&id].stickers, stickers);
    assert_eq!(state.objects[&id].timestamp, object_stamp);
    let before = state.clone();
    assert_ne!(
        before.objects[&id].name, "Journal Sticker Carrier",
        "reach guard: the name sticker changed the retained name"
    );
    move_to_zone(&mut state, id, Zone::Hand, &mut Vec::new());
    let command = recorded_zone_change(&state, Zone::Graveyard, Zone::Hand);
    assert!(command.entry_timestamp.is_none());
    assert!(command.retained_sticker_timestamps.is_none());
    let mut replay = before;
    apply_resolved_zone_change(&mut replay, &command).unwrap();
    let object = &replay.objects[&id];
    assert_eq!(object.zone, Zone::Hand);
    assert!(object.stickers.is_empty());
    assert_eq!(object.name, "Journal Sticker Carrier");
    assert_eq!(object.abilities.len(), object.base_abilities.len());
    assert_eq!((object.power, object.toughness), (Some(2), Some(2)));
    assert_eq!(replay.next_timestamp, timestamp);
}

/// J1 (real card) - CR 123.5 + CR 613.7k: a real Cremate records the retained
/// rows of its Graveyard -> Exile move, and replaying that transition core
/// reproduces the exact stamps and allocator.
#[test]
fn real_cremate_records_retained_metadata_that_replays_the_exact_transition() {
    let db = shared_card_db().expect("production fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_real_card(P0, "Grist, the Hunger Tide", Zone::Graveyard, db);
    let spell = scenario.add_real_card(P0, "Cremate", Zone::Hand, db);
    scenario.with_library_top(P0, &["Draw fodder"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    // Supplementary staging: a registry P/T sticker on the graveyard card.
    let timestamp = runner.state_mut().next_timestamp();
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .stickers
        .push(AppliedSticker::PowerToughness {
            locator: StickerLocator {
                sheet: "Playable Delusionary Hydra".into(),
                index: 8,
            },
            ticket_cost: 2,
            power: 1,
            toughness: 5,
            timestamp,
        });
    runner.state_mut().layers_dirty.mark_full();
    flush_layers(runner.state_mut());
    runner.cast(spell).target_object(target).commit();
    let before = runner.state().clone();
    runner.pass_both_players();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    let live = runner.state();
    assert_eq!(live.objects[&target].zone, Zone::Exile);
    assert_eq!(live.objects[&spell].zone, Zone::Graveyard);
    let command = recorded_zone_change(live, Zone::Graveyard, Zone::Exile);
    assert_eq!(command.object.object_id, target);
    let rows = retained_rows(&command);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].prior_timestamp, timestamp);
    assert_eq!(rows[0].timestamp, command.entry_timestamp.unwrap() + 1);
    assert_eq!(
        (live.objects[&target].power, live.objects[&target].toughness),
        (Some(1), Some(5))
    );
    let mut replay = before;
    apply_resolved_zone_change(&mut replay, &command).unwrap();
    flush_layers(&mut replay);
    assert_eq!(
        (
            replay.objects[&target].power,
            replay.objects[&target].toughness
        ),
        (Some(1), Some(5))
    );
    assert_eq!(
        replay.objects[&target].stickers,
        live.objects[&target].stickers
    );
    assert_eq!(
        replay.objects[&target].timestamp,
        live.objects[&target].timestamp
    );
    assert_eq!(
        replay.objects[&target].incarnation,
        command.resulting_incarnation
    );
    assert_eq!(
        replay.zone_changes_this_turn[command.turn_zone_change_index],
        command.zone_change_record
    );
    assert_eq!(replay.next_timestamp(), rows[0].timestamp + 1);
}
