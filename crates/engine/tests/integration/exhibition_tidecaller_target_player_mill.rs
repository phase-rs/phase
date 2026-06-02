//! Reproduction for issue #1373 — Exhibition Tidecaller.
//!
//! Oracle:
//!   "Opus — Whenever you cast an instant or sorcery spell, target player mills
//!    three cards. If five or more mana was spent to cast that spell, that
//!    player mills ten cards instead."
//!
//! Reported symptom: when you cast a spell with mana value 5+, the card mills
//! YOU for ten instead of milling the chosen target player.
//!
//! CR 603.3d + CR 601.2c: a "you cast … target player …" trigger's controller
//! restriction ("you cast") gates WHO must cast, while "target player" is the
//! trigger's chosen target — the milled player is the chosen target, never the
//! caster.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const TIDECALLER: &str = "Opus — Whenever you cast an instant or sorcery spell, \
     target player mills three cards. If five or more mana was spent to cast that \
     spell, that player mills ten cards instead.";

fn library_count(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .expect("player exists")
}

fn graveyard_count(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.graveyard.len())
        .expect("player exists")
}

/// mv < 5 path: cast a 0-cost instant. The primary "target player mills three"
/// must mill the CHOSEN target player (P1), never the caster (P0).
#[test]
fn tidecaller_mv_below_5_mills_chosen_target_three() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Both players need libraries to mill from.
    for pid in [P0, P1] {
        for _ in 0..15 {
            scenario.add_card_to_library_top(pid, "Lib Card");
        }
    }

    // Tidecaller on P0's battlefield, parsed from its real Oracle text.
    scenario.add_creature_from_oracle(P0, "Exhibition Tidecaller", 0, 2, TIDECALLER);

    // A 0-cost instant in P0's hand to trigger Tidecaller (mv 0 < 5).
    let spell = scenario
        .add_spell_to_hand(P0, "Cheap Bolt", true)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    let lib_p0_before = library_count(&runner, P0);
    let lib_p1_before = library_count(&runner, P1);

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
        })
        .expect("casting a 0-cost instant must succeed");
    runner.advance_until_stack_empty();

    // The trigger must surface a target-player selection — choose the opponent.
    match runner.state().waiting_for.clone() {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            assert!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Player(P1)),
                "opponent must be a legal target for 'target player mills'"
            );
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                })
                .expect("targeting the opponent must succeed");
            runner.advance_until_stack_empty();
        }
        other => panic!("expected TriggerTargetSelection from Tidecaller, got {other:?}"),
    }

    assert_eq!(
        lib_p0_before - library_count(&runner, P0),
        0,
        "caster (P0) must NOT be milled — they are not the chosen target"
    );
    assert_eq!(
        lib_p1_before - library_count(&runner, P1),
        3,
        "chosen target (P1) must be milled three"
    );
    assert_eq!(
        graveyard_count(&runner, P1),
        3,
        "P1's milled cards land in graveyard"
    );
}

/// mv >= 5 path: cast a 5-mana sorcery. The "instead" clause must mill the
/// CHOSEN target player (P1) ten cards — never the caster (P0).
#[test]
fn tidecaller_mv_5_or_more_mills_chosen_target_ten_not_caster() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    for pid in [P0, P1] {
        for _ in 0..15 {
            scenario.add_card_to_library_top(pid, "Lib Card");
        }
    }

    scenario.add_creature_from_oracle(P0, "Exhibition Tidecaller", 0, 2, TIDECALLER);

    // Five colorless mana so casting a {5} sorcery spends exactly five mana.
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| {
                ManaUnit::new(
                    ManaType::Colorless,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );

    let spell = scenario
        .add_spell_to_hand(P0, "Big Spell", false)
        .with_mana_cost(ManaCost::generic(5))
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    let lib_p0_before = library_count(&runner, P0);
    let lib_p1_before = library_count(&runner, P1);

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
        })
        .expect("casting a {5} sorcery must succeed");

    // Drive cast/target prompts to completion.
    let mut targeted = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Player(P1)),
                    "opponent must be a legal target for the Tidecaller trigger"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("targeting the opponent must succeed");
                targeted = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::ManaPayment { .. } => {
                // Auto-pay from the floating pool if a payment prompt appears.
                runner.advance_until_stack_empty();
            }
            other => panic!("unexpected waiting state during Big Spell cast: {other:?}"),
        }
    }
    assert!(targeted, "the trigger must have surfaced target selection");
    runner.advance_until_stack_empty();

    assert_eq!(
        lib_p0_before - library_count(&runner, P0),
        0,
        "caster (P0) must NOT be milled by the 'instead' clause"
    );
    assert_eq!(
        lib_p1_before - library_count(&runner, P1),
        10,
        "chosen target (P1) must be milled TEN by the mv>=5 'instead' clause"
    );
}
