//! Silent misparse: "The owner of target creature an opponent controls puts it into
//! their library second from the top or on the bottom" (Trickster's Stratagem,
//! Deem Inferior, Lost Days, Temporal Cleansing, Happy Hogan, Wan Shi Tong)
//! parsed to `Effect::PutOnTopOrBottom` with the "second from the top" position
//! DROPPED, so the "top" branch placed the creature FIRST in its owner's library.
//!
//! CR 401.7: "Nth from the top" is a specific library position. CR 608.2d: the
//! owner (not the caster) makes the choice. These tests drive the real cast ->
//! resolve -> `TopOrBottomChoice` -> `ChooseTopOrBottom` path.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, LibraryPosition};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TRICKSTERS_STRATAGEM: &str = "The owner of target creature an opponent controls puts it into their library second from the top or on the bottom. Then up to one target creature you control connives.";

/// Cast the spell at P1's creature (and connive P0's own) and pass priority until the owner's choice is
/// offered. Returns the runner, the tucked creature and P1's library fillers.
fn cast_until_choice(fillers: &[&str]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    cast_until_choice_with(fillers, true)
}

/// `connive_own`: supply the "up to one target creature you control" connive
/// target (true), or decline that optional slot (false).
fn cast_until_choice_with(
    fillers: &[&str],
    connive_own: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, fillers);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Trickster's Stratagem", false, TRICKSTERS_STRATAGEM)
        .id();
    let victim = scenario.add_creature(P1, "Opposing Creature", 2, 2).id();
    // The connive half targets the caster's own creature, so it draws from P0's
    // library and never disturbs the P1 library arrangement under test.
    let own_creature = scenario.add_creature(P0, "Own Creature", 1, 1).id();
    scenario.with_library_top(P0, &["Caster Filler"]);
    let mut runner = scenario.build();
    let filler_ids: Vec<ObjectId> = runner.state().library_of(P1).iter().copied().collect();
    assert_eq!(filler_ids.len(), fillers.len());

    // The parsed effect must carry the position, not drop it.
    let parsed = &runner.state().objects[&spell].abilities[0];
    assert!(
        matches!(
            &*parsed.effect,
            Effect::PutOnTopOrBottom {
                top_position: LibraryPosition::NthFromTop { n: 2 },
                ..
            }
        ),
        "second-from-the-top must be parsed, got {:?}",
        parsed.effect
    );

    let targets: Vec<ObjectId> = if connive_own {
        vec![victim, own_creature]
    } else {
        vec![victim]
    };
    runner.cast(spell).target_objects(&targets).commit();
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TopOrBottomChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve the spell");
    }
    (runner, victim, filler_ids)
}

fn library(runner: &GameRunner, seat: engine::types::player::PlayerId) -> Vec<ObjectId> {
    runner.state().library_of(seat).iter().copied().collect()
}

/// The OWNER (P1), not the caster (P0), chooses; the "top" branch lands the
/// creature SECOND from the top (index 1), under the former top card.
#[test]
fn owner_chooses_and_top_branch_is_second_from_the_top() {
    let (mut runner, victim, fillers) = cast_until_choice(&["Filler A", "Filler B", "Filler C"]);
    match &runner.state().waiting_for {
        WaitingFor::TopOrBottomChoice {
            player,
            object_id,
            top_position,
        } => {
            assert_eq!(*player, P1, "the creature's owner makes the choice");
            assert_eq!(*object_id, victim);
            assert_eq!(*top_position, LibraryPosition::NthFromTop { n: 2 });
        }
        other => panic!("expected TopOrBottomChoice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTopOrBottom { top: true })
        .expect("owner picks the top branch");
    assert_eq!(runner.state().objects[&victim].zone, Zone::Library);
    assert_eq!(
        library(&runner, P1),
        vec![fillers[0], victim, fillers[1], fillers[2]],
        "top branch = second from the top (index 1), not first"
    );
    // The interactive continuation (the connive half) must have finished.
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("finish the chain");
    }
    assert!(runner.state().stack.is_empty(), "chain fully resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no pending prompt after the chain, got {:?}",
        runner.state().waiting_for
    );
}

/// CHARACTERIZATION of a PRE-EXISTING defect, not of intended behaviour.
/// Declining the optional "up to one target creature you control" connive slot
/// leaves the connive sub-ability inheriting the parent's target list
/// (`targeting.rs` ~1212), and `effects/connive.rs:34-45` takes the FIRST object
/// target (`ability.targets.iter().find_map(TargetRef::Object)`) instead of
/// `targeting::resolved_targets(..)` as the sibling `put_on_top_or_bottom.rs:28`
/// does. So the tucked creature itself connives: its owner draws the card above
/// it and discards it, leaving the creature on TOP of the library - the exact
/// outcome "second from the top" exists to prevent. Not fixed here (separate
/// measurement); when connive.rs is fixed this assertion must flip to index 1.
#[test]
fn declined_connive_slot_currently_lets_the_tucked_creature_connive() {
    let (mut runner, victim, _) =
        cast_until_choice_with(&["Filler A", "Filler B", "Filler C"], false);
    runner
        .act(GameAction::ChooseTopOrBottom { top: true })
        .expect("owner picks the top branch");
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("finish the chain");
    }
    let lib = library(&runner, P1);
    assert_eq!(
        lib.iter().position(|id| *id == victim),
        Some(0),
        "KNOWN DEFECT: victim ends on top after the mis-targeted connive, library = {lib:?}"
    );
}

/// Reach-guard partner: the bottom branch still lands at the very bottom.
#[test]
fn bottom_branch_is_still_the_bottom() {
    let (mut runner, victim, fillers) = cast_until_choice(&["Filler A", "Filler B", "Filler C"]);
    runner
        .act(GameAction::ChooseTopOrBottom { top: false })
        .expect("owner picks the bottom branch");
    assert_eq!(
        library(&runner, P1),
        vec![fillers[0], fillers[1], fillers[2], victim]
    );
}

/// CR 401.7: with fewer than N cards in the library the card goes on the bottom
/// (an empty library simply receives it).
#[test]
fn second_from_the_top_in_an_empty_library_is_the_only_card() {
    let (mut runner, victim, _) = cast_until_choice(&[]);
    runner
        .act(GameAction::ChooseTopOrBottom { top: true })
        .expect("owner picks the top branch");
    assert_eq!(library(&runner, P1), vec![victim]);
}

/// Same class, Calim, Djinn Emperor: "When you discard ~, conjure a card named
/// Calim, Djinn Emperor into your library seventh from the top" dropped the
/// ordinal. The discard is staged as a hand -> graveyard move plus the
/// `Discarded` event (the activated "Calim's Breath" cannot be activated from
/// hand in this engine - a separate gap), then the real trigger is processed and
/// resolved off the stack. The conjured card must land at index 6.
const CALIM: &str = "Flying, ward {2}\nCalim's Breath \u{2014} {1}{U}, Discard Calim: Tap up to one target nonland permanent. Draw a card. Then you may exile two other cards named Calim, Djinn Emperor from your graveyard. When you do, return Calim from your graveyard to the battlefield tapped.\nWhen you discard Calim, conjure a card named Calim, Djinn Emperor into your library seventh from the top.";

#[test]
fn calim_discard_trigger_conjures_seventh_from_the_top() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9"]);
    let calim = scenario
        .add_creature_to_hand_from_oracle(P0, "Calim, Djinn Emperor", 5, 6, CALIM)
        .id();
    let mut runner = scenario.build();
    let before: Vec<ObjectId> = library(&runner, P0);

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), calim, Zone::Graveyard, &mut events);
    events.push(GameEvent::Discarded {
        player_id: P0,
        object_id: calim,
        source_id: None,
    });
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Calim's discard trigger must be on the stack"
    );
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve the trigger");
    }
    assert!(runner.state().stack.is_empty());

    let after = library(&runner, P0);
    let conjured: Vec<ObjectId> = after
        .iter()
        .copied()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(conjured.len(), 1, "exactly one Calim was conjured");
    assert_eq!(
        after.iter().position(|id| *id == conjured[0]),
        Some(6),
        "seventh from the top is index 6, library = {after:?}"
    );
    assert_eq!(&after[..6], &before[..6]);
    assert_eq!(&after[7..], &before[6..]);
}
