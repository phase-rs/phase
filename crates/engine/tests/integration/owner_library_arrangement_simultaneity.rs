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
use engine::types::game_state::WaitingFor;
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
