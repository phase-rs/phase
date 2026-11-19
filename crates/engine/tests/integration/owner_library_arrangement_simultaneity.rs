//! CR 101.4 + CR 401.4 + CR 800.4a (issue #10217): when one instruction puts
//! several owners' cards into one library position, each owner arranges their
//! own cards in APNAP order, and then every card moves in one simultaneous
//! action. Replacements inside that one move are preserved and the instruction
//! completes once; a player leaving mid-arrangement is reconciled with the
//! players and objects still in the game.

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::visibility::filter_events_for_viewer;
use engine::types::ability::{DigRestSplitScope, EffectKind, LibraryPosition};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, PersistedGameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HALLOWED_BURIAL_ORACLE: &str = "Put all creatures on the bottom of their owners' libraries.";

const EXTRACTOR_DEMON_ORACLE: &str = "Flying\n\
     Whenever another creature leaves the battlefield, you may have target player mill two \
     cards.\n\
     Unearth {2}{B} ({2}{B}: Return this card from your graveyard to the battlefield. It gains \
     haste. Exile it at the beginning of the next end step or if it would leave the battlefield. \
     Unearth only as a sorcery.)";

const P2: PlayerId = PlayerId(2);

/// {3}{W}{W} for Hallowed Burial.
fn burial_mana() -> Vec<ManaUnit> {
    [
        ManaType::White,
        ManaType::White,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ]
    .into_iter()
    .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
    .collect()
}

fn add_hallowed_burial(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Hallowed Burial", false, HALLOWED_BURIAL_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![
                engine::types::mana::ManaCostShard::White,
                engine::types::mana::ManaCostShard::White,
            ],
            generic: 3,
        })
        .id()
}

fn library(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// The owner's Bottom arrangement prompt over exactly `cards` (in any order).
/// Asserted unconditionally: any other `WaitingFor` fails the row.
fn assert_bottom_arrangement_prompt(waiting_for: &WaitingFor, owner: PlayerId, cards: &[ObjectId]) {
    match waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards: offered,
            effect_kind,
            library_position,
            mass_library_order,
            ..
        } => {
            assert_eq!(*player, owner, "CR 401.4: the cards' owner arranges them");
            let mut offered = offered.clone();
            offered.sort_by_key(|id| id.0);
            let mut expected = cards.to_vec();
            expected.sort_by_key(|id| id.0);
            assert_eq!(
                offered, expected,
                "the prompt names exactly that owner's cards"
            );
            assert_eq!(*effect_kind, EffectKind::PutAtLibraryPosition);
            assert_eq!(*library_position, Some(LibraryPosition::Bottom));
            assert!(
                mass_library_order.is_some(),
                "the prompt carries the frozen owner batch"
            );
        }
        other => panic!("expected {owner:?}'s CR 401.4 ordering prompt, got {other:?}"),
    }
}

/// CR 401.4: the batch owns the permutation of approved physical arrivals.
fn assert_approved_arrangement(
    waiting_for: &WaitingFor,
    owner: PlayerId,
    expected: &[ObjectId],
    on_top: bool,
) {
    let WaitingFor::DigRestSplitChoice {
        player,
        library_owner,
        cards,
        top_count,
        scope: DigRestSplitScope::OrderOnly,
        ..
    } = waiting_for
    else {
        panic!("expected {owner:?}'s CR 401.4 ordering prompt, got {waiting_for:?}");
    };
    assert_eq!((*player, *library_owner), (owner, owner));
    assert_eq!(*top_count, if on_top { expected.len() } else { 0 });
    let mut cards = cards.clone();
    cards.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(cards, expected);
}

fn put_at_library_position_resolutions(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::PutAtLibraryPosition,
                    ..
                }
            )
        })
        .count()
}

/// Put pending triggers on the stack: order them as listed and give each the
/// first legal target, until a player has priority.
fn drive_trigger_prompts(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => return,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("a trigger takes its first legal target");
            }
            other => panic!("unexpected prompt while putting triggers on the stack: {other:?}"),
        }
    }
    panic!("triggers were not put on the stack within the prompt budget");
}

/// Resolve the stack to an empty priority window, declining every optional
/// mill so the libraries stay as asserted.
fn resolve_to_priority(runner: &mut GameRunner) {
    for _ in 0..128 {
        let state = runner.state();
        let action = match &state.waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: false }
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("a trigger takes its first legal target");
                continue;
            }
            WaitingFor::Priority { .. } if !state.stack.is_empty() => GameAction::PassPriority,
            WaitingFor::Priority { .. } => return,
            other => panic!("unexpected prompt while resolving to priority: {other:?}"),
        };
        runner.act(action).expect("resolve the stack");
    }
    panic!("the stack did not resolve within the prompt budget");
}

/// CR 101.4 + CR 401.4 + CR 603.10a + CR 603.2c (issue #10217): Hallowed
/// Burial puts every creature on the bottom of its owner's library. P0 owns
/// Extractor Demon and a bear; P1 owns `a` (which P0 controls) and `b`. Each
/// owner orders their own cards — P1 orders `a`, per the card's ruling,
/// whoever controlled it — and then all four move at once. The Demon leaves in
/// that same event and sees all three other creatures leave (CR 603.10a; its
/// ruling: it triggers when it and another creature leave at the same time),
/// triggering once for each (CR 603.2c).
///
/// REVERT-FAILING: with per-owner delivery restored, P0's two creatures are
/// already in P0's library when P1 is prompted, and the Demon is in the
/// library when P1's creatures leave, so it triggers once instead of three
/// times.
#[test]
fn hallowed_burial_collects_every_owners_order_before_one_simultaneous_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["P0 Library"]);
    scenario.with_library_top(P1, &["P1 Library"]);
    let demon = scenario
        .add_creature_from_oracle(P0, "Extractor Demon", 5, 5, EXTRACTOR_DEMON_ORACLE)
        .id();
    let p0_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let a = scenario
        .add_creature(P1, "Borrowed Bear", 2, 2)
        .controlled_by(P0)
        .id();
    let b = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    let l0 = library(&runner, P0)[0];
    let l1 = library(&runner, P1)[0];

    let outcome = runner.cast(burial).resolve();
    assert_bottom_arrangement_prompt(outcome.final_waiting_for(), P0, &[demon, p0_bear]);
    runner
        .act(GameAction::SelectCards {
            cards: vec![p0_bear, demon],
        })
        .expect("P0 submits their arrangement");

    // CR 101.4: nothing moves before the last owner chooses.
    for id in [demon, p0_bear, a, b] {
        assert_eq!(
            zone(&runner, id),
            Zone::Battlefield,
            "CR 101.4: no creature moves before P1 chooses"
        );
    }
    assert_bottom_arrangement_prompt(&runner.state().waiting_for, P1, &[a, b]);

    runner
        .act(GameAction::SelectCards { cards: vec![b, a] })
        .expect("P1 submits their arrangement");
    assert_eq!(library(&runner, P0), vec![l0, p0_bear, demon]);
    assert_eq!(
        library(&runner, P1),
        vec![l1, b, a],
        "P1, the owner, ordered the creature P0 controlled"
    );
    assert_eq!(zone(&runner, burial), Zone::Graveyard);
    assert!(runner.state().pending_mass_library_order_choice.is_none());

    // Reach guard: at least one trigger shows the Demon's ability parses and
    // fires; exactly three show it saw every co-departing creature.
    drive_trigger_prompts(&mut runner);
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == demon)
            .count(),
        3,
        "CR 603.10a + CR 603.2c: the departing Demon sees every other owner's creature leave"
    );
    resolve_to_priority(&mut runner);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 903.9b + CR 616.1 + CR 101.4: a commander in the combined move may go to
/// the command zone instead. Its owner's replacement choice pauses the one
/// simultaneous move after every owner has ordered; answering it finishes the
/// move, and the instruction completes exactly once.
///
/// Preservation row: the single `EffectResolved` is the engine's completion
/// contract (one completion per instruction), not a card fact. It
/// discriminates against a per-owner completion or a dropped suffix after the
/// replacement pause.
#[test]
fn hallowed_burial_commander_replacement_pauses_the_combined_move_and_completes_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["P0 Library"]);
    scenario.with_library_top(P1, &["P1 Library"]);
    let x = scenario.add_creature(P0, "P0 Bear X", 2, 2).id();
    let y = scenario.add_creature(P0, "P0 Bear Y", 2, 2).id();
    // CR 903.3: the commander designation is an attribute of the card, so the
    // commander stays on the battlefield (`with_commander` would move it).
    let c = {
        let mut builder = scenario.add_creature(P1, "Commanding Bear", 2, 2);
        builder.as_legendary().commander();
        builder.id()
    };
    let b = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let l0 = library(&runner, P0)[0];
    let l1 = library(&runner, P1)[0];

    let outcome = runner.cast(burial).resolve();
    let mut completions = put_at_library_position_resolutions(outcome.events());
    assert_bottom_arrangement_prompt(outcome.final_waiting_for(), P0, &[x, y]);
    let first = runner
        .act(GameAction::SelectCards { cards: vec![y, x] })
        .expect("P0 submits their arrangement");
    completions += put_at_library_position_resolutions(&first.events);
    assert_bottom_arrangement_prompt(&runner.state().waiting_for, P1, &[c, b]);
    let second = runner
        .act(GameAction::SelectCards { cards: vec![b, c] })
        .expect("P1 submits their arrangement");
    completions += put_at_library_position_resolutions(&second.events);

    // Reach guard: the commander's replacement really pauses the move.
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => assert_eq!(*player, P1),
        other => panic!("expected P1's CR 903.9b replacement choice, got {other:?}"),
    }
    assert_eq!(completions, 0, "the instruction has not completed yet");

    let answered = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 sends the commander to the command zone");
    completions += put_at_library_position_resolutions(&answered.events);

    assert_eq!(zone(&runner, c), Zone::Command);
    assert_eq!(library(&runner, P0), vec![l0, y, x]);
    assert_eq!(library(&runner, P1), vec![l1, b]);
    assert_eq!(completions, 1, "the arrangement completes exactly once");
    assert_eq!(zone(&runner, burial), Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 104.3a + CR 800.4a + CR 400.7: P1 owns both creatures, which P2
/// controls with no control effect. P2 concedes while P1 is ordering them, so
/// CR 800.4a exiles both as objects still controlled by the departed player.
/// P1 has nothing left to arrange; the instruction completes empty and the
/// spell finishes.
///
/// REVERT-FAILING: without the departure reconciliation, P1's prompt still
/// names the exiled creatures and the game waits on it.
#[test]
fn hallowed_burial_owner_whose_cards_leave_with_their_controller_completes_empty() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    let x = scenario
        .add_creature(P1, "Borrowed Bear X", 2, 2)
        .controlled_by(P2)
        .id();
    let y = scenario
        .add_creature(P1, "Borrowed Bear Y", 2, 2)
        .controlled_by(P2)
        .id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.cast(burial).resolve();
    assert_bottom_arrangement_prompt(outcome.final_waiting_for(), P1, &[x, y]);

    let conceded = runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert_eq!(zone(&runner, x), Zone::Exile);
    assert_eq!(zone(&runner, y), Zone::Exile);
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events),
        1,
        "the instruction completes once, empty"
    );
    assert_eq!(zone(&runner, burial), Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert!(runner.state().pending_mass_library_order_choice.is_none());
}

const GEMRAZER_ORACLE: &str = "Mutate {1}{G}{G} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nReach, trample\nWhenever this creature mutates, destroy target artifact or enchantment an opponent controls.";

fn add_component_pair(
    scenario: &mut GameScenario,
    owner: PlayerId,
    commander: bool,
) -> (ObjectId, ObjectId) {
    let host = scenario
        .add_creature_from_oracle(owner, "Nyxborn Courser", 2, 4, "")
        .as_enchantment()
        .as_creature()
        .id();
    let rider = if commander {
        scenario
            .add_creature_to_hand_from_oracle(
                owner,
                "Otrimi, the Ever-Playful",
                6,
                6,
                OTRIMI_ORACLE,
            )
            .as_legendary()
            .with_subtypes(vec!["Nightmare", "Beast"])
            .commander()
            .id()
    } else {
        scenario
            .add_creature_to_hand_from_oracle(owner, "Gemrazer", 4, 4, GEMRAZER_ORACLE)
            .id()
    };
    (host, rider)
}

// Infrastructure exception: this direct merge stands in for the printed creature's
// public Mutate cast so these rows isolate owner arrangement and component delivery.
fn merge_pair(runner: &mut GameRunner, host: ObjectId, rider: ObjectId) {
    engine::game::merge::merge_object_onto(
        runner.state_mut(),
        rider,
        host,
        engine::game::merge::MergeSide::Bottom,
        &mut Vec::new(),
    );
}

fn round_trip_arrangement(runner: &mut GameRunner) {
    runner
        .state()
        .resolution_stack
        .validate_at_rest(&runner.state().waiting_for)
        .unwrap();
    let saved =
        serde_json::to_string(&PersistedGameState::capture(runner.state().clone())).unwrap();
    let restored = serde_json::from_str::<PersistedGameState>(&saved)
        .unwrap()
        .into_game_state()
        .unwrap();
    restored
        .resolution_stack
        .validate_at_rest(&restored.waiting_for)
        .unwrap();
    *runner.state_mut() = restored;
}

fn assert_arrangement_finished(runner: &GameRunner) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

/// CR 401.4 + CR 730.3 + CR 730.3a: for merged A+B and ordinary C,
/// the owner can privately choose A, C, B. A+B still departs as one permanent.
#[test]
fn hallowed_burial_interleaves_components_with_an_ordinary_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut mana = burial_mana();
    mana.extend((0..2).map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])));
    scenario.with_mana_pool(P0, mana);
    scenario.with_library_top(P0, &["Sentinel"]);
    let host = scenario
        .add_creature_from_oracle(P0, "Nyxborn Courser", 2, 4, "")
        .as_enchantment()
        .as_creature()
        .with_subtypes(vec!["Centaur", "Scout"])
        .id();
    const OCTOPUS_ORACLE: &str = "Mutate {1}{U} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nFlash\nWhenever this creature deals combat damage to a player, draw a card.";
    let rider = scenario
        .add_creature_to_hand(P0, "Sea-Dasher Octopus", 2, 2)
        .with_subtypes(vec!["Octopus"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["Mutate", "Flash"], OCTOPUS_ORACLE)
        .id();
    let ordinary = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    // CR 702.140a + CR 702.140c: reach the merged permanent through its
    // printed alternative cost and the public choice, not a constructed merge.
    let mutate = runner
        .cast(rider)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .casting_variant(CastingVariant::Mutate)
        .target_object(host)
        .resolve();
    assert!(matches!(
        mutate.final_waiting_for(),
        WaitingFor::MutateMergeChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseMutateMergeSide {
            side: engine::game::merge::MergeSide::Top,
        })
        .unwrap();
    assert_eq!(
        runner.state().objects[&host].merged_components,
        vec![rider, host]
    );
    assert_arrangement_finished(&runner);
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(burial).resolve();
    assert_approved_arrangement(
        cast.final_waiting_for(),
        P0,
        &[ordinary, host, rider],
        false,
    );
    let mut events = cast.events().to_vec();
    // CR 702.140c + CR 730.2a: choosing ranks is not a battlefield exit.
    let assert_merged = |runner: &GameRunner| {
        let permanent = &runner.state().objects[&host];
        assert_eq!(permanent.merged_components, [rider, host]);
        assert_eq!(permanent.name, "Sea-Dasher Octopus");
        assert_eq!((permanent.power, permanent.toughness), (Some(2), Some(2)));
        assert!(runner.state().battlefield.contains(&host));
        assert!(!runner.state().battlefield.contains(&rider));
    };
    assert_merged(&runner);
    // Checked import must reject both a forged survivor and an omitted
    // authority, even when the member and its proposal agree with each other.
    let wrong = serde_json::json!({
        "object_id": host,
        "incarnation": runner.state().objects[&host].incarnation + 1,
    });
    for (authority, expected) in [
        (Some(wrong), "no matching announced merge departure"),
        (None, "missing its merge departure"),
    ] {
        let mut saved =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let frames = saved["state"]["resolution_frames"]["frames"]
            .as_array_mut()
            .unwrap();
        let member = frames
            .iter_mut()
            .find_map(|frame| {
                frame
                    .get_mut("data")?
                    .get_mut("staged")?
                    .get_mut("members")?
                    .as_array_mut()?
                    .iter_mut()
                    .find(|member| member["identity"]["object_id"] == serde_json::json!(rider))
            })
            .expect("saved physical arrival");
        let approval = member["approved_event"]["ZoneChange"]
            .as_object_mut()
            .unwrap();
        if let Some(authority) = authority {
            approval.insert("merged_departure".into(), authority.clone());
            member["merged_departure"] = authority;
        } else {
            approval
                .remove("merged_departure")
                .expect("approval has authority");
            member
                .as_object_mut()
                .unwrap()
                .remove("merged_departure")
                .expect("physical member has authority");
        }
        let error = serde_json::from_value::<PersistedGameState>(saved)
            .unwrap()
            .into_game_state()
            .expect_err("invalid merge departure must not reopen");
        assert!(error.to_string().contains(expected), "{error}");
    }
    round_trip_arrangement(&mut runner);
    assert_merged(&runner);
    let mut alternate = GameRunner::from_state(runner.state().clone());
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![host, ordinary, rider],
            })
            .unwrap()
            .events,
    );
    let mut alternate_events = cast.events().to_vec();
    alternate_events.extend(
        alternate
            .act(GameAction::SelectCards {
                cards: vec![ordinary, host, rider],
            })
            .unwrap()
            .events,
    );
    assert_eq!(library(&runner, P0), vec![sentinel, host, ordinary, rider]);
    assert_eq!(
        library(&alternate, P0),
        vec![sentinel, ordinary, host, rider]
    );
    // CR 401.4: neither an opponent nor an unseated spectator learns which
    // order the owner chose from the zone-change event stream.
    for viewer in [P1, PlayerId(u8::MAX)] {
        assert_eq!(
            filter_events_for_viewer(&events, runner.state(), viewer),
            filter_events_for_viewer(&alternate_events, alternate.state(), viewer),
        );
    }
    for (id, origin) in [
        (rider, None),
        (ordinary, Some(Zone::Battlefield)),
        (host, Some(Zone::Battlefield)),
    ] {
        let departures: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from,
                    to,
                    ..
                } if *object_id == id => Some((*from, *to)),
                _ => None,
            })
            .collect();
        assert_eq!(departures, vec![(origin, Zone::Library)]);
    }
    // CR 603.10a: the one departure sees the merged permanent, not Courser.
    let departed = events
        .iter()
        .find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                record,
                ..
            } if *object_id == host => Some(record),
            _ => None,
        })
        .expect("the merged permanent departs");
    assert_eq!(departed.name, "Sea-Dasher Octopus");
    assert_eq!((departed.power, departed.toughness), (Some(2), Some(2)));
    assert_eq!(runner.state().objects[&host].name, "Nyxborn Courser");
    assert_arrangement_finished(&runner);
    assert_arrangement_finished(&alternate);
}

/// CR 401.4 + CR 730.3a: Top placement also allows an ordinary card to
/// interleave with the components of one merged permanent.
#[test]
fn harmonic_convergence_interleaves_components_at_the_top() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Sentinel"]);
    let (host, rider) = add_component_pair(&mut scenario, P0, false);
    let ordinary = scenario
        .add_enchantment_from_oracle(P0, "Ordinary enchantment", "")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Harmonic Convergence",
            true,
            "Put all enchantments on top of their owners' libraries.",
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(spell).resolve();
    assert_approved_arrangement(cast.final_waiting_for(), P0, &[host, rider, ordinary], true);
    runner
        .act(GameAction::SelectCards {
            cards: vec![rider, ordinary, host],
        })
        .unwrap();
    assert_eq!(library(&runner, P0), vec![rider, ordinary, host, sentinel]);
    assert_arrangement_finished(&runner);
}

/// CR 903.9b + CR 730.3a: the absorbed commander is offered its redirect
/// before the interleaved physical arrangement, including after save/reopen.
#[test]
fn hallowed_burial_absorbed_commander_choice_and_arrangement_round_trip() {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["Sentinel"]);
    let (host, rider) = add_component_pair(&mut scenario, P0, true);
    let ordinary = scenario.add_creature(P0, "Ordinary creature", 2, 2).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(burial).resolve();
    assert!(matches!(
        cast.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    round_trip_arrangement(&mut runner);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert_approved_arrangement(
        &runner.state().waiting_for,
        P0,
        &[host, rider, ordinary],
        false,
    );
    round_trip_arrangement(&mut runner);
    runner
        .act(GameAction::SelectCards {
            cards: vec![rider, ordinary, host],
        })
        .unwrap();
    assert_eq!(library(&runner, P0), vec![sentinel, rider, ordinary, host]);
    assert_arrangement_finished(&runner);
}

const OTRIMI_ORACLE: &str = "Mutate {1}{B}{G}{U} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nTrample\nWhenever this creature deals combat damage to a player, return target creature card with mutate from your graveyard to your hand.";
const ARCADES_ORACLE: &str = "Flying, vigilance\nWhenever a creature you control with defender enters, draw a card.\nEach creature you control with defender assigns combat damage equal to its toughness rather than its power and can attack as though it didn't have defender.";

fn check_hallowed_burial_commander_host_round_trip(with_ordinary: bool, accept_command: bool) {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mut mana = burial_mana();
    mana.extend(
        [ManaType::Green, ManaType::Green, ManaType::Colorless]
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![])),
    );
    scenario.with_mana_pool(P0, mana);
    scenario.with_library_top(P0, &["Sentinel"]);
    let host = scenario
        .add_creature_from_oracle(P0, "Arcades, the Strategist", 3, 5, ARCADES_ORACLE)
        .as_legendary()
        .with_subtypes(vec!["Elder", "Dragon"])
        .commander()
        .id();
    let rider = scenario
        .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER_ORACLE)
        .with_subtypes(vec!["Beast"])
        .id();
    let ordinary = with_ordinary.then(|| scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id());
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    let mutate = runner
        .cast(rider)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .casting_variant(CastingVariant::Mutate)
        .target_object(host)
        .resolve();
    assert!(matches!(
        mutate.final_waiting_for(),
        WaitingFor::MutateMergeChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseMutateMergeSide {
            side: engine::game::merge::MergeSide::Bottom,
        })
        .unwrap();
    assert_eq!(
        runner.state().objects[&host].merged_components,
        [host, rider]
    );
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(burial).resolve();
    assert!(matches!(
        cast.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let mut events = cast.events().to_vec();
    if !with_ordinary {
        // The ordinary plan writes the pre-plan ChangeZone payload: no
        // arrangement field. The following reopen must keep ordinary delivery.
        let saved =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        assert!(saved_change_zone_tail(&saved).get("arrangement").is_none());
    }
    round_trip_arrangement(&mut runner);
    events.extend(
        runner
            .act(GameAction::ChooseReplacement {
                index: usize::from(!accept_command),
            })
            .unwrap()
            .events,
    );
    if let Some(ordinary) = ordinary {
        let order = if accept_command {
            vec![rider, ordinary]
        } else {
            vec![host, ordinary, rider]
        };
        assert_eq!(library(&runner, P0), vec![sentinel]);
        assert_approved_arrangement(&runner.state().waiting_for, P0, &order, false);
        round_trip_arrangement(&mut runner);
        let mut alternate = GameRunner::from_state(runner.state().clone());
        let mut alternate_events = events.clone();
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: order.clone(),
                })
                .unwrap()
                .events,
        );
        let reversed: Vec<_> = order.iter().rev().copied().collect();
        alternate_events.extend(
            alternate
                .act(GameAction::SelectCards {
                    cards: reversed.clone(),
                })
                .unwrap()
                .events,
        );
        assert_eq!(
            library(&runner, P0),
            [&[sentinel], order.as_slice()].concat()
        );
        assert_eq!(
            library(&alternate, P0),
            [&[sentinel], reversed.as_slice()].concat()
        );
        for viewer in [P1, PlayerId(u8::MAX)] {
            assert_eq!(
                filter_events_for_viewer(&events, runner.state(), viewer),
                filter_events_for_viewer(&alternate_events, alternate.state(), viewer),
            );
        }
        let departures = |id, origin| {
            events
                .iter()
                .filter(|event| {
                    matches!(event, GameEvent::ZoneChanged { object_id, from, .. }
                        if *object_id == id && *from == origin)
                })
                .count()
        };
        assert_eq!(departures(host, Some(Zone::Battlefield)), 1);
        assert_eq!(departures(rider, Some(Zone::Battlefield)), 0);
        assert_eq!(departures(rider, None), 1);
        assert_arrangement_finished(&alternate);
    }
    if ordinary.is_none() && !accept_command {
        match &runner.state().waiting_for {
            WaitingFor::DigRestSplitChoice { player, cards, .. } => {
                assert_eq!(*player, P0);
                let mut offered = cards.clone();
                offered.sort_unstable();
                let mut expected = vec![host, rider];
                expected.sort_unstable();
                assert_eq!(offered, expected);
            }
            other => panic!("expected lone merged permanent's component order, got {other:?}"),
        }
        runner
            .act(GameAction::SelectCards {
                cards: vec![rider, host],
            })
            .unwrap();
    }
    let mut expected_library = vec![sentinel];
    if let Some(ordinary) = ordinary {
        if accept_command {
            expected_library.extend([rider, ordinary]);
        } else {
            expected_library.extend([host, ordinary, rider]);
        }
        assert_eq!(zone(&runner, ordinary), Zone::Library);
    } else if accept_command {
        expected_library.push(rider);
    } else {
        expected_library.extend([rider, host]);
    }
    assert_eq!(library(&runner, P0), expected_library);
    assert_eq!(
        zone(&runner, host),
        if accept_command {
            Zone::Command
        } else {
            Zone::Library
        }
    );
    assert_eq!(zone(&runner, rider), Zone::Library);
    assert_eq!(zone(&runner, burial), Zone::Graveyard);
    assert_arrangement_finished(&runner);
}

/// CR 903.9b + CR 903.9c: Arcades is the representing permanent, with Gemrazer
/// mutated underneath it. Accepting the command-zone replacement moves only
/// the commander there, after a resumable, persisted component choice.
#[test]
fn hallowed_burial_lone_commander_host_accepts_command_after_round_trip() {
    check_hallowed_burial_commander_host_round_trip(false, true);
}

/// CR 903.9b + CR 730.3a: Arcades represents the permanent and Gemrazer is
/// underneath; declining the replacement preserves the component order choice.
#[test]
fn hallowed_burial_lone_commander_host_declines_command_after_round_trip() {
    check_hallowed_burial_commander_host_round_trip(false, false);
}

/// CR 903.9b + CR 903.9c + CR 401.4: Arcades represents the permanent, but
/// redirecting it to command leaves only Gemrazer and the ordinary card in the
/// library order, and neither private permutation changes a viewer's events.
#[test]
fn hallowed_burial_interleaved_commander_host_accepts_command_after_round_trip() {
    check_hallowed_burial_commander_host_round_trip(true, true);
}

/// CR 903.9b + CR 730.3a + CR 401.4: Arcades represents the permanent and
/// Gemrazer is underneath; declining retains the full interleaved physical order.
#[test]
fn hallowed_burial_interleaved_commander_host_declines_command_after_round_trip() {
    check_hallowed_burial_commander_host_round_trip(true, false);
}

/// CR 101.4 + CR 616.1 + CR 903.9c: the active player's merged commander
/// and the nonactive player's ordinary commander share an approval window.
#[test]
fn hallowed_burial_schedules_merged_and_ordinary_commander_choices_apnap() {
    let mut merged_first = Vec::new();
    for ordinary_first in [true, false] {
        let mut scenario =
            GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let mut mana = burial_mana();
        mana.extend(
            [ManaType::Green, ManaType::Green, ManaType::Colorless]
                .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![])),
        );
        scenario.with_mana_pool(P0, mana);
        let add_isamaru = |scenario: &mut GameScenario| {
            scenario
                .add_creature_from_oracle(P1, "Isamaru, Hound of Konda", 2, 2, "")
                .as_legendary()
                .with_subtypes(vec!["Dog"])
                .commander()
                .id()
        };
        let isamaru = ordinary_first.then(|| add_isamaru(&mut scenario));
        let host = scenario
            .add_creature_from_oracle(P0, "Arcades, the Strategist", 3, 5, ARCADES_ORACLE)
            .as_legendary()
            .with_subtypes(vec!["Elder", "Dragon"])
            .commander()
            .id();
        let isamaru = isamaru.unwrap_or_else(|| add_isamaru(&mut scenario));
        let rider = scenario
            .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER_ORACLE)
            .with_subtypes(vec!["Beast"])
            .id();
        let bear = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
        let burial = add_hallowed_burial(&mut scenario);
        let mut runner = scenario.build();
        let mutate = runner
            .cast(rider)
            .alternative_cast(AlternativeCastDecision::Alternative)
            .casting_variant(CastingVariant::Mutate)
            .target_object(host)
            .resolve();
        assert!(matches!(
            mutate.final_waiting_for(),
            WaitingFor::MutateMergeChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::ChooseMutateMergeSide {
                side: engine::game::merge::MergeSide::Bottom,
            })
            .unwrap();
        let before = library(&runner, P0);
        // The resolver enumerates the object map, not scenario builder order.
        merged_first.push(
            runner
                .state()
                .objects
                .keys()
                .find(|&&id| id == host || id == isamaru)
                == Some(&host),
        );
        runner.cast(burial).resolve();
        for expected in [P0, P1] {
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "expected {expected:?}'s commander choice, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, expected, "ordinary_first={ordinary_first}");
            let index = candidates
                .iter()
                .position(|candidate| candidate.description == "Move to command zone")
                .expect("commander redirect is offered");
            runner.act(GameAction::ChooseReplacement { index }).unwrap();
            if expected == P0 {
                round_trip_arrangement(&mut runner);
            }
        }
        assert_approved_arrangement(&runner.state().waiting_for, P0, &[rider, bear], false);
        let saved =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        assert_eq!(
            saved_change_zone_tail(&saved)["arrangement"],
            "ArrangePhysicalLibraryArrivals"
        );
        assert_eq!(library(&runner, P0), before);
        runner
            .act(GameAction::SelectCards {
                cards: vec![bear, rider],
            })
            .unwrap();
        assert_eq!(
            library(&runner, P0),
            [before.as_slice(), &[bear, rider]].concat()
        );
        assert_eq!(zone(&runner, host), Zone::Command);
        assert_eq!(zone(&runner, isamaru), Zone::Command);
        assert_arrangement_finished(&runner);
    }
    assert_ne!(
        merged_first[0], merged_first[1],
        "exercise both announcement orders"
    );
}

fn saved_change_zone_tail(saved: &serde_json::Value) -> &serde_json::Value {
    saved["state"]["resolution_frames"]["frames"]
        .as_array()
        .expect("saved resolution stack")
        .iter()
        .find_map(|frame| frame["data"]["completion"]["ChangeZonePopulationSettled"].get("tail"))
        .expect("saved ChangeZone completion tail")
}

/// CR 903.9b + CR 730.3a + CR 401.4: declining the command-zone replacement
/// leaves the merged permanent's component order pending, and that pending
/// choice survives a save/reopen before the resumed admission delivers.
#[test]
fn hallowed_burial_declined_component_order_survives_round_trip_and_continues() {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["Sentinel"]);
    let host = scenario
        .add_creature_from_oracle(P0, "Otrimi, the Ever-Playful", 6, 6, OTRIMI_ORACLE)
        .with_subtypes(vec!["Nightmare", "Beast"])
        .commander()
        .id();
    let rider = scenario
        .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER_ORACLE)
        .with_subtypes(vec!["Beast"])
        .id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(burial).resolve();
    // One merged permanent, one owner: the pending replacement is the first
    // pause (the component order only follows a decline).
    assert!(matches!(
        cast.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    // Save/reopen before the decline: the resumable replacement pause is intact.
    round_trip_arrangement(&mut runner);
    let declined = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("P0 declines the command-zone replacement");
    assert_eq!(
        put_at_library_position_resolutions(&declined.events),
        0,
        "the declined host still owes its component order"
    );
    match &runner.state().waiting_for {
        WaitingFor::DigRestSplitChoice { player, cards, .. } => {
            assert_eq!(*player, P0);
            let mut offered = cards.clone();
            offered.sort_unstable();
            let mut expected = vec![host, rider];
            expected.sort_unstable();
            assert_eq!(offered, expected);
        }
        other => panic!("expected lone merged permanent's component order, got {other:?}"),
    }
    // Save/reopen with the component-order choice pending, then finish it.
    round_trip_arrangement(&mut runner);
    runner
        .act(GameAction::SelectCards {
            cards: vec![rider, host],
        })
        .expect("P0 submits the resumed component order");
    assert_eq!(
        library(&runner, P0),
        vec![sentinel, rider, host],
        "the declined host and its rider land in submitted order"
    );
    assert_eq!(zone(&runner, host), Zone::Library);
    assert_eq!(zone(&runner, rider), Zone::Library);
    assert_eq!(zone(&runner, burial), Zone::Graveyard);
    assert!(
        runner.state().active_batch_delivery().is_none(),
        "the resumed order settles its batch owner"
    );
    assert_arrangement_finished(&runner);
}

/// CR 730.3c + CR 401.4 + CR 401.7: a merged permanent put "just beneath the
/// top X cards" makes its components one owner arrangement at a fixed slot, and
/// that pending fixed-depth arrangement is a savable state: capture, reopen and
/// finish it.
#[test]
fn unexpectedly_absent_fixed_depth_component_arrangement_survives_reopen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [ManaType::White, ManaType::White, ManaType::White]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let absent = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unexpectedly Absent",
            false,
            "Put target nonland permanent into its owner's library just beneath the top X cards of that library.",
        )
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![
                engine::types::mana::ManaCostShard::X,
                engine::types::mana::ManaCostShard::White,
                engine::types::mana::ManaCostShard::White,
            ],
            generic: 0,
        })
        .id();
    let host = scenario.add_creature(P1, "Merged host", 3, 3).id();
    let rider = scenario.add_creature(P1, "Merged rider", 2, 2).id();
    scenario.with_library_top(P1, &["Sentinel"]);
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let sentinel = library(&runner, P1)[0];

    runner.cast(absent).target_object(host).x(1).commit();
    if matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }) {
        runner.act(GameAction::ChooseX { value: 1 }).unwrap();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(host)),
            })
            .unwrap();
    }
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .unwrap();
            }
            WaitingFor::ChooseXValue { .. } => {
                runner.act(GameAction::ChooseX { value: 1 }).unwrap();
            }
            WaitingFor::DigRestSplitChoice { .. } => break,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner.act(GameAction::PassPriority).unwrap();
            }
        }
    }
    let WaitingFor::DigRestSplitChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!(
            "the fixed-depth component arrangement must pause for its owner: {:?} host={:?} rider={:?} lib={:?}",
            runner.state().waiting_for,
            zone(&runner, host),
            zone(&runner, rider),
            library(&runner, P1)
        );
    };
    assert_eq!(*player, P1);
    let mut offered = cards.clone();
    offered.sort_unstable();
    let mut expected = vec![host, rider];
    expected.sort_unstable();
    assert_eq!(offered, expected);

    // The blocker: this pending fixed-slot arrangement must pass persistence
    // validation, so the game can be saved and reopened here.
    round_trip_arrangement(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DigRestSplitChoice { player, .. } if player == P1
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![rider, host],
        })
        .expect("P1 submits the reopened fixed-depth arrangement");
    assert_eq!(
        library(&runner, P1),
        vec![sentinel, rider, host],
        "the X=1 slot keeps one card above the pair, submitted in top-down order"
    );
    assert_eq!(zone(&runner, host), Zone::Library);
    assert_eq!(zone(&runner, rider), Zone::Library);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 903.9b + CR 401.4: a separate commander's replacement pauses the
/// logical approval sweep; save/reopen before either component is arranged.
#[test]
fn hallowed_burial_ordinary_commander_choice_round_trips_before_component_order() {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["Sentinel"]);
    let commander = scenario
        .add_creature(P0, "Commander", 2, 2)
        .commander()
        .id();
    let (host, rider) = add_component_pair(&mut scenario, P0, false);
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(burial).resolve();
    assert_eq!(library(&runner, P0), vec![sentinel]);
    assert!(matches!(
        cast.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let staged = runner
        .state()
        .active_batch_delivery()
        .unwrap()
        .staged
        .as_ref()
        .unwrap();
    assert_eq!(
        staged.stage,
        engine::types::game_state::BatchDeliveryStage::Approving
    );
    let member = staged
        .members
        .iter()
        .find(|member| member.identity.object_id == host)
        .unwrap();
    assert_eq!(
        member.state,
        engine::types::game_state::StagedBatchMemberState::Approved
    );
    assert!(
        member.held_decision.is_none(),
        "the noncommander permanent needs no choice"
    );
    assert!(
        matches!(
            member.approved_event,
            Some(engine::types::proposed_event::ProposedEvent::ZoneChange {
                object_id,
                to: Zone::Library,
                ..
            }) if object_id == host
        ),
        "the sweep approved the merged permanent's own library move, got {:?}",
        member.approved_event
    );
    assert_eq!(
        staged.asking.map(|asking| asking.object_id),
        Some(commander),
        "the commander's choice is the one asked"
    );
    assert!(staged.arrangements.is_empty());
    round_trip_arrangement(&mut runner);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert_approved_arrangement(
        &runner.state().waiting_for,
        P0,
        &[commander, rider, host],
        false,
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![commander, rider, host],
        })
        .unwrap();
    assert_eq!(library(&runner, P0), vec![sentinel, commander, rider, host]);
    assert_arrangement_finished(&runner);
}

/// CR 730.3 + CR 730.3a: a lone merged permanent supplies two library cards
/// but only one departure. Its owner still arranges those cards privately.
#[test]
fn hallowed_burial_lone_merged_permanent_keeps_component_order_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["Sentinel"]);
    let (host, rider) = add_component_pair(&mut scenario, P0, false);
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(burial).resolve();
    match cast.final_waiting_for() {
        WaitingFor::DigRestSplitChoice { cards, .. } => {
            let mut offered = cards.clone();
            offered.sort_unstable();
            let mut expected = vec![host, rider];
            expected.sort_unstable();
            assert_eq!(offered, expected);
        }
        other => panic!("expected standalone component order, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![rider, host],
        })
        .unwrap();
    assert_eq!(library(&runner, P0), vec![sentinel, rider, host]);
    assert_arrangement_finished(&runner);
}

/// CR 800.4a + CR 401.4: a departing owner loses their physical arrivals
/// while another owner's already-submitted arrangement finishes.
#[test]
fn hallowed_burial_owner_concedes_during_approved_arrangement() {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Sentinel"]);
    }
    let a = scenario.add_creature(P0, "P0 creature", 2, 2).id();
    let other = scenario.add_creature(P0, "Other P0 creature", 2, 2).id();
    let (host, rider) = add_component_pair(&mut scenario, P1, false);
    let commander = scenario
        .add_creature(P1, "Commander", 2, 2)
        .commander()
        .id();
    let b = scenario.add_creature(P2, "P2 creature", 2, 2).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let l0 = library(&runner, P0)[0];
    let l2 = library(&runner, P2)[0];
    let cast = runner.cast(burial).resolve();
    assert!(matches!(
        cast.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert_approved_arrangement(&runner.state().waiting_for, P0, &[a, other], false);
    runner
        .act(GameAction::SelectCards {
            cards: vec![other, a],
        })
        .unwrap();
    assert_approved_arrangement(
        &runner.state().waiting_for,
        P1,
        &[host, rider, commander],
        false,
    );
    let departure = runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    for card in [host, rider, commander] {
        assert_eq!(zone(&runner, card), Zone::Exile);
        assert!(
            !departure.events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, to: Zone::Library, .. }
                    if *object_id == card
            )),
            "departing owner's card must never be delivered to a library"
        );
    }
    runner
        .state()
        .resolution_stack
        .validate_at_rest(&runner.state().waiting_for)
        .unwrap();
    assert!(runner.state().active_batch_delivery().is_none());
    assert_eq!(library(&runner, P0), vec![l0, other, a]);
    assert_eq!(library(&runner, P2), vec![l2, b]);
    assert_arrangement_finished(&runner);
}

/// CR 800.4a + CR 903.9b: a surviving chooser can finish consultation after
/// a different owner's later members have been abandoned.
#[test]
fn hallowed_burial_survivor_commander_choice_skips_a_departed_later_member() {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Sentinel"]);
    }
    let commander = scenario
        .add_creature(P0, "Commander", 2, 2)
        .commander()
        .id();
    let departed = scenario.add_creature(P1, "Departing creature", 2, 2).id();
    let survivor = scenario.add_creature(P2, "Surviving creature", 2, 2).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    let l0 = library(&runner, P0)[0];
    let l2 = library(&runner, P2)[0];
    let cast = runner.cast(burial).resolve();
    assert_bottom_arrangement_prompt(cast.final_waiting_for(), P0, &[commander]);
    for (owner, card) in [(P0, commander), (P1, departed), (P2, survivor)] {
        assert_bottom_arrangement_prompt(&runner.state().waiting_for, owner, &[card]);
        runner
            .act(GameAction::SelectCards { cards: vec![card] })
            .unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert_eq!(library(&runner, P0), vec![l0, commander]);
    assert_eq!(library(&runner, P2), vec![l2, survivor]);
    assert_eq!(zone(&runner, departed), Zone::Exile);
    assert_eq!(zone(&runner, burial), Zone::Graveyard);
    assert_arrangement_finished(&runner);
}

/// The predecessor writer (`bea11941d`, v0.122.0) asked the owner for Hallowed
/// Burial's order before the CR 903.9b commander choice. These saves are taken
/// at that choice; see each fixture's `.provenance` for the generator and what
/// the predecessor itself did next.
const BASE_9039B_ORDER: &str = include_str!("../fixtures/base_9039b_hallowed_order.json");
const BASE_9039B_MERGED: &str = include_str!("../fixtures/base_9039b_merged_hallowed.json");

/// CR 401.4 + CR 903.9b: a predecessor save in which P0 had already arranged
/// Gemrazer above its commander Otrimi (the reverse of battlefield order) and
/// then paused on the commander's command-zone choice. Checked restore keeps
/// that submitted order: declining puts Otrimi beneath Gemrazer, with no second
/// prompt, and the result survives another save and reopen.
#[test]
fn base_writer_submitted_order_survives_checked_restore_through_the_decline() {
    let restored = serde_json::from_str::<PersistedGameState>(BASE_9039B_ORDER)
        .expect("the predecessor payload decodes")
        .into_game_state()
        .expect("the predecessor payload passes checked restore");
    restored
        .resolution_stack
        .validate_at_rest(&restored.waiting_for)
        .expect("the restored pause validates");
    let mut runner = GameRunner::from_state(restored);
    let (sentinel, otrimi, gemrazer) = (ObjectId(1), ObjectId(2), ObjectId(3));
    assert_eq!(
        runner.state().objects[&otrimi].name,
        "Otrimi, the Ever-Playful"
    );
    assert_eq!(runner.state().objects[&gemrazer].name, "Gemrazer");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert_eq!(
        library(&runner, P0),
        vec![sentinel, gemrazer],
        "the card ordered first is already at the bottom"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("P0 keeps its commander out of the command zone");
    assert_eq!(
        library(&runner, P0),
        vec![sentinel, gemrazer, otrimi],
        "the order P0 submitted before the save, not battlefield order"
    );
    assert_arrangement_finished(&runner);
    round_trip_arrangement(&mut runner);
    assert_eq!(library(&runner, P0), vec![sentinel, gemrazer, otrimi]);
    assert_arrangement_finished(&runner);
}

/// CR 616.1 + CR 730.3d + CR 903.9b: the predecessor save of the same pause
/// with Otrimi merged over Gemrazer holds the commander's choice in two batch
/// deliveries that the payload does not relate. The predecessor engine panicked
/// on the decline, live and after restore (see the fixture's `.provenance`).
/// Checked restore refuses the save with a typed error instead of resuming it
/// in a guessed order.
#[test]
fn base_writer_merged_commander_pause_is_rejected_at_checked_restore() {
    let decoded = serde_json::from_str::<PersistedGameState>(BASE_9039B_MERGED)
        .expect("the predecessor payload decodes");
    match decoded.into_game_state() {
        Err(engine::types::game_state::PersistedRestoreError::UnresumableSharedBatchPause {
            member,
        }) => {
            assert_eq!(
                member.object_id,
                ObjectId(2),
                "the commander's pause is named"
            );
        }
        Ok(state) => panic!(
            "the unresumable pause must not restore: {:?}",
            state.waiting_for
        ),
        Err(other) => panic!("expected the typed shared-pause rejection, got {other}"),
    }
}

const EVACUATION_ORACLE: &str = "Return all creatures to their owners' hands.";

/// CR 903.9b + CR 730.3d: Evacuation returns a merged commander (Otrimi over
/// Gemrazer) to its owner's hand. Its owner may put the commander into the
/// command zone instead, and that choice parks the original batch and its
/// linked component child on the same incarnation. The linked pair is a valid
/// save: checked restore accepts it whichever frame it inspects first, and the
/// reopened choice settles either way.
#[test]
fn evacuation_merged_commander_choice_reopens_before_the_answer() {
    for accept_command in [false, true] {
        let mut scenario =
            GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario
            .add_creature_from_oracle(P0, "Otrimi, the Ever-Playful", 6, 6, OTRIMI_ORACLE)
            .as_legendary()
            .with_subtypes(vec!["Nightmare", "Beast"])
            .commander()
            .id();
        let rider = scenario
            .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER_ORACLE)
            .with_subtypes(vec!["Beast"])
            .id();
        let evacuation = scenario
            .add_spell_to_hand_from_oracle(P0, "Evacuation", true, EVACUATION_ORACLE)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        merge_pair(&mut runner, host, rider);
        runner.cast(evacuation).resolve();
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected the commander's CR 903.9b choice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, P0);
        assert!(candidates
            .iter()
            .any(|candidate| candidate.description == "Move to command zone"));
        // Save and reopen at the choice, before answering it.
        round_trip_arrangement(&mut runner);
        let index = candidates
            .iter()
            .position(|candidate| {
                (candidate.description == "Move to command zone") == accept_command
            })
            .expect("both branches are offered");
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("P0 answers the reopened choice");
        let expected_host = if accept_command {
            Zone::Command
        } else {
            Zone::Hand
        };
        assert_eq!(
            zone(&runner, host),
            expected_host,
            "accept={accept_command}"
        );
        assert_eq!(zone(&runner, rider), Zone::Hand, "accept={accept_command}");
        assert_eq!(zone(&runner, evacuation), Zone::Graveyard);
        assert!(runner.state().active_batch_delivery().is_none());
        assert_arrangement_finished(&runner);
    }
}

/// CR 730.3c: Evacuation's serial component choice must use the split card's
/// actual survivor, not a departure authority supplied by the saved proposal.
#[test]
fn evacuation_rejects_unowned_merge_authority_on_an_absorbed_commander() {
    let db = super::support::shared_card_db().expect("integration card fixture must load");
    for accept_command in [false, true] {
        let mut scenario =
            GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_real_card(P0, "Gemrazer", Zone::Battlefield, db);
        let commander = scenario
            .add_creature_to_hand_from_oracle(P0, "Otrimi, the Ever-Playful", 6, 6, OTRIMI_ORACLE)
            .as_legendary()
            .with_subtypes(vec!["Nightmare", "Beast"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Black,
                    ManaCostShard::Green,
                    ManaCostShard::Blue,
                ],
                generic: 3,
            })
            .id();
        scenario.with_commander(commander);
        let evacuation = scenario.add_real_card(P0, "Evacuation", Zone::Hand, db);
        scenario.with_mana_pool(
            P0,
            [ManaType::Black, ManaType::Green]
                .into_iter()
                .chain([ManaType::Blue; 9])
                .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        runner
            .cast(commander)
            .alternative_cast(AlternativeCastDecision::Alternative)
            .casting_variant(CastingVariant::Mutate)
            .target_object(host)
            .resolve();
        runner
            .act(GameAction::ChooseMutateMergeSide {
                side: engine::game::merge::MergeSide::Bottom,
            })
            .unwrap();
        assert_eq!(
            runner.state().objects[&host].merged_components,
            [host, commander]
        );
        runner.cast(evacuation).resolve();
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("Evacuation must ask Otrimi's owner before moving the split card");
        };
        assert_eq!(*player, P0);
        let index = candidates
            .iter()
            .position(|candidate| {
                (candidate.description == "Move to command zone") == accept_command
            })
            .expect("both answers are offered");

        let mut saved =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let proposed = &mut saved["state"]["pending_replacement"]["proposed"]["ZoneChange"];
        assert_eq!(proposed["object_id"], serde_json::json!(commander));
        assert!(proposed.get("merged_departure").is_none());
        proposed["merged_departure"] =
            serde_json::json!({"object_id": evacuation, "incarnation": 0});
        let error = serde_json::from_value::<PersistedGameState>(saved)
            .unwrap()
            .into_game_state()
            .expect_err("a serial proposal cannot carry a staged merge departure");
        assert!(error.to_string().contains("no staged approval"), "{error}");

        round_trip_arrangement(&mut runner);
        runner.act(GameAction::ChooseReplacement { index }).unwrap();
        assert_eq!(zone(&runner, host), Zone::Hand);
        assert_eq!(
            zone(&runner, commander),
            if accept_command {
                Zone::Command
            } else {
                Zone::Hand
            }
        );
        let survivor = runner.state().objects[&commander]
            .split_from_merge_survivor
            .expect("the absorbed commander names the survivor after the split");
        assert_eq!(
            survivor,
            ObjectIncarnationRef::from_object(&runner.state().objects[&host])
        );
        assert_eq!(zone(&runner, evacuation), Zone::Graveyard);
        assert_arrangement_finished(&runner);
    }
}
