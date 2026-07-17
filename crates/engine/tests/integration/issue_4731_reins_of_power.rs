//! Runtime regression for GH issue #4731 — Reins of Power reportedly gave the
//! CASTING player control of every creature on the board instead of the
//! printed two-way swap.
//!
//! Oracle (verified against the issue's own quoted text, confidence 0.90):
//! "Untap all creatures you control and all creatures target opponent
//! controls. You and that opponent each gain control of all creatures the
//! other controls until end of turn. Those creatures gain haste until end of
//! turn."
//!
//! Root cause (confirmed independently and by a repo collaborator's own
//! diagnosis in the issue thread): the "each gain control of all creatures
//! the other controls" clause collapsed into a single `Effect::GainControlAll`
//! whose population filter had NO controller anaphor (`controller: null`,
//! matching every creature regardless of owner) and whose resolver always
//! assigns the new controller to `ability.controller` — the caster. There was
//! no second, opposite-direction control change for the opponent's side.
//!
//! Fix: a dedicated combinator (`try_parse_symmetric_gain_control_all`)
//! recognizes this fixed "you and that/target opponent each gain control of
//! all creatures the other controls" idiom and composes TWO independently
//! correct mechanisms instead of one ambiguous one:
//!   - `GainControlAll { target: creatures the target opponent controls }` —
//!     the caster takes (byte-identical mechanism to the already-correct
//!     Hellkite Tyrant "gain control of all artifacts that player controls").
//!   - `GiveControlAll { target: creatures the caster controls, recipient:
//!     the target opponent }` — new mass counterpart of `GiveControl`; the
//!     opponent takes.
//!
//! This drives the REAL cast → target-selection → resolution pipeline (not a
//! hand-built `ResolvedAbility`) with each player controlling creatures the
//! OTHER doesn't, so a one-way "grab everything" regression and a "swap
//! nothing" regression are both distinguishable from the correct outcome.
//! Discriminating: revert either the `GiveControlAll` resolver or the parser
//! combinator and this fails — pre-fix, P0 ends up controlling ALL FOUR
//! creatures (P1 controls none) instead of a clean two-for-two swap.
//!
//! A second defect surfaced during testing, independent of the composition
//! above: `resolve_chain_body` deliberately flushes pending layers before a
//! sub-ability resolves (issue #2384, so a P/T-dependent sub-effect sees
//! current characteristics). That flush applies `GainControlAll`'s
//! control-change TCEs BEFORE `GiveControlAll` runs, so a live "creatures you
//! control" filter at that point would incorrectly also match the creatures
//! `GainControlAll` just took — collapsing the intended two-way swap right
//! back into a one-way grab, just shifted to the second half. Fixed via
//! `GameState::last_gained_control_object_ids`: `resolve_all` stamps the
//! objects it just took, and `resolve_give_all` excludes them from its own
//! filter — see the doc on that field and on `resolve_give_all` for the full
//! mechanics.
//!
//! NOT fixed here: "Those creatures gain haste until end of turn" (the
//! oracle's third sentence) resolves to an `AddKeyword` TCE targeting the two
//! PLAYERS, not the four creatures that changed control — a separate,
//! pre-existing "those creatures" scoping bug, unrelated to the reported
//! one-way-grab issue. See the note on the test below.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const REINS_OF_POWER: &str = "Untap all creatures you control and all creatures target \
     opponent controls. You and that opponent each gain control of all creatures the \
     other controls until end of turn. Those creatures gain haste until end of turn.";

fn controller_of(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects.get(&id).unwrap().controller
}

#[test]
fn reins_of_power_swaps_control_both_ways_not_a_one_sided_grab() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Each player controls TWO creatures the other doesn't — this is what
    // distinguishes a genuine two-way swap from the reported one-way grab
    // (which would leave P0 controlling all four and P1 controlling none).
    let p0_bear = scenario.add_vanilla(P0, 2, 2);
    let p0_elephant = scenario.add_vanilla(P0, 3, 3);
    let p1_goblin = scenario.add_vanilla(P1, 1, 1);
    let p1_ogre = scenario.add_vanilla(P1, 4, 4);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Reins of Power", false, REINS_OF_POWER)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free instant must succeed");

    // Exactly one target slot: "target opponent" is declared once and reused
    // (as "that opponent") by every later clause in the same sentence.
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(
                target_slots.len(),
                1,
                "Reins of Power declares exactly one target (the opponent), got {}",
                target_slots.len()
            );
            assert!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Player(P1)),
                "the opponent must be a legal target"
            );
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                })
                .expect("targeting the opponent must succeed");
        }
        other => panic!("expected a single TargetSelection prompt, got {other:?}"),
    }

    runner.pass_both_players();
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    // CR 613.1b + CR 613.9: a genuine two-way swap — P0's creatures are now
    // P1's, and P1's creatures are now P0's. Pre-fix this reproduced as ALL
    // FOUR creatures ending up under P0's control (the one-way grab the issue
    // reports), so every one of these four assertions is discriminating.
    assert_eq!(
        controller_of(&runner, p0_bear),
        P1,
        "P1 must gain control of the bear (it was P0's)"
    );
    assert_eq!(
        controller_of(&runner, p0_elephant),
        P1,
        "P1 must gain control of the elephant (it was P0's)"
    );
    assert_eq!(
        controller_of(&runner, p1_goblin),
        P0,
        "P0 must gain control of the goblin (it was P1's) — not stay with P1, and not \
         ALSO stay with P0 having taken it while keeping its own creatures too"
    );
    assert_eq!(
        controller_of(&runner, p1_ogre),
        P0,
        "P0 must gain control of the ogre (it was P1's)"
    );

    // NOTE: "Those creatures gain haste until end of turn" (the third
    // sentence) is NOT asserted here. Investigating it surfaced a separate,
    // pre-existing defect unrelated to the reported one-way-grab bug: the
    // haste grant ends up as an `AddKeyword` TCE targeting the two PLAYERS
    // (`SpecificPlayer`), not the four creatures that changed control — "those
    // creatures" isn't resolving to a creature population at all. That's a
    // distinct parser/scoping bug, out of scope for this fix; left for a
    // follow-up issue rather than bundled in here.
}
