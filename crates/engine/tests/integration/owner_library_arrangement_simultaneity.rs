//! CR 101.4 + CR 401.4 + CR 800.4a (issue #10217): when one instruction puts
//! several owners' cards into one library position, each owner arranges their
//! own cards in APNAP order, and then every card moves in one simultaneous
//! action. Replacements inside that one move are preserved and the instruction
//! completes once; a player leaving mid-arrangement is reconciled with the
//! players and objects still in the game.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{EffectKind, LibraryPosition};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
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
    let rider = {
        let mut card =
            scenario.add_creature_to_hand_from_oracle(owner, "Gemrazer", 4, 4, GEMRAZER_ORACLE);
        if commander {
            card.commander();
        }
        card.id()
    };
    (host, rider)
}

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

/// CR 401.4 + CR 730.3a: the physical cards interleave in the one submitted
/// order, rather than obtaining an independent component order later.
#[test]
fn hallowed_burial_interleaves_components_with_an_ordinary_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["Sentinel"]);
    let (host, rider) = add_component_pair(&mut scenario, P0, false);
    let ordinary = scenario.add_creature(P0, "Ordinary creature", 2, 2).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(burial).resolve();
    assert_bottom_arrangement_prompt(cast.final_waiting_for(), P0, &[ordinary, host, rider]);
    let mut events = cast.events().to_vec();
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![rider, ordinary, host],
            })
            .unwrap()
            .events,
    );
    assert_eq!(library(&runner, P0), vec![sentinel, rider, ordinary, host]);
    for id in [rider, ordinary, host] {
        assert_eq!(events.iter().filter(|event| matches!(
            event, GameEvent::ZoneChanged { object_id, to: Zone::Library, .. } if *object_id == id
        )).count(), 1);
    }
    assert_arrangement_finished(&runner);
}

/// CR 401.4 + CR 730.3a: Top ordering discriminates verbatim inheritance from
/// a component-only filtered copy of the parent's ranks.
#[test]
fn harmonic_convergence_top_interleave_keeps_inherited_order() {
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
    match cast.final_waiting_for() {
        WaitingFor::EffectZoneChoice {
            cards,
            library_position: Some(LibraryPosition::Top),
            ..
        } => {
            let mut offered = cards.clone();
            offered.sort_unstable();
            let mut expected = vec![host, rider, ordinary];
            expected.sort_unstable();
            assert_eq!(offered, expected);
        }
        other => panic!("expected physical Top arrangement, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![rider, ordinary, host],
        })
        .unwrap();
    assert_eq!(library(&runner, P0), vec![rider, ordinary, host, sentinel]);
    assert_arrangement_finished(&runner);
}

/// CR 903.9b + CR 730.3a: a component-specific replacement retains the full
/// parent order through the production persistence chokepoint.
#[test]
fn hallowed_burial_component_commander_pause_round_trips_the_inherited_arrangement() {
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
    assert_bottom_arrangement_prompt(cast.final_waiting_for(), P0, &[ordinary, host, rider]);
    runner
        .act(GameAction::SelectCards {
            cards: vec![rider, ordinary, host],
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let child = runner.state().active_batch_delivery().unwrap();
    assert!(child.component_delivery_parent.is_some());
    round_trip_arrangement(&mut runner);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert_eq!(library(&runner, P0), vec![sentinel, rider, ordinary, host]);
    assert_arrangement_finished(&runner);
}

const OTRIMI_ORACLE: &str = "Mutate {1}{B}{G}{U} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nTrample\nWhenever this creature deals combat damage to a player, return target creature card with mutate from your graveyard to your hand.";

fn check_hallowed_burial_commander_host_round_trip(with_ordinary: bool, accept_command: bool) {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["Sentinel"]);
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
    let ordinary = with_ordinary.then(|| scenario.add_creature(P0, "Ordinary creature", 2, 2).id());
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_pair(&mut runner, host, rider);
    let sentinel = library(&runner, P0)[0];
    let cast = runner.cast(burial).resolve();
    if let Some(ordinary) = ordinary {
        assert_bottom_arrangement_prompt(cast.final_waiting_for(), P0, &[host, rider, ordinary]);
        runner
            .act(GameAction::SelectCards {
                cards: vec![rider, ordinary, host],
            })
            .unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let child = runner.state().active_batch_delivery().unwrap();
    assert_eq!(
        child
            .component_delivery_parent
            .as_ref()
            .unwrap()
            .member
            .object_id,
        host
    );
    round_trip_arrangement(&mut runner);
    runner
        .act(GameAction::ChooseReplacement {
            index: usize::from(!accept_command),
        })
        .unwrap();
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
        expected_library.extend([rider, ordinary]);
        if !accept_command {
            expected_library.push(host);
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

/// CR 903.9b + CR 903.9c: Otrimi is the representing permanent, with Gemrazer
/// mutated underneath it. Accepting the command-zone replacement moves only
/// the commander there, after a resumable, persisted component choice.
#[test]
fn hallowed_burial_lone_commander_host_accepts_command_after_round_trip() {
    check_hallowed_burial_commander_host_round_trip(false, true);
}

/// CR 903.9b + CR 730.3a: Otrimi represents the permanent and Gemrazer is
/// underneath; declining the replacement preserves the component order choice.
#[test]
fn hallowed_burial_lone_commander_host_declines_command_after_round_trip() {
    check_hallowed_burial_commander_host_round_trip(false, false);
}

/// CR 903.9b + CR 903.9c + CR 401.4: Otrimi represents the permanent and
/// Gemrazer is underneath; redirecting the host preserves the other cards' order.
#[test]
fn hallowed_burial_interleaved_commander_host_accepts_command_after_round_trip() {
    check_hallowed_burial_commander_host_round_trip(true, true);
}

/// CR 903.9b + CR 730.3a + CR 401.4: Otrimi represents the permanent and
/// Gemrazer is underneath; declining retains the full interleaved physical order.
#[test]
fn hallowed_burial_interleaved_commander_host_declines_command_after_round_trip() {
    check_hallowed_burial_commander_host_round_trip(true, false);
}

/// CR 903.9b + CR 401.4: an earlier consultation can pause while the seeded
/// permanent has not yet been consulted. Its original submitted order survives.
#[test]
fn hallowed_burial_commander_first_pause_round_trips_unconsulted_component_seed() {
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
    assert_bottom_arrangement_prompt(cast.final_waiting_for(), P0, &[commander, host, rider]);
    runner
        .act(GameAction::SelectCards {
            cards: vec![commander, rider, host],
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
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
        engine::types::game_state::StagedBatchMemberState::Unconsulted
    );
    assert_eq!(member.approved_event, None);
    assert_eq!(
        staged.arrangements[0]
            .batch
            .members
            .iter()
            .map(|entry| entry.identity.object_id)
            .collect::<Vec<_>>(),
        vec![commander, rider, host]
    );
    round_trip_arrangement(&mut runner);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert_eq!(library(&runner, P0), vec![sentinel, commander, rider, host]);
    assert_arrangement_finished(&runner);
}

/// CR 730.3a: without an outer submitted order a lone merged permanent still
/// obtains the ordinary component order prompt.
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

/// CR 800.4a + CR 401.4: abandoning a covered permanent prunes all its physical
/// entries while the other owners' already-submitted cards finish.
#[test]
fn hallowed_burial_owner_concedes_while_seeded_group_is_live() {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Sentinel"]);
    }
    let a = scenario.add_creature(P0, "P0 creature", 2, 2).id();
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
    assert_bottom_arrangement_prompt(cast.final_waiting_for(), P0, &[a]);
    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .unwrap();
    assert_bottom_arrangement_prompt(&runner.state().waiting_for, P1, &[host, rider, commander]);
    runner
        .act(GameAction::SelectCards {
            cards: vec![rider, host, commander],
        })
        .unwrap();
    assert_bottom_arrangement_prompt(&runner.state().waiting_for, P2, &[b]);
    runner
        .act(GameAction::SelectCards { cards: vec![b] })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
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
    // The batch has already settled. Entry pruning is witnessed by the lib rows
    // `prune_inherited_entries_keeps_link_equality` and
    // `component_detach_strips_inherited_foreign_entries`.
    assert!(runner.state().active_batch_delivery().is_none());
    assert_eq!(library(&runner, P0), vec![l0, a]);
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
