//! Game log privacy, naming and dedupe across the cast, activation and elimination pipelines.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::game_object::{AttachTarget, BackFaceData};
use engine::game::log::resolve_log_entries;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_events_for_viewer, filter_state_for_viewer};
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{
    ActionResult, AutoPassRequest, ExileLinkKind, GameState, LookGrant, TurnBoundary, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::log::{GameLogEntry, LogSegment, LogVisibility};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const SPECTATOR: PlayerId = PlayerId(u8::MAX);
const EXILE_HAND_THEN_DRAW: &str = "Exile all cards from target player's hand, then that player draws a card. That player loses 1 life.";

fn names(entry: &GameLogEntry, id: ObjectId) -> bool {
    entry.segments.iter().any(
        |segment| matches!(segment, LogSegment::CardName { object_id, .. } if *object_id == id),
    )
}

fn entries_naming(entries: &[GameLogEntry], id: ObjectId) -> Vec<&GameLogEntry> {
    entries.iter().filter(|entry| names(entry, id)).collect()
}

fn is_move_line(entry: &GameLogEntry, id: ObjectId, from: Zone, to: Zone) -> bool {
    matches!(
        entry.segments.as_slice(),
        [
            LogSegment::CardName { object_id, .. },
            LogSegment::Text(moves),
            LogSegment::Zone(logged_from),
            LogSegment::Text(_),
            LogSegment::Zone(logged_to),
        ] if *object_id == id && moves == " moves from " && *logged_from == from && *logged_to == to
    )
}

fn has_elimination_line(entries: &[GameLogEntry], player: PlayerId) -> bool {
    entries.iter().any(|entry| {
        matches!(
            entry.segments.as_slice(),
            [LogSegment::PlayerName { player_id, .. }, LogSegment::Text(text)]
                if *player_id == player && text == " is eliminated"
        )
    })
}

fn has_resolution_line(entries: &[GameLogEntry], spell: ObjectId) -> bool {
    entries.iter().any(|entry| {
        matches!(
            entry.segments.as_slice(),
            [LogSegment::CardName { object_id, .. }, LogSegment::Text(text)]
                if *object_id == spell && text == "'s effect resolves"
        )
    })
}

/// The moves of `id` whose record names it `name`.
fn named_moves(events: &[GameEvent], id: ObjectId, name: &str) -> Vec<(Zone, Zone)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(from),
                to,
                record,
            } if *object_id == id && record.name == name => Some((*from, *to)),
            _ => None,
        })
        .collect()
}

/// The moves of `id` named `name` that `viewer` receives on the raw event channel.
fn viewer_named_moves(
    events: &[GameEvent],
    state: &GameState,
    viewer: PlayerId,
    id: ObjectId,
    name: &str,
) -> Vec<(Zone, Zone)> {
    named_moves(&filter_events_for_viewer(events, state, viewer), id, name)
}

fn event_position(result: &ActionResult, predicate: impl Fn(&GameEvent) -> bool) -> Option<usize> {
    result.events.iter().position(predicate)
}

fn move_position(result: &ActionResult, id: ObjectId, from: Zone, to: Zone) -> Option<usize> {
    event_position(result, |event| {
        matches!(event, GameEvent::ZoneChanged { object_id, from: Some(f), to: t, .. }
            if *object_id == id && *f == from && *t == to)
    })
}

fn eliminated_position(result: &ActionResult, player: PlayerId) -> Option<usize> {
    event_position(
        result,
        |event| matches!(event, GameEvent::PlayerEliminated { player_id } if *player_id == player),
    )
}

fn turn_started_position(result: &ActionResult) -> Option<usize> {
    event_position(result, |event| {
        matches!(event, GameEvent::TurnStarted { .. })
    })
}

fn pass_until_elimination(runner: &mut GameRunner, mut result: ActionResult) -> ActionResult {
    for _ in 0..6 {
        if eliminated_position(&result, P2).is_some() {
            return result;
        }
        result = runner.act(GameAction::PassPriority).unwrap();
    }
    panic!("no elimination batch; waiting for {:?}", result.waiting_for);
}

fn arm_auto_pass_and_reach_end_step(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).unwrap();
    for player in [P1, P2] {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: holder } if holder == player
        ));
        runner
            .act(GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilTurnBoundary {
                    until: TurnBoundary::MyNextTurnStart,
                },
            })
            .unwrap();
    }
    while runner.state().phase != Phase::End {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
}

/// CR 400.2 + CR 701.17c: a card leaving the library face up into a public zone is public.
#[test]
fn library_departures_into_public_zones_are_logged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let milled = scenario.add_card_to_library_top(P0, "Probe Milled");
    let mill = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe Mill", false, "Mill a card.")
        .id();
    let mut runner = scenario.build();
    let before = runner.state().clone();
    let outcome = runner.cast(mill).resolve();
    outcome.assert_zone(&[milled], Zone::Graveyard);
    let entries = resolve_log_entries(outcome.events(), &before, outcome.state());
    let milled_entries = entries_naming(&entries, milled);
    assert_eq!(milled_entries.len(), 1, "{entries:?}");
    assert!(is_move_line(
        milled_entries[0],
        milled,
        Zone::Library,
        Zone::Graveyard
    ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let found = scenario.add_card_to_library_top(P0, "Probe Found");
    let search = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe Search",
            false,
            "Search your library for a card, put it onto the battlefield, then shuffle.",
        )
        .id();
    let mut runner = scenario.build();
    let before = runner.state().clone();
    let outcome = runner.cast(search).search_first_legal().resolve();
    outcome.assert_zone(&[found], Zone::Battlefield);
    let entries = resolve_log_entries(outcome.events(), &before, outcome.state());
    assert!(entries.iter().any(|entry| is_move_line(
        entry,
        found,
        Zone::Library,
        Zone::Battlefield
    )));
}

/// CR 406.3 + CR 400.2: face-down exile and a draw keep the card's identity out of the public log.
#[test]
fn face_down_exile_and_draw_stay_unnamed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hidden = scenario.add_card_to_library_top(P0, "Probe Hidden Top");
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe Hideaway",
            false,
            "Exile the top card of your library face down.",
        )
        .id();
    let mut runner = scenario.build();
    let before = runner.state().clone();
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[hidden], Zone::Exile);
    assert!(outcome.state().objects[&hidden].face_down);
    let entries = resolve_log_entries(outcome.events(), &before, outcome.state());
    assert!(has_resolution_line(&entries, spell), "{entries:?}");
    assert!(entries_naming(&entries, hidden).is_empty(), "{entries:?}");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drawn = scenario.add_card_to_library_top(P0, "Probe Drawn");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe Draw", false, "Draw a card.")
        .id();
    let mut runner = scenario.build();
    let before = runner.state().clone();
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[drawn], Zone::Hand);
    let entries = resolve_log_entries(outcome.events(), &before, outcome.state());
    assert!(has_resolution_line(&entries, spell), "{entries:?}");
    assert!(entries
        .iter()
        .filter(|entry| entry.presentation.visibility == LogVisibility::Public)
        .all(|entry| !names(entry, drawn)));
}

const DISCOVER: &str = "Look at the top five cards of your library. Exile one of them face down and put the rest on the bottom of your library in a random order. You may cast the exiled card without paying its mana cost if it's an instant spell with mana value 2 or less. If you don't, put that card into your hand.";

/// CR 406.3: a card leaving face-down exile for a hidden zone is not revealed.
#[test]
fn discover_decline_keeps_the_card_unnamed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Probe Deck {index}"));
    }
    let found = scenario.add_card_to_library_top(P0, "Probe Discovered");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe Discover", false, DISCOVER)
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(spell).commit();
    for _ in [P0, P1] {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let chosen = runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .unwrap();
    assert!(move_position(&chosen, found, Zone::Library, Zone::Exile).is_some());

    let declined = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();

    assert!(move_position(&declined, found, Zone::Exile, Zone::Hand).is_some());
    assert_eq!(runner.state().objects[&found].zone, Zone::Hand);
    let exile_to_hand = vec![(Zone::Exile, Zone::Hand)];
    assert_eq!(
        named_moves(&declined.events, found, "Probe Discovered"),
        exile_to_hand
    );
    for viewer in [P1, SPECTATOR] {
        assert!(viewer_named_moves(
            &declined.events,
            runner.state(),
            viewer,
            found,
            "Probe Discovered"
        )
        .is_empty());
    }
    assert_eq!(
        viewer_named_moves(
            &declined.events,
            runner.state(),
            P0,
            found,
            "Probe Discovered"
        ),
        exile_to_hand
    );
    assert!(declined.log_entries.iter().any(|entry| matches!(
        entry.segments.as_slice(),
        [LogSegment::CardName { object_id, .. }, LogSegment::Text(text)]
            if *object_id == spell && text == "'s effect resolves"
    )));
    assert!(
        entries_naming(&declined.log_entries, found).is_empty(),
        "{:?}",
        declined.log_entries
    );
}

/// CR 400.2 + CR 406.3: cards exiled face up from the library are public.
#[test]
fn face_up_library_exile_stays_named() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let second = scenario.add_card_to_library_top(P0, "Probe Second");
    let top = scenario.add_card_to_library_top(P0, "Probe Top");
    let spell = scenario
        .add_spell_to_hand(P0, "Probe Light Up", false)
        .from_oracle_text_with_keywords(
            &["Spectacle"],
            "Spectacle {R} (You may cast this spell for its spectacle cost rather than its mana cost if an opponent lost life this turn.)\nExile the top two cards of your library. Until the end of your next turn, you may play those cards.",
        )
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(spell).commit();
    let mut resolved = None;
    for _ in 0..4 {
        let result = runner.act(GameAction::PassPriority).unwrap();
        if move_position(&result, top, Zone::Library, Zone::Exile).is_some() {
            resolved = Some(result);
            break;
        }
    }
    let resolved = resolved.expect("no resolution batch");
    let state = runner.state();
    let entries = &resolved.log_entries;
    for id in [top, second] {
        assert_eq!(state.objects[&id].zone, Zone::Exile);
        assert!(!state.objects[&id].face_down);
        let naming = entries_naming(entries, id);
        assert_eq!(naming.len(), 1, "{entries:?}");
        assert!(is_move_line(naming[0], id, Zone::Library, Zone::Exile));
    }
}

/// CR 800.4a: the leaving player's hidden cards leave the game without being revealed.
#[test]
fn eliminated_players_hidden_cards_stay_unnamed() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let hand = scenario.add_card_to_hand(P2, "Probe Hand");
    let library = scenario.add_card_to_library_top(P2, "Probe Library");
    let face_down = scenario
        .add_creature_to_exile(P2, "Probe Face Down", 2, 2)
        .id();
    let graveyard = scenario
        .add_creature_to_graveyard(P2, "Probe Graveyard", 2, 2)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&face_down)
        .unwrap()
        .face_down = true;

    let result = runner.act(GameAction::Concede { player_id: P2 }).unwrap();

    for id in [hand, library] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    let entries = &result.log_entries;
    assert!(entries.iter().any(|entry| is_move_line(
        entry,
        graveyard,
        Zone::Graveyard,
        Zone::Exile
    )));
    for id in [hand, library, face_down] {
        assert!(entries_naming(entries, id).is_empty(), "{entries:?}");
    }
    assert!(entries.iter().all(|entry| {
        let zones: Vec<_> = entry
            .segments
            .iter()
            .filter_map(|segment| match segment {
                LogSegment::Zone(zone) => Some(zone),
                _ => None,
            })
            .collect();
        !(zones.len() == 2 && zones[0] == zones[1])
    }));
    assert!(has_elimination_line(entries, P2));
}

/// CR 800.4a: only the sweep's moves are hidden; the leaver's own face-up exile earlier in the
/// same batch stays public.
#[test]
fn leavers_same_batch_face_up_exile_is_logged() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P2, 1);
    let kept = scenario.add_card_to_hand(P2, "Probe Kept");
    let deep = scenario.add_card_to_library_top(P2, "Probe Deep");
    let drawn = scenario.add_card_to_library_top(P2, "Probe Drawn");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe Exile Hand", false, EXILE_HAND_THEN_DRAW)
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(spell).target_player(P2).commit();
    let first = runner.act(GameAction::PassPriority).unwrap();
    let result = pass_until_elimination(&mut runner, first);

    assert!(move_position(&result, kept, Zone::Hand, Zone::Exile).is_some());
    assert!(move_position(&result, drawn, Zone::Hand, Zone::Exile).is_some());
    assert!(move_position(&result, deep, Zone::Library, Zone::Exile).is_some());
    assert_eq!(turn_started_position(&result), None);

    let entries = &result.log_entries;
    let kept_entries = entries_naming(entries, kept);
    assert_eq!(kept_entries.len(), 1, "{entries:?}");
    assert!(is_move_line(kept_entries[0], kept, Zone::Hand, Zone::Exile));
    assert!(entries_naming(entries, drawn).is_empty(), "{entries:?}");
    assert!(entries_naming(entries, deep).is_empty(), "{entries:?}");
    assert!(has_elimination_line(entries, P2));
}

/// CR 800.4a + CR 800.4j: the sweep stays hidden when the same action carries play into the
/// next turn.
#[test]
fn turn_crossing_elimination_keeps_hidden_cards_unnamed() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let hand = scenario.add_card_to_hand(P0, "Probe Hand");
    let library = scenario.add_card_to_library_top(P0, "Probe Library");
    let mut runner = scenario.build();
    arm_auto_pass_and_reach_end_step(&mut runner);

    let result = runner.act(GameAction::Concede { player_id: P0 }).unwrap();

    assert!(move_position(&result, hand, Zone::Hand, Zone::Exile).is_some());
    assert!(move_position(&result, library, Zone::Library, Zone::Exile).is_some());
    let eliminated = eliminated_position(&result, P0).expect("P0 eliminated in this batch");
    assert!(turn_started_position(&result).is_some_and(|turn_start| eliminated < turn_start));

    let entries = &result.log_entries;
    assert!(entries_naming(entries, hand).is_empty(), "{entries:?}");
    assert!(entries_naming(entries, library).is_empty(), "{entries:?}");
    assert!(has_elimination_line(entries, P0));
}

/// Pins a known limit: in a turn segment the batch leaves through a turn start, the leaver's own
/// face-up exile before their elimination is hidden too, because the journal that tells it apart
/// from the sweep is cleared at turn start and one batch may cross it. This assertion is expected
/// to fail once the log is resolved per turn segment.
#[test]
fn turn_crossing_elimination_hides_leavers_face_up_exile() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P2, 1);
    let kept = scenario.add_card_to_hand(P2, "Probe Kept");
    scenario.add_card_to_library_top(P2, "Probe Deep");
    scenario.add_card_to_library_top(P2, "Probe Drawn");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe Exile Hand", true, EXILE_HAND_THEN_DRAW)
        .id();
    let mut runner = scenario.build();
    arm_auto_pass_and_reach_end_step(&mut runner);
    let _ = runner.cast(spell).target_player(P2).commit();
    let armed = runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        })
        .unwrap();
    let result = pass_until_elimination(&mut runner, armed);

    let kept_move = move_position(&result, kept, Zone::Hand, Zone::Exile).expect("kept exiled");
    let eliminated = eliminated_position(&result, P2).unwrap();
    let turn_start = turn_started_position(&result).expect("batch crosses a turn start");
    assert!(kept_move < eliminated && eliminated < turn_start);

    assert!(
        has_elimination_line(&result.log_entries, P2),
        "{:?}",
        result.log_entries
    );
    assert!(
        entries_naming(&result.log_entries, kept).is_empty(),
        "{:?}",
        result.log_entries
    );
}

fn adventure_face() -> BackFaceData {
    BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Probe Adventure Face".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: {
            let mut card_type = CardType::default();
            card_type.core_types.push(CoreType::Instant);
            card_type.subtypes.push("Adventure".to_string());
            card_type
        },
        mana_cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        },
        keywords: Vec::new(),
        abilities: vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        )],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Red],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        layout_kind: None,
        parse_warnings: vec![],
    }
}

/// CR 400.7 + CR 715.4: lines about the Adventure spell name the face it had on the stack, not
/// the creature it becomes in exile.
#[test]
fn adventure_resolution_names_the_adventure_face() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario
        .add_creature_to_hand(P0, "Probe Creature Face", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, Vec::new()); 2],
    );
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&card).unwrap().back_face = Some(adventure_face());
    let before = runner.state().clone();
    let outcome = runner
        .cast(card)
        .adventure_face(false)
        .target_player(P1)
        .resolve();
    outcome.assert_zone(&[card], Zone::Exile);
    let entries = resolve_log_entries(outcome.events(), &before, outcome.state());
    let card_name = |entry: &GameLogEntry| match entry.segments.first() {
        Some(LogSegment::CardName { name, object_id }) if *object_id == card => Some(name.clone()),
        _ => None,
    };
    let text_after_card = |entry: &GameLogEntry, expected: &str| matches!(entry.segments.get(1), Some(LogSegment::Text(text)) if text == expected);

    let damage = entries
        .iter()
        .find(|entry| text_after_card(entry, " deals "))
        .and_then(card_name);
    let effect = entries
        .iter()
        .find(|entry| text_after_card(entry, "'s effect resolves"))
        .and_then(card_name);
    let exile = entries
        .iter()
        .find(|entry| is_move_line(entry, card, Zone::Stack, Zone::Exile))
        .and_then(card_name);
    assert_eq!(
        damage.as_deref(),
        Some("Probe Adventure Face"),
        "{entries:?}"
    );
    assert_eq!(
        effect.as_deref(),
        Some("Probe Adventure Face"),
        "{entries:?}"
    );
    assert_eq!(exile.as_deref(), Some("Probe Creature Face"), "{entries:?}");
}

#[test]
fn tagged_activation_logs_one_line() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Probe Bear", 2, 2).id();
    let sword = scenario
        .add_artifact_from_oracle(P0, "Probe Sword", "Equip {0}")
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    let before = runner.state().clone();
    let outcome = runner.activate(sword, 0).target_object(bear).resolve();

    assert_eq!(
        outcome.state().objects[&sword].attached_to,
        Some(AttachTarget::Object(bear))
    );
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == sword)));
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::KeywordAbilityActivated { source_id, .. } if *source_id == sword
    )));

    let entries = resolve_log_entries(outcome.events(), &before, outcome.state());
    let activation_lines = |label: &str| {
        entries
            .iter()
            .filter(|entry| {
                names(entry, sword)
                    && entry
                        .segments
                        .iter()
                        .any(|segment| matches!(segment, LogSegment::Text(text) if text == label))
            })
            .count()
    };
    assert_eq!(activation_lines(" activates equip: "), 1, "{entries:?}");
    assert_eq!(activation_lines(" activates ability: "), 0, "{entries:?}");
}

const BROODLORD: &str = "Convoke\nFlying\nWhen this creature enters, search your library for a card, exile it face down, then shuffle. For as long as that card remains exiled, you may play it.\nSpells you cast from exile have convoke.";
const WORD_OF_SEIZING: &str = "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and gain control of it until end of turn. It gains haste until end of turn.";
const AVACYN: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Search your library for a card, exile it face down, then shuffle.\nII — Turn the exiled card face up. If it's a creature card, you lose life equal to its mana value.\nIII — You may put the exiled card onto the battlefield if it's a creature card. If you don't put it onto the battlefield, put it into its owner's hand.";

fn view_name(state: &GameState, viewer: PlayerId, id: ObjectId) -> String {
    filter_state_for_viewer(state, viewer).objects[&id]
        .name
        .clone()
}

fn left_library_for_exile(events: &[GameEvent], id: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Exile, .. } if *object_id == id)
    })
}

fn shuffled_library(events: &[GameEvent], player: PlayerId) -> bool {
    events.iter().any(|event| {
        matches!(event, GameEvent::PlayerPerformedAction { player_id, action: PlayerActionKind::ShuffledLibrary, .. } if *player_id == player)
    })
}

fn fill_libraries(scenario: &mut GameScenario, players: &[PlayerId]) {
    for &player in players {
        for i in 0..6 {
            scenario.add_card_to_library_top(player, &format!("Probe Filler {i}"));
        }
    }
}

fn add_free_instant(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    keywords: &[&str],
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand(player, name, true)
        .from_oracle_text_with_keywords(keywords, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// Passes priority until `player` holds it.
fn pass_priority_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..8 {
        if runner.state().priority_player == player
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().priority_player, player);
}

/// Passes priority until `player` holds it, then casts `spell` targeting `target` and resolves it.
fn cast_on(runner: &mut GameRunner, player: PlayerId, spell: ObjectId, target: ObjectId) {
    pass_priority_to(runner, player);
    runner.cast(spell).target_objects(&[target]).resolve();
}

/// Resolves Hoarding Broodlord's search for `found` with Word of Seizing in P1's hand; returns
/// `found`, the Broodlord, Word of Seizing and the runner.
fn broodlord_search_exiles_found() -> (ObjectId, ObjectId, ObjectId, GameRunner) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fill_libraries(&mut scenario, &[P0, P1]);
    let found = scenario.add_card_to_library_top(P0, "Probe Found");
    let lord = scenario
        .add_creature_to_hand(P0, "Hoarding Broodlord", 4, 4)
        .from_oracle_text_with_keywords(&["Convoke", "Flying"], BROODLORD)
        .id();
    let seize = add_free_instant(
        &mut scenario,
        P1,
        "Word of Seizing",
        &["Split second"],
        WORD_OF_SEIZING,
    );
    let mut runner = scenario.build();
    let before = runner.state().clone();
    let outcome = runner.cast(lord).search_first_legal().resolve();
    assert!(left_library_for_exile(outcome.events(), found));
    // CR 701.24a: the search shuffles the searched library.
    assert!(shuffled_library(outcome.events(), P0));
    let entries = resolve_log_entries(outcome.events(), &before, outcome.state());
    assert!(entries_naming(&entries, found).is_empty(), "{entries:?}");
    (found, lord, seize, runner)
}

/// CR 406.3: a card searched for and exiled face down is hidden from the other players.
#[test]
fn search_exile_face_down_hides_the_card_from_opponents() {
    let (found, _, _, runner) = broodlord_search_exiles_found();
    let state = runner.state();
    assert!(state.objects[&found].face_down);
    assert_eq!(view_name(state, P0, found), "Probe Found");
    assert!(spell_objects_available_to_cast(state, P0).contains(&found));
    assert_ne!(view_name(state, P1, found), "Probe Found");
}

/// CR 406.3: gaining control of the searching permanent does not grant a look at the card it
/// exiled face down.
#[test]
fn search_exile_look_is_not_gained_by_taking_the_searching_permanent() {
    let (found, lord, seize, mut runner) = broodlord_search_exiles_found();
    cast_on(&mut runner, P1, seize, lord);
    let state = runner.state();
    assert_eq!(state.objects[&lord].controller, P1);
    assert_eq!(view_name(state, P0, found), "Probe Found");
    assert_eq!(view_name(state, P1, found), "Hidden Card");
    assert!(state.exile_links.iter().any(|link| link.exiled_id == found
        && matches!(
            link.kind,
            ExileLinkKind::HideawayLookable {
                grant: LookGrant::Player { player: P0 },
                ..
            }
        )));
}

/// CR 406.3: the searcher, not the owner, keeps the look at a card searched out of another
/// player's library and exiled face down.
#[test]
fn foreign_search_exile_look_is_bound_to_the_searcher() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fill_libraries(&mut scenario, &[P0, P1]);
    let found = scenario.add_card_to_library_top(P1, "Probe Foreign Found");
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe Grasp",
            false,
            "Search target opponent's library for a card and exile it face down. Then that player shuffles. You may play that card for as long as it remains exiled.",
        )
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(spell).target_player(P1).commit();
    for _ in [P0, P1] {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let chosen = runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .unwrap();
    let state = runner.state();
    let events = &chosen.events;
    assert!(left_library_for_exile(events, found));
    // CR 701.24a: only the searched library is shuffled.
    assert!(shuffled_library(events, P1));
    assert!(!shuffled_library(events, P0));
    // CR 406.3 + CR 608.2c: the searcher, not the owner, is the player the look is bound to.
    assert!(state.exile_links.iter().any(|link| link.exiled_id == found
        && matches!(
            link.kind,
            ExileLinkKind::HideawayLookable {
                grant: LookGrant::Player { player: P0 },
                ..
            }
        )));
    assert_eq!(view_name(state, P0, found), "Probe Foreign Found");
    assert!(spell_objects_available_to_cast(state, P0).contains(&found));
    assert_ne!(view_name(state, P1, found), "Probe Foreign Found");
    let entries = &chosen.log_entries;
    assert!(has_resolution_line(entries, spell), "{entries:?}");
    assert!(entries_naming(entries, found).is_empty(), "{entries:?}");
}

/// Resolves The Creation of Avacyn's chapter I search for `found`, with Word of Seizing in P1's
/// hand and Disenchant in P0's; returns the runner, `found`, the Saga, Word of Seizing and
/// Disenchant.
fn avacyn_chapter_one() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fill_libraries(&mut scenario, &[P0, P1]);
    let found = scenario.add_card_to_library_top(P0, "Probe Found");
    let saga = scenario
        .add_spell_to_hand(P0, "The Creation of Avacyn", false)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(AVACYN)
        .id();
    let seize = add_free_instant(
        &mut scenario,
        P1,
        "Word of Seizing",
        &["Split second"],
        WORD_OF_SEIZING,
    );
    let disenchant = add_free_instant(
        &mut scenario,
        P0,
        "Disenchant",
        &[],
        "Destroy target artifact or enchantment.",
    );
    let mut runner = scenario.build();
    let before = runner.state().clone();
    let outcome = runner.cast(saga).search_first_legal().resolve();
    let state = outcome.state();
    assert!(left_library_for_exile(outcome.events(), found));
    // CR 701.24a: the search shuffles the searched library.
    assert!(shuffled_library(outcome.events(), P0));
    assert!(state.objects[&found].face_down);
    assert_eq!(view_name(state, P0, found), "Probe Found");
    assert_ne!(view_name(state, P1, found), "Probe Found");
    let entries = resolve_log_entries(outcome.events(), &before, state);
    assert!(entries_naming(&entries, found).is_empty(), "{entries:?}");
    (runner, found, saga, seize, disenchant)
}

/// CR 406.3: the searcher keeps the look after the Saga that exiled the card leaves the
/// battlefield.
#[test]
fn saga_search_exile_look_survives_the_saga_leaving() {
    let (mut runner, found, saga, _, disenchant) = avacyn_chapter_one();
    cast_on(&mut runner, P0, disenchant, saga);
    let state = runner.state();
    assert_eq!(state.objects[&saga].zone, Zone::Graveyard);
    assert_eq!(state.objects[&found].zone, Zone::Exile);
    assert!(state.objects[&found].face_down);
    assert_eq!(view_name(state, P0, found), "Probe Found");
    assert_eq!(view_name(state, P1, found), "Hidden Card");
}

/// CR 406.3 + CR 613.1b: gaining control of the Saga does not let a player look at the card its
/// chapter I exiled face down, even after the Saga leaves the battlefield.
#[test]
fn saga_search_exile_look_is_not_gained_by_taking_the_saga() {
    let (mut runner, found, saga, seize, disenchant) = avacyn_chapter_one();
    cast_on(&mut runner, P1, seize, saga);
    assert_eq!(runner.state().objects[&saga].controller, P1);
    cast_on(&mut runner, P0, disenchant, saga);
    let state = runner.state();
    assert_eq!(state.objects[&saga].zone, Zone::Graveyard);
    assert_eq!(state.objects[&found].zone, Zone::Exile);
    assert!(state.objects[&found].face_down);
    assert_eq!(view_name(state, P1, found), "Hidden Card");
    assert_eq!(view_name(state, P0, found), "Probe Found");
}

/// CR 714.3c + CR 714.2b: the Saga's chapter II, reached through turn flow, turns the card
/// face up and logs it publicly.
#[test]
fn saga_search_exile_face_down_stays_hidden_until_chapter_two() {
    let (mut runner, found, saga, _, _) = avacyn_chapter_one();
    let chapter_one_turn = runner.state().turn_number;
    let mut flipped = None;
    for _ in 0..64 {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        let result = runner.act(GameAction::PassPriority).unwrap();
        if !runner.state().objects[&found].face_down {
            flipped = Some(result);
            break;
        }
    }
    let flipped = flipped.expect("chapter II turns the card face up");
    let state = runner.state();
    assert_eq!(state.phase, Phase::PreCombatMain);
    assert!(state.turn_number > chapter_one_turn);
    assert_eq!(state.active_player, P0);
    assert_eq!(
        state.objects[&saga].counters.get(&CounterType::Lore),
        Some(&2)
    );
    assert_eq!(view_name(state, P1, found), "Probe Found");
    assert!(
        flipped.log_entries.iter().any(|entry| {
            entry.presentation.visibility == LogVisibility::Public
                && matches!(
                    entry.segments.as_slice(),
                    [LogSegment::CardName { object_id, .. }, LogSegment::Text(text)]
                        if *object_id == found && text == " is turned face up"
                )
        }),
        "{:?}",
        flipped.log_entries
    );
}

/// CR 400.2 + CR 406.3: a search's face-up exile is public.
#[test]
fn face_up_search_exile_stays_public() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands = [
        scenario.add_card_to_library_top(P0, "Probe Land A"),
        scenario.add_card_to_library_top(P0, "Probe Land B"),
    ];
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe Severance",
            false,
            "Search your library for any number of land cards, exile them, then shuffle.",
        )
        .id();
    let mut runner = scenario.build();
    for id in lands {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }
    let before = runner.state().clone();
    let outcome = runner.cast(spell).search_first_legal().resolve();
    outcome.assert_zone(&lands, Zone::Exile);
    let entries = resolve_log_entries(outcome.events(), &before, outcome.state());
    for (id, name) in lands.into_iter().zip(["Probe Land A", "Probe Land B"]) {
        assert!(!outcome.state().objects[&id].face_down);
        assert_eq!(view_name(outcome.state(), P1, id), name);
        assert!(
            entries
                .iter()
                .any(|entry| is_move_line(entry, id, Zone::Library, Zone::Exile)),
            "{entries:?}"
        );
    }
}

/// CR 406.3: Beseech the Mirror's search, face-down exile and return to hand in one action never
/// reveal the card.
#[test]
fn beseech_chain_keeps_the_card_unnamed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Probe Filler");
    let found = scenario.add_card_to_library_top(P0, "Probe Beseeched");
    let spell = scenario
        .add_spell_to_hand(P0, "Probe Beseech", false)
        .from_oracle_text_with_keywords(
            &["Bargain"],
            "Bargain (You may sacrifice an artifact, enchantment, or token as you cast this spell.)\nSearch your library for a card, exile it face down, then shuffle. If this spell was bargained, you may cast the exiled card without paying its mana cost if that spell's mana value is 4 or less. Put the exiled card into your hand if it wasn't cast this way.",
        )
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(spell).commit();
    for _ in [P0, P1] {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let chosen = runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .unwrap();

    assert!(move_position(&chosen, found, Zone::Library, Zone::Exile).is_some());
    // CR 701.24a: the search shuffles the searched library.
    assert!(shuffled_library(&chosen.events, P0));
    assert!(move_position(&chosen, found, Zone::Exile, Zone::Hand).is_some());
    assert_eq!(runner.state().objects[&found].zone, Zone::Hand);
    let entries = &chosen.log_entries;
    assert!(entries.iter().any(|entry| matches!(
        entry.segments.as_slice(),
        [LogSegment::CardName { object_id, .. }, LogSegment::Text(text)]
            if *object_id == spell && text == "'s effect resolves"
    )));
    assert!(entries_naming(entries, found).is_empty(), "{entries:?}");
}

const CULVERT_AMBUSHER: &str = "When this creature enters or is turned face up, target creature blocks this turn if able.\nDisguise {4}{G} (You may cast this card face down for {3} as a 2/2 creature with ward {2}. Turn it face up any time for its disguise cost.)";

/// CR 708.5: only the controller may look at a card played face down, so only they receive the
/// record naming it.
#[test]
fn face_down_play_keeps_the_card_out_of_opponents_events() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ambusher = scenario
        .add_creature_to_hand_from_oracle(P0, "Culvert Ambusher", 4, 5, CULVERT_AMBUSHER)
        .id();
    let mut runner = scenario.build();
    for _ in 0..3 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    let card_id = runner.state().objects[&ambusher].card_id;
    let played = runner
        .act(GameAction::PlayFaceDown {
            object_id: ambusher,
            card_id,
        })
        .unwrap();
    let state = runner.state();
    assert_eq!(state.objects[&ambusher].zone, Zone::Battlefield);
    assert!(state.objects[&ambusher].face_down);
    let hand_to_battlefield = vec![(Zone::Hand, Zone::Battlefield)];
    assert_eq!(
        named_moves(&played.events, ambusher, "Culvert Ambusher"),
        hand_to_battlefield
    );
    for viewer in [P1, SPECTATOR] {
        assert!(
            viewer_named_moves(&played.events, state, viewer, ambusher, "Culvert Ambusher")
                .is_empty()
        );
    }
    assert_eq!(
        viewer_named_moves(&played.events, state, P0, ambusher, "Culvert Ambusher"),
        hand_to_battlefield
    );
}

const KARN_SCION_OF_URZA: &str = "+1: Reveal the top two cards of your library. An opponent chooses one of them. Put that card into your hand and exile the other with a silver counter on it.\n−1: Put a card you own with a silver counter on it from exile into your hand.\n−2: Create a 0/0 colorless Construct artifact creature token with \"This token gets +1/+1 for each artifact you control.\"";

/// CR 400.2 + CR 406.3: a face-up card leaving exile for its owner's hand stays public.
#[test]
fn face_up_exile_to_hand_stays_public() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let karn = scenario
        .add_planeswalker_from_oracle(P0, "Karn, Scion of Urza", "Karn", 5, KARN_SCION_OF_URZA)
        .id();
    let exiled = scenario.add_spell_to_exile(P0, "Probe Silvered", true).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&exiled)
        .unwrap()
        .counters
        .insert(CounterType::Generic("silver".to_string()), 1);
    let outcome = runner.activate(karn, 1).target_object(exiled).resolve();
    let state = outcome.state();
    assert_eq!(state.objects[&exiled].zone, Zone::Hand);
    let exile_to_hand = vec![(Zone::Exile, Zone::Hand)];
    for viewer in [P0, P1, SPECTATOR] {
        assert_eq!(
            viewer_named_moves(outcome.events(), state, viewer, exiled, "Probe Silvered"),
            exile_to_hand
        );
    }
}

const YEDORA: &str = "Whenever another nontoken creature you control dies, you may return it to the battlefield face down under its owner's control. It's a Forest land. (It has no other types or abilities.)";

/// Acts until the stack is empty and priority returns, picking from `picks` at every card choice;
/// returns every event.
fn drive_to_empty_stack(runner: &mut GameRunner, picks: &[ObjectId]) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..32 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return events,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::SearchChoice { cards, count, .. }
            | WaitingFor::ChooseFromZoneChoice { cards, count, .. } => GameAction::SelectCards {
                cards: picks
                    .iter()
                    .filter(|id| cards.contains(id))
                    .chain(cards.iter().filter(|id| !picks.contains(id)))
                    .copied()
                    .take(*count)
                    .collect(),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    panic!("the stack never emptied");
}

/// CR 400.2: a face-down return from the graveyard, a public zone, is not hidden from opponents.
#[test]
fn face_down_return_from_a_public_zone_stays_public() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Yedora, Grave Gardener", 5, 5, YEDORA);
    let bear = scenario.add_creature(P0, "Probe Bear", 2, 2).id();
    let kill = add_free_instant(
        &mut scenario,
        P0,
        "Probe Murder",
        &[],
        "Destroy target creature.",
    );
    let mut runner = scenario.build();
    let _ = runner.cast(kill).target_objects(&[bear]).commit();
    let events = drive_to_empty_stack(&mut runner, &[]);
    let state = runner.state();
    assert_eq!(state.objects[&bear].zone, Zone::Battlefield);
    assert!(state.objects[&bear].face_down);
    assert_eq!(
        viewer_named_moves(&events, state, P1, bear, "Probe Bear"),
        vec![
            (Zone::Battlefield, Zone::Graveyard),
            (Zone::Graveyard, Zone::Battlefield)
        ]
    );
}

/// Casts `spell` from P0's hand at P1 and drives it to completion, picking `found` first.
fn cast_at_p1(runner: &mut GameRunner, spell: ObjectId, found: ObjectId) -> Vec<GameEvent> {
    let _ = runner.cast(spell).target_player(P1).commit();
    drive_to_empty_stack(runner, &[found])
}

fn searched_board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fill_libraries(&mut scenario, &[P0, P1]);
    scenario
}

/// CR 608.2c + CR 701.24a: "that player shuffles" after a search of a target opponent's library
/// shuffles that opponent's library, whoever ends up controlling the found card.
#[test]
fn searched_player_shuffles_their_library() {
    let mut shuffled = Vec::new();
    // Bribery: the found creature enters under the searcher's control.
    let mut scenario = searched_board();
    let found = scenario.add_card_to_library_top(P1, "Probe Foreign Creature");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bribery", false, "Search target opponent's library for a creature card and put that card onto the battlefield under your control. Then that player shuffles.")
        .id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&found).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
    let events = cast_at_p1(&mut runner, spell, found);
    assert_eq!(runner.state().objects[&found].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&found].controller, P0);
    shuffled.push((
        "Bribery",
        shuffled_library(&events, P1),
        shuffled_library(&events, P0),
    ));

    // Knowledge Exploitation: the searcher casts the found card.
    let mut scenario = searched_board();
    let found = scenario
        .add_spell_to_library_top(P1, "Probe Foreign Instant", true)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Knowledge Exploitation", false, "Prowl {3}{U} (You may cast this for its prowl cost if you dealt combat damage to a player this turn with a Rogue.)\nSearch target opponent's library for an instant or sorcery card. You may cast that card without paying its mana cost. Then that player shuffles.")
        .id();
    let mut runner = scenario.build();
    let events = cast_at_p1(&mut runner, spell, found);
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::SpellCast { object_id, controller, .. } if *object_id == found && *controller == P0
    )));
    shuffled.push((
        "Knowledge Exploitation",
        shuffled_library(&events, P1),
        shuffled_library(&events, P0),
    ));

    // Gifts Given: the anaphor is "that player shuffles their library".
    let mut scenario = searched_board();
    let found = scenario.add_card_to_library_top(P1, "Probe Foreign Gift");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Gifts Given", true, "Search target opponent's library for four cards with different names and reveal them. That player chooses two of those cards. Put the chosen cards into the player's graveyard and the rest into your hand. Then that player shuffles their library.")
        .id();
    let mut runner = scenario.build();
    let events = cast_at_p1(&mut runner, spell, found);
    assert_ne!(runner.state().objects[&found].zone, Zone::Library);
    shuffled.push((
        "Gifts Given",
        shuffled_library(&events, P1),
        shuffled_library(&events, P0),
    ));

    // Earwig Squad: the search is a triggered ability's.
    let mut scenario = searched_board();
    let found = scenario.add_card_to_library_top(P1, "Probe Foreign Found");
    let earwig = scenario
        .add_creature_to_hand_from_oracle(P0, "Earwig Squad", 5, 3, "Prowl {2}{B} (You may cast this for its prowl cost if you dealt combat damage to a player this turn with a Goblin or Rogue.)\nWhen this creature enters, if its prowl cost was paid, search target opponent's library for three cards and exile them. Then that player shuffles.")
        .with_subtypes(vec!["Goblin", "Rogue"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Colorless, ManaType::Black]
            .into_iter()
            .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .creature_types_dealt_combat_damage_this_turn
        .insert((P0, "Rogue".to_string()));
    let _ = runner.cast(earwig).commit();
    let events = drive_to_empty_stack(&mut runner, &[found]);
    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    shuffled.push((
        "Earwig Squad",
        shuffled_library(&events, P1),
        shuffled_library(&events, P0),
    ));
    assert_eq!(
        shuffled,
        [
            "Bribery",
            "Knowledge Exploitation",
            "Gifts Given",
            "Earwig Squad"
        ]
        .map(|member| (member, true, false))
    );
}

const AUDITORE_AMBUSH: &str = "Choose one or both —\n• Return target creature to its owner's hand.\n• Target player searches their library and/or graveyard for a card named Ezio, Blade of Vengeance, reveals it, and puts it into their hand. If they search their library this way, they shuffle.";

/// Casts Auditore Ambush from P0 with `modes` at P1's bear and at P1 as the searching player,
/// optionally bolting the bear in response, and drives it to an empty stack; returns the zones of
/// the bear and P1's Ezio, then whether P1's and P0's libraries were shuffled.
fn auditore_ambush_outcome(modes: &[usize], bolt_the_creature: bool) -> (Zone, Zone, bool, bool) {
    let mut scenario = searched_board();
    let found = scenario.add_card_to_library_top(P1, "Ezio, Blade of Vengeance");
    let bear = scenario.add_creature(P1, "Probe Bear", 2, 2).id();
    let bolt = scenario.add_bolt_to_hand(P0);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Auditore Ambush", false, AUDITORE_AMBUSH)
        .id();
    let mut runner = scenario.build();
    let cast = runner.cast(spell).modes(modes);
    let cast = if modes.contains(&0) {
        cast.target_object(bear)
    } else {
        cast
    };
    let mut commit = cast.target_player(P1).commit();
    if bolt_the_creature {
        let _ = commit.cast(bolt).target_object(bear).commit();
    }
    let events = drive_to_empty_stack(&mut runner, &[found]);
    let objects = &runner.state().objects;
    (
        objects[&bear].zone,
        objects[&found].zone,
        shuffled_library(&events, P1),
        shuffled_library(&events, P0),
    )
}

/// CR 700.2c + CR 608.2c: "they" in the search mode is that mode's target player, whichever
/// mode is chosen with it.
#[test]
fn auditore_ambush_both_modes_shuffle_the_searched_players_library() {
    assert_eq!(
        auditore_ambush_outcome(&[0, 1], false),
        (Zone::Hand, Zone::Hand, true, false)
    );
}

#[test]
fn auditore_ambush_search_mode_alone_shuffles_the_searched_players_library() {
    assert_eq!(
        auditore_ambush_outcome(&[1], false),
        (Zone::Battlefield, Zone::Hand, true, false)
    );
}

/// CR 608.2b: the illegal creature target does not stop the search mode's legal target player.
#[test]
fn auditore_ambush_illegal_creature_target_still_shuffles_the_searched_player() {
    assert_eq!(
        auditore_ambush_outcome(&[0, 1], true),
        (Zone::Graveyard, Zone::Hand, true, false)
    );
}

const LEYLINE_OF_ANTICIPATION: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.";
const LEYLINE_OF_SANCTITY: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou have hexproof. (You can't be the target of spells or abilities your opponents control.)";

/// CR 608.2b: when the searched player becomes an illegal target, "if they search their library
/// this way, they shuffle" needs information about them, so no library is shuffled.
#[test]
fn auditore_ambush_illegal_searched_player_shuffles_no_library() {
    let mut scenario = searched_board();
    let found = scenario.add_card_to_library_top(P1, "Ezio, Blade of Vengeance");
    let bear = scenario.add_creature(P1, "Probe Bear", 2, 2).id();
    scenario.add_enchantment_from_oracle(P1, "Leyline of Anticipation", LEYLINE_OF_ANTICIPATION);
    let sanctity = scenario
        .add_spell_to_hand(P1, "Leyline of Sanctity", false)
        .as_enchantment()
        .from_oracle_text(LEYLINE_OF_SANCTITY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Auditore Ambush", false, AUDITORE_AMBUSH)
        .id();
    let mut runner = scenario.build();
    let _ = runner
        .cast(spell)
        .modes(&[0, 1])
        .target_object(bear)
        .target_player(P1)
        .commit();
    pass_priority_to(&mut runner, P1);
    let _ = runner.cast(sanctity).commit();
    let events = drive_to_empty_stack(&mut runner, &[found]);
    // CR 702.11c: the response made P1 an illegal target; the creature mode still resolved.
    assert!(engine::game::static_abilities::player_has_hexproof(
        runner.state(),
        P1
    ));
    let objects = &runner.state().objects;
    assert_eq!(objects[&bear].zone, Zone::Hand);
    assert_eq!(objects[&found].zone, Zone::Library);
    assert_eq!(
        (shuffled_library(&events, P1), shuffled_library(&events, P0)),
        (false, false)
    );
}

const DROMOKAS_COMMAND: &str = "Choose two —\n• Prevent all damage target instant or sorcery spell would deal this turn.\n• Target player sacrifices an enchantment of their choice.\n• Put a +1/+1 counter on target creature.\n• Target creature you control fights target creature you don't control.";

/// CR 701.14b: when the fight mode's opposing fighter is an illegal target, neither creature fights.
#[test]
fn illegal_later_mode_fighter_means_no_fight() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let countered = scenario.add_creature(P0, "Probe Countered", 1, 5).id();
    let fighter = scenario.add_creature(P0, "Probe Fighter", 2, 6).id();
    let foe = scenario.add_creature(P1, "Probe Foe", 4, 3).id();
    let bolt = scenario.add_bolt_to_hand(P0);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dromoka's Command", true, DROMOKAS_COMMAND)
        .id();
    let mut runner = scenario.build();
    let mut commit = runner
        .cast(spell)
        .modes(&[2, 3])
        .target_objects(&[countered, fighter, foe])
        .commit();
    let _ = commit.cast(bolt).target_object(foe).commit();
    drive_to_empty_stack(&mut runner, &[]);
    let objects = &runner.state().objects;
    assert_eq!(objects[&foe].zone, Zone::Graveyard);
    assert_eq!(
        objects[&countered].counters.get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    assert_eq!(
        (
            objects[&countered].damage_marked,
            objects[&fighter].damage_marked
        ),
        (0, 0)
    );
}

fn json_objects<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            out.push(value);
            map.values().for_each(|child| json_objects(child, out));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|child| json_objects(child, out)),
        _ => {}
    }
}

/// The searcher's look is bound for face-down search exiles from the library only, so every
/// exported one must come from there.
#[test]
fn face_down_search_exiles_come_only_from_the_library() {
    let Some(export) = crate::support::shared_card_export_json() else {
        return;
    };
    let mut objects = Vec::new();
    export
        .values()
        .for_each(|face| json_objects(face, &mut objects));
    let mut face_down_exiles = 0;
    for root in objects
        .into_iter()
        .filter(|object| object.get("effect").is_some() && object.get("sub_ability").is_some())
    {
        let mut subtree = Vec::new();
        json_objects(root, &mut subtree);
        if !subtree
            .iter()
            .any(|node| node.get("type").and_then(|kind| kind.as_str()) == Some("SearchLibrary"))
        {
            continue;
        }
        for node in subtree
            .into_iter()
            .filter(|node| node.get("face_down_in_exile") == Some(&serde_json::Value::Bool(true)))
        {
            face_down_exiles += 1;
            assert_eq!(node["effect"]["origin"], "Library", "{node}");
        }
    }
    assert!(face_down_exiles > 0);
}
