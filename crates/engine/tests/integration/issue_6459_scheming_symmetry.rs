//! Scheming Symmetry — "Choose two target players." must require TWO DIFFERENT
//! players (CR 115.1b + CR 601.2c: the same player can't be chosen to fill more
//! than one of a single instruction's target slots).
//!
//! Regression for issue #6459: in a multiplayer game the same player could be
//! chosen for both slots. A modal "each mode must target a different player"
//! card already carries `DifferentTargetPlayers`, but a plain multi-target
//! player requirement had no constraint attached, so the runtime distinctness
//! authority never saw one to enforce.
//!
//! Two layers of proof:
//!  * parser — the parsed ability carries `DifferentTargetPlayers`;
//!  * runtime — after the first player is chosen, choosing that SAME player for
//!    the second slot is rejected, while a DIFFERENT player is accepted (the
//!    discriminating end-to-end behaviour).

use engine::game::scenario::GameScenario;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const SCHEMING: &str =
    "Choose two target players. Each of them searches their library for a card, \
then shuffles and puts that card on top.";

#[test]
fn scheming_symmetry_parses_distinct_player_constraint() {
    use engine::types::game_state::TargetSelectionConstraint;

    let parsed = engine::parser::parse_oracle_text(
        SCHEMING,
        "Scheming Symmetry",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("Scheming Symmetry lowers to a spell ability");
    assert!(
        ability
            .target_constraints
            .iter()
            .any(|c| matches!(c, TargetSelectionConstraint::DifferentTargetPlayers)),
        "a \"choose two target players\" requirement must carry \
         DifferentTargetPlayers (issue #6459); got {:?}",
        ability.target_constraints
    );
}

#[test]
fn scheming_symmetry_rejects_choosing_the_same_player_twice() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1, P2] {
        scenario.with_library_top(pid, &["Lib A", "Lib B"]);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scheming Symmetry", true, SCHEMING)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the sorcery must be accepted");

    // First slot: all three players are legal. Choose P1.
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a per-slot TargetSelection, got {}",
            runner.waiting_for_kind()
        );
    };
    let slot0 = &target_slots[selection.current_slot];
    for pid in [P0, P1, P2] {
        assert!(
            slot0.legal_targets.contains(&TargetRef::Player(pid)),
            "{pid:?} must be a legal first-slot target, slot = {slot0:?}"
        );
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choosing P1 for the first slot must succeed");

    // Second slot: choosing the ALREADY-CHOSEN player P1 must be rejected
    // (CR 115.1b), while the state stays on the same target slot.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "a second target slot must be surfaced, got {}",
        runner.waiting_for_kind()
    );
    let reselect_same = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Player(P1)),
    });
    assert!(
        reselect_same.is_err(),
        "CR 115.1b (issue #6459): choosing the already-chosen player P1 for the \
         second slot must be rejected"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "after the rejected reselection the second target slot must still be open"
    );

    // A DIFFERENT player (P2) is accepted, so the requirement is satisfiable —
    // proving the rejection is the distinctness rule, not a dead slot.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P2)),
        })
        .expect("choosing a different player (P2) for the second slot must succeed");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "with two distinct players chosen the spell must leave target selection, got {}",
        runner.waiting_for_kind()
    );
}
