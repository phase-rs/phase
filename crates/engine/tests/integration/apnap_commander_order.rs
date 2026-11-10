//! CR 101.4 + CR 616.1 + CR 903.9b: replacement choices during one simultaneous
//! mass move follow APNAP order, not object-id order.
//!
//! Hallowed Burial puts every creature on the bottom of its owner's library in
//! one combined move. When a commander is in that move, CR 903.9b offers its
//! owner the command-zone replacement at the would-move boundary. The delivery
//! order of the combined move therefore decides which owner is asked first, and
//! CR 616.1/APNAP require the active player's replacement choice first — not the
//! owner whose commander happens to carry the lower object id.
//!
//! The fixture allocates the commanders in the opposite order: P1's commander
//! is created first (lower `ObjectId`) while P0 is the active player, so a
//! global sort of the carrier ids parks P1's CR 903.9b choice first. The fixed
//! owner-arrangement batch emits carriers per owner in APNAP order instead, so
//! P0 is asked first and P1 second.
//!
//! REVERT-FAILING: restoring the old global
//! `carriers.sort_by_key(|card_id| card_id.0)` in
//! `engine_resolution_choices.rs` makes the first parked `ReplacementChoice`
//! target P1 (the lower-id owner), which the first assertion here rejects.
//!
//! Trace (fixed order):
//! 1. `cast`/`resolve` parks P0's bottom-arrangement prompt, then P1's.
//! 2. Both submissions start the combined delivery: P0's commander is the first
//!    carrier, so the CR 903.9b prompt targets P0.
//! 3. Accepting sends P0's commander to the command zone and resumes the parked
//!    batch; P1's commander is the next carrier and prompts P1.
//! 4. Accepting finishes the batch: both commanders are in the command zone and
//!    the instruction completes exactly once.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
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

/// A commander permanent owned by `owner`. `with_commander` would move the card,
/// so the designation is applied to the battlefield creature instead (CR 903.3).
fn add_commander(scenario: &mut GameScenario, owner: PlayerId, name: &str) -> ObjectId {
    let mut builder = scenario.add_creature(owner, name, 2, 2);
    builder.as_legendary().commander();
    builder.id()
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

/// The parked CR 903.9b optional replacement prompt, asserted to belong to
/// `owner` and to name `commander` as the replaced object.
fn assert_commander_replacement_prompt(
    waiting_for: &WaitingFor,
    owner: PlayerId,
    commander: ObjectId,
) {
    match waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(
                *player, owner,
                "CR 616.1 + CR 903.9b: the commander's owner answers first in APNAP order"
            );
            assert_eq!(
                candidates[0].source_id, commander,
                "the parked CR 903.9b prompt belongs to this commander"
            );
        }
        other => panic!("expected {owner:?}'s CR 903.9b replacement prompt, got {other:?}"),
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

/// CR 101.4 + CR 616.1 + CR 903.9b: the first parked commander replacement in
/// one combined Hallowed Burial move targets the active player first, even when
/// that active player's commander has the higher object id.
#[test]
fn mass_library_delivery_asks_active_commander_owner_before_lower_id_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["P0 Library"]);
    scenario.with_library_top(P1, &["P1 Library"]);

    // Allocation order is the OPPOSITE of turn/APNAP order: P1's commander is
    // created first and therefore owns the lower ObjectId, while P0 is the
    // active player and must be asked first.
    let p1_commander = add_commander(&mut scenario, P1, "P1 Commanding Bear");
    let p0_commander = add_commander(&mut scenario, P0, "P0 Commanding Bear");
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    assert_eq!(
        runner.state().active_player,
        P0,
        "P0 is the active player, so APNAP starts with P0"
    );
    assert!(
        p1_commander.0 < p0_commander.0,
        "fixture invariant: the later-turn owner's commander must carry the lower id"
    );
    let l0 = library(&runner, P0)[0];
    let l1 = library(&runner, P1)[0];

    // Step 1: each owner arranges their own commander, in APNAP order.
    let outcome = runner.cast(burial).resolve();
    let mut completions = put_at_library_position_resolutions(outcome.events());
    assert_bottom_arrangement_prompt(outcome.final_waiting_for(), P0, &[p0_commander]);
    let first = runner
        .act(GameAction::SelectCards {
            cards: vec![p0_commander],
        })
        .expect("P0 submits their arrangement");
    completions += put_at_library_position_resolutions(&first.events);
    assert_eq!(
        zone(&runner, p0_commander),
        Zone::Battlefield,
        "CR 101.4: nothing moves before the last owner chooses"
    );
    assert_bottom_arrangement_prompt(&runner.state().waiting_for, P1, &[p1_commander]);
    let second = runner
        .act(GameAction::SelectCards {
            cards: vec![p1_commander],
        })
        .expect("P1 submits their arrangement");
    completions += put_at_library_position_resolutions(&second.events);
    assert_eq!(completions, 0, "the combined move has not completed yet");

    // Step 2: the combined delivery starts. Both commanders are still on the
    // battlefield, and the first CR 903.9b choice belongs to P0 (APNAP), NOT to
    // P1 whose commander has the lower id.
    for id in [p0_commander, p1_commander] {
        assert_eq!(zone(&runner, id), Zone::Battlefield);
    }
    assert_commander_replacement_prompt(&runner.state().waiting_for, P0, p0_commander);

    // Step 3: accept P0's command-zone replacement. The parked batch resumes and
    // immediately parks P1's CR 903.9b choice.
    let answered_p0 = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 sends the commander to the command zone");
    completions += put_at_library_position_resolutions(&answered_p0.events);
    assert_eq!(
        zone(&runner, p0_commander),
        Zone::Command,
        "CR 903.9b: P0's accepted replacement redirected the move to the command zone"
    );
    assert_eq!(
        zone(&runner, p1_commander),
        Zone::Battlefield,
        "the parked tail has not delivered P1's commander yet"
    );
    assert_eq!(
        completions, 0,
        "the instruction is still parked on P1's replacement choice"
    );
    assert_commander_replacement_prompt(&runner.state().waiting_for, P1, p1_commander);

    // Step 4: accept P1's command-zone replacement and finish the batch.
    let answered_p1 = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 sends the commander to the command zone");
    completions += put_at_library_position_resolutions(&answered_p1.events);

    assert_eq!(zone(&runner, p0_commander), Zone::Command);
    assert_eq!(zone(&runner, p1_commander), Zone::Command);
    assert_eq!(
        library(&runner, P0),
        vec![l0],
        "the commander never entered P0's library"
    );
    assert_eq!(
        library(&runner, P1),
        vec![l1],
        "the commander never entered P1's library"
    );
    assert_eq!(zone(&runner, burial), Zone::Graveyard);
    assert_eq!(
        completions, 1,
        "the combined arrangement completes exactly once"
    );
    assert!(runner.state().pending_mass_library_order_choice.is_none());
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}
