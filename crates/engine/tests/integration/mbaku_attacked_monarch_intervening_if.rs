//! M'Baku, Jabari Chieftain — both printed abilities.
//!
//! **Ability 1** — "At the beginning of your end step, if there is no monarch,
//! **target opponent** becomes the monarch." The designation must go to the
//! DECLARED TARGET. `Effect::BecomeMonarch` used to be a unit variant whose
//! resolver read `ability.controller`, so this crowned M'Baku's own controller
//! — the one player the clause exists to deny, and the player whose coronation
//! structurally disables ability 2 (an opponent could never become the monarch
//! off this card).
//!
//! **Ability 2** — "Whenever a creature attacks one of your
//! opponents, **if that player is the monarch**, that creature gets +1/+1 and
//! gains trample until end of turn."
//!
//! Before this change the intervening-if was dropped entirely
//! (`condition: null` plus a self-flagged `SwallowedClause/Condition_If`), so
//! the buff applied to every creature attacking any opponent regardless of
//! monarch status.
//!
//! Three independent failure directions are discriminated here:
//!   1. **Parser subject axis missing** — `parse_inner_condition` rejects
//!      "that player is the monarch", the condition stays `None`, and the buff
//!      applies unconditionally (Test 2 catches this).
//!   2. **Anaphor not rebound** — the condition parses as
//!      `IsMonarch { ScopedPlayer }`, which
//!      `targeting::extract_player_from_event` resolves to the ATTACKING
//!      player for an `AttackersDeclared` event, so nothing is ever buffed
//!      (Test 1's positive assertion catches this).
//!   3. **CR 508.5 anchor precedence wrong** — the source's own combat latch
//!      answers instead of the triggering attacker's defender, so the buff
//!      tracks whoever M'Baku is attacking rather than whoever the buffed
//!      creature is attacking (Test 1's two-attacker split and Test 4 catch
//!      this).
//!
//! CR references:
//!   - CR 508.5 / CR 508.5a: an ability referring to both an attacking creature
//!     and a defending player means the player THAT creature is attacking, and
//!     in multiplayer that player is determined individually per attacker.
//!   - CR 310.8d: a battle's protector, and a planeswalker's controller, are the
//!     defending player for an attack against them (Test 5's planeswalker).
//!   - CR 603.2 / CR 603.2c: the trigger fires once per matching attacker.
//!   - CR 603.4: the intervening-if is checked at fire time AND again as the
//!     ability resolves (Test 3).
//!   - CR 725.1: the monarch is a single-player designation.
//!   - CR 702.19a: trample.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

/// Verbatim Scryfall Oracle text — a paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const MBAKU_ORACLE: &str = "At the beginning of your end step, if there is no monarch, target opponent becomes the monarch.\n\
     Whenever a creature attacks one of your opponents, if that player is the monarch, that creature gets +1/+1 and gains trample until end of turn.";

struct Board {
    runner: GameRunner,
    mbaku: ObjectId,
    bears: ObjectId,
}

/// Three-player board: P0 controls M'Baku plus a vanilla 2/2.
fn board(monarch: Option<PlayerId>) -> Board {
    board_at(Phase::PreCombatMain, monarch)
}

/// [`board`] starting in `phase`. The end-step rows start POST-combat: the
/// scenario driver's phase advance only walks priority windows, so a pre-combat
/// start parks on the `DeclareAttackers` turn-based action instead of reaching
/// the end step.
fn board_at(phase: Phase, monarch: Option<PlayerId>) -> Board {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(phase);

    let mbaku = {
        let mut builder = scenario.add_creature(P0, "M'Baku, Jabari Chieftain", 4, 3);
        builder.from_oracle_text(MBAKU_ORACLE);
        builder.id()
    };
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner.state_mut().monarch = monarch;
    evaluate_layers(runner.state_mut());

    Board {
        runner,
        mbaku,
        bears,
    }
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = runner.state().objects.get(&id).expect("object must exist");
    (object.power, object.toughness)
}

fn has_trample(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object must exist")
        .keywords
        .contains(&Keyword::Trample)
}

// ---------------------------------------------------------------------------
// Ability 1 — "At the beginning of your end step, if there is no monarch,
// target opponent becomes the monarch."
// ---------------------------------------------------------------------------

/// Advance to P0's end step and answer the end-step trigger's target prompt
/// with `choice`. Returns the seat the prompt offered, so callers can assert on
/// the legality set as well as the outcome.
fn crown_via_end_step(runner: &mut GameRunner, choice: PlayerId) -> Vec<TargetRef> {
    runner.advance_to_end_step();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach-guard: the driver must actually reach the end step"
    );

    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the end-step trigger must prompt for its target; got {:?}\nstack={:?} monarch={:?}",
            runner.state().waiting_for,
            runner.stack_names(),
            runner.state().monarch,
        );
    };
    let legal = target_slots[selection.current_slot].legal_targets.to_vec();

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(choice)),
        })
        .expect("choosing the targeted opponent must be legal");
    runner.advance_until_stack_empty();
    legal
}

/// **The row that proves the fix.** CR 115.1 + CR 725.1: "target opponent
/// becomes the monarch" crowns the DECLARED TARGET.
///
/// Revert-failing: restore `Effect::BecomeMonarch` as a unit variant whose
/// resolver reads `ability.controller` and `state.monarch` is `Some(P0)` — the
/// first assertion flips. The `assert_ne!` is the same claim stated against the
/// exact shipped bug, so a future refactor that reintroduces a controller
/// fallback (rather than reverting wholesale) also fails here.
#[test]
fn mbaku_end_step_crowns_the_targeted_opponent_not_its_controller_cr_115_1() {
    let Board { mut runner, .. } = board_at(Phase::PostCombatMain, None);

    let legal = crown_via_end_step(&mut runner, P2);

    assert_eq!(
        runner.state().monarch,
        Some(P2),
        "the TARGETED opponent must become the monarch"
    );
    assert_ne!(
        runner.state().monarch,
        Some(P0),
        "the ability's controller must never be crowned by `target opponent \
         becomes the monarch` — that is the shipped bug this row pins"
    );

    // Discrimination guard: the prompt really offered more than one seat, so
    // the assertion above is about the CHOICE, not about a single forced
    // answer that any implementation would land on.
    assert!(
        legal.len() >= 2,
        "the target prompt must offer a real choice; got {legal:?}"
    );
    // CR 115.1: "target OPPONENT" — the controller is not a legal target. This
    // is the assertion that flips if the slot is built from a bare
    // `TargetFilter::Player` instead of the parsed opponent-scoped filter.
    assert!(
        !legal.contains(&TargetRef::Player(P0)),
        "CR 115.1: `target opponent` must not offer the controller; got {legal:?}"
    );
}

/// The same trigger, the OTHER opponent. Two rows with different answers are
/// what prove the resolver reads the target rather than any fixed seat
/// (`PlayerId(1)`, the first opponent, the active player, …).
#[test]
fn mbaku_end_step_crowns_whichever_opponent_was_targeted() {
    let Board { mut runner, .. } = board_at(Phase::PostCombatMain, None);

    crown_via_end_step(&mut runner, P1);

    assert_eq!(
        runner.state().monarch,
        Some(P1),
        "targeting P1 must crown P1, not the seat the other row crowned"
    );
}

/// CR 603.4: the printed intervening-if still gates the trigger — with a
/// monarch already seated, the end-step ability must not fire at all.
///
/// Reach-guarded: the two rows above prove the same board DOES prompt and crown
/// when the designation is vacant, so this negative cannot pass vacuously.
#[test]
fn mbaku_end_step_does_not_fire_while_a_monarch_exists_cr_603_4() {
    let Board { mut runner, .. } = board_at(Phase::PostCombatMain, Some(P1));

    runner.advance_to_end_step();
    // Reach-guard: without this the negative below is vacuous — the driver can
    // park before the end step and no trigger prompt would appear for reasons
    // that have nothing to do with the intervening-if.
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach-guard: the driver must actually reach the end step"
    );

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "`if there is no monarch` must suppress the trigger entirely; got {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().monarch,
        Some(P1),
        "the seated monarch must be untouched"
    );
}

// ---------------------------------------------------------------------------
// Ability 2 — the attack-trigger intervening-if.
// ---------------------------------------------------------------------------

/// **The row that proves the fix.** CR 508.5 + CR 603.2c: two creatures attack
/// two different opponents in one declaration. Only the one attacking the
/// MONARCH (P2) is buffed.
///
/// Revert-failing in three independent directions, all distinguishable:
///   - no anaphor rebind → the condition anchors on the ATTACKING player (P0),
///     who is not the monarch, so NEITHER creature is buffed;
///   - no CR 508.5 precedence fix → M'Baku's own latch (P1) answers for both
///     firings, so the BEARS are not buffed;
///   - no parser subject axis → the condition is absent, so M'BAKU is buffed
///     too.
#[test]
fn mbaku_buffs_only_the_creature_attacking_the_monarch() {
    let Board {
        mut runner,
        mbaku,
        bears,
    } = board(Some(P2));

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (mbaku, AttackTarget::Player(P1)),
            (bears, AttackTarget::Player(P2)),
        ])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, bears),
        (Some(3), Some(3)),
        "the creature attacking the monarch (P2) must get +1/+1"
    );
    assert!(
        has_trample(&runner, bears),
        "the creature attacking the monarch must gain trample (CR 702.19a)"
    );

    assert_eq!(
        power_toughness(&runner, mbaku),
        (Some(4), Some(3)),
        "M'Baku attacks P1, who is NOT the monarch — no buff"
    );
    assert!(
        !has_trample(&runner, mbaku),
        "M'Baku must not gain trample while attacking a non-monarch"
    );
}

/// CR 725.1: attacking a non-monarch opponent grants nothing.
///
/// Reach-guarded: the same declaration against the monarch DOES buff, so a
/// parse failure cannot make this negative pass vacuously.
#[test]
fn mbaku_no_buff_when_attacked_player_is_not_the_monarch() {
    // Reach-guard first: P2 is the monarch and Bears attacks P2 → buffed.
    let Board {
        mut runner, bears, ..
    } = board(Some(P2));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bears, AttackTarget::Player(P2))])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        power_toughness(&runner, bears),
        (Some(3), Some(3)),
        "reach-guard: the trigger DOES fire and buff when the attacked player is the monarch"
    );

    // Now the real negative: P1 is the monarch, but Bears attacks P2.
    let Board {
        mut runner, bears, ..
    } = board(Some(P1));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bears, AttackTarget::Player(P2))])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, bears),
        (Some(2), Some(2)),
        "attacking a non-monarch must not buff"
    );
    assert!(!has_trample(&runner, bears), "and must not grant trample");
}

/// CR 603.4: the intervening-if is checked AGAIN as the ability resolves. If
/// the monarch changes between declaration and resolution, the ability is
/// removed from the stack and does nothing.
///
/// This is the row that would fail if the condition had been lowered to the
/// declaration-time event qualifier `AttackTargetFilter::Monarch` instead of a
/// real intervening-if. Reach-guarded by Test 1, which proves the same
/// declaration buffs when the monarch is unchanged.
#[test]
fn mbaku_intervening_if_rechecked_at_resolution_cr_603_4() {
    let Board {
        mut runner, bears, ..
    } = board(Some(P2));

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bears, AttackTarget::Player(P2))])
        .expect("DeclareAttackers should succeed");

    // The trigger is on the stack; revoke the monarch designation before it
    // resolves.
    runner.state_mut().monarch = None;
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, bears),
        (Some(2), Some(2)),
        "CR 603.4: the resolution-time recheck must remove the ability"
    );
    assert!(!has_trample(&runner, bears));
}

/// CR 603.2: the source is an ordinary subject of its own observer trigger. When
/// M'Baku itself attacks the monarch it IS buffed — proving the per-attacker
/// event binding is not confused by the source being one of the attackers.
#[test]
fn mbaku_buffs_itself_when_it_attacks_the_monarch() {
    let Board {
        mut runner,
        mbaku,
        bears,
    } = board(Some(P1));

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (mbaku, AttackTarget::Player(P1)),
            (bears, AttackTarget::Player(P2)),
        ])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, mbaku),
        (Some(5), Some(4)),
        "M'Baku attacks the monarch (P1) and is itself a creature — CR 603.2"
    );
    assert!(has_trample(&runner, mbaku));

    assert_eq!(
        power_toughness(&runner, bears),
        (Some(2), Some(2)),
        "Bears attacks P2, who is not the monarch"
    );
    assert!(!has_trample(&runner, bears));
}
