//! Alchemist's Gift -- a pump compounded with a modal keyword grant.
//!
//! Oracle:
//!   Target creature gets +1/+1 and gains your choice of deathtouch or lifelink
//!   until end of turn.
//!
//! Surface parser near-miss (coverage gap). `try_split_pump_compound` peels the
//! pump half ("gets +1/+1") off the trailing grant. A fixed "and gains trample"
//! collapses into a single `ContinuousModification` (Mortal's Resolve), but a
//! two-branch "your choice of X or Y" cannot -- so the remainder was handed to
//! the general effect-chain parser, which does not recognize the modal grant,
//! leaving an `Unimplemented` sub_ability. The standalone targeted modal grant
//! (Orcish Medicine) and the self pump+modal (Argivian Avenger) both parse;
//! only the *targeted pump + modal* did not. The fix routes the remainder
//! through the same `parse_keyword_choice_grant` / `keyword_choice_branch`
//! builders the standalone modal grant uses, riding the pump as a `ChooseOneOf`
//! sub_ability keyed to the pumped creature (`ParentTarget`).
//!
//! This drives the REAL cast -> resolve -> `ChooseOneOfBranch` pipeline and
//! FAILS on `main`: the grant parses to `Unimplemented`, so no branch surfaces
//! and no keyword is granted.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const ALCHEMISTS_GIFT_ORACLE: &str = "Target creature gets +1/+1 and gains your \
     choice of deathtouch or lifelink until end of turn.";

/// Drive the engine forward until it pauses on the modal branch choice or a
/// terminal state, passing priority and declaring nothing so resolution runs.
fn advance_to_choice(runner: &mut GameRunner) {
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseOneOfBranch { .. } => return,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Casting Alchemist's Gift at a vanilla creature must pump it +1/+1 AND offer a
/// two-branch "your choice of deathtouch or lifelink" grant; resolving the
/// chosen branch grants ONLY that keyword to the pumped creature.
#[test]
fn alchemists_gift_grants_the_chosen_keyword_to_the_pumped_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Non-empty libraries so an unfixed run does not deck out before the
    // ChooseOneOfBranch assertion has a chance to observe the missing branch.
    for pid in [P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Alchemist Gift", true, ALCHEMISTS_GIFT_ORACLE)
        .id();
    // A vanilla 2/2: any deathtouch/lifelink it has after resolution can only
    // have come from the modal grant.
    let target = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&target].has_keyword(&Keyword::Deathtouch),
        "precondition: the vanilla creature has no deathtouch"
    );
    assert!(
        !runner.state().objects[&target].has_keyword(&Keyword::Lifelink),
        "precondition: the vanilla creature has no lifelink"
    );

    runner.cast(spell).target_object(target).commit();
    advance_to_choice(&mut runner);

    // The modal grant must surface as a two-branch choice made by the spell's
    // controller. On `main` the grant parsed to `Unimplemented`, so no branch
    // ever appeared here.
    let deathtouch_index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            player, branches, ..
        } => {
            assert_eq!(*player, P0, "the spell's controller (P0) makes the choice");
            assert_eq!(
                branches.len(),
                2,
                "deathtouch-or-lifelink is a two-branch choice"
            );
            branches
                .iter()
                .position(|b| {
                    b.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains("deathtouch"))
                })
                .expect("a gain-Deathtouch branch must exist")
        }
        other => panic!("expected a ChooseOneOfBranch for the modal keyword grant, got {other:?}"),
    };

    runner
        .act(GameAction::ChooseBranch {
            index: deathtouch_index,
        })
        .expect("resolving the deathtouch branch must succeed");

    // End-to-end: the pumped creature gains ONLY the chosen keyword.
    let obj = &runner.state().objects[&target];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(3), Some(3)),
        "the pump half must remain a +1/+1 effect until end of turn"
    );
    assert!(
        obj.has_keyword(&Keyword::Deathtouch),
        "the chosen keyword (deathtouch) must be granted to the pumped creature"
    );
    assert!(
        !obj.has_keyword(&Keyword::Lifelink),
        "the unchosen keyword (lifelink) must NOT be granted"
    );
}
