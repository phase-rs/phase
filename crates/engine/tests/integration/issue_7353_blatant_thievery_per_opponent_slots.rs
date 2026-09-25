//! Issue #7353 — Blatant Thievery: "Says to choose 6 targets and won't let you
//! target anything."
//!
//! > Blatant Thievery {4}{U}{U}{U} Sorcery
//! > For each opponent, gain control of target permanent that player controls.
//!
//! WHY THIS TEST EXISTS. The functional defect was fixed by #7704 (`d0a41701e`),
//! which announces the pinned binder on the controller's behalf. Two runtime
//! tests pin that fix, but BOTH drive a *triggered* ability's targeting
//! (Diluvian Primordial, `WaitingFor::TriggerTargetSelection`). Blatant Thievery
//! is a sorcery, so its cast takes the SPELL path (`WaitingFor::TargetSelection`)
//! — and no test drove a per-opponent fanout spell cast end to end. This is that
//! test.
//!
//! THE SHAPE, MEASURED RATHER THAN ASSUMED. A per-opponent fanout builds TWO
//! slots per opponent: a pinned binder naming that opponent, then the permanent
//! slot scoped to them. CR 115.10a — being *affected* by a spell does not make
//! you a target; only what the word "target" names is one, and here "target"
//! attaches to the permanent. CR 115.1a — an instant or sorcery spell is
//! targeted only through that phrase. So the binder is engine-announced and six
//! slots is CORRECT. An earlier revision of this test asserted three slots and
//! was simply wrong about the contract.
//!
//! WHAT MEASUREMENT KILLED, recorded so it is not re-derived:
//!   * the triage's "a single unbound `target player controls permanent` filter
//!     yields an empty legal set" — REFUTED; each object slot offers exactly its
//!     own opponent's permanents, and never the caster's.
//!   * "the slots are empty" — REFUTED; every slot has legal targets.
//!   * a ONE-permanent-per-opponent fixture — useless here, and the reason an
//!     earlier run was unreadable: every slot then holds a single candidate,
//!     auto-selection takes them all, no prompt is raised, and an untargeted
//!     sweep is indistinguishable from correct play. TWO permanents per opponent
//!     is what separates them, which is why the fixture is shaped that way.
//!
//! The discriminator is therefore the CURSOR, not the slot count: an
//! engine-announced binder leaves `selection.current_slot` at 1 with the binder
//! already recorded, whereas a prompted binder would leave it at 0 offering a
//! player. Each permanent slot's legal set must match the opponent bound to it.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

// Verbatim Oracle text (Scryfall, 2026-09-18).
const BLATANT_THIEVERY: &str =
    "For each opponent, gain control of target permanent that player controls.";

/// Cast Blatant Thievery in a four-player game and capture the target-selection
/// slots exactly as the engine offers them.
///
/// Four players means three opponents and three permanent targets. The engine
/// represents each target with a binder and a permanent-selection slot, so the
/// slot count becomes a measurement rather than a guess.
#[test]
fn blatant_thievery_binds_each_target_slot_to_its_own_opponent() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..7)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, Vec::new()))
            .collect(),
    );

    // TWO permanents per opponent, not one. This is load-bearing, and a
    // one-apiece board is why the first run of this test was unreadable.
    //
    // With a single permanent per opponent, "the engine ignored targeting and
    // swept every opponent's permanents" and "the engine auto-selected the only
    // legal target per slot, so it had nothing to prompt for" produce the
    // IDENTICAL end board — all three stolen, no prompt. The fixture could not
    // tell a serious bug from correct behaviour.
    //
    // A second permanent per opponent breaks that degeneracy: auto-selection is
    // no longer available (two candidates is a real choice, so a prompt MUST be
    // raised), while an untargeted fan-out still takes all six. The caster's own
    // bear stays the control — "that player controls" must never offer it, since
    // P0 is not an opponent.
    let mine = scenario.add_creature(P0, "My Bear", 2, 2).id();
    let p1_first = scenario.add_creature(P1, "P1 Bear A", 2, 2).id();
    let p1_second = scenario.add_creature(P1, "P1 Bear B", 2, 2).id();
    let p2_first = scenario.add_creature(P2, "P2 Bear A", 2, 2).id();
    let p2_second = scenario.add_creature(P2, "P2 Bear B", 2, 2).id();
    let p3_first = scenario.add_creature(P3, "P3 Bear A", 2, 2).id();
    let p3_second = scenario.add_creature(P3, "P3 Bear B", 2, 2).id();

    let thievery = scenario
        .add_spell_to_hand_from_oracle(P0, "Blatant Thievery", false, BLATANT_THIEVERY)
        .id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: thievery,
            card_id: runner.state().objects[&thievery].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach-guard: Blatant Thievery must be castable with seven blue mana");

    // Walk to the target-selection prompt, recording the trail so an unexpected
    // state reports the real flow instead of failing opaquely.
    let mut trail = Vec::new();
    let mut slots = None;
    for _ in 0..12 {
        let waiting = runner.state().waiting_for.clone();
        trail.push(format!("{waiting:?}").chars().take(60).collect::<String>());
        match waiting {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                // The CURSOR is the measurement, not the slot count: six slots
                // are the intended shape (a pinned binder plus a real object
                // slot per opponent), so only `selection` can distinguish an
                // engine-announced binder from one the caster is prompted for.
                slots = Some((target_slots, selection));
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            other => panic!("unexpected prompt before targeting: {other:?}; trail: {trail:?}"),
        }
    }

    let slots = slots.unwrap_or_else(|| {
        // No prompt at all is a distinct shape from "prompt with empty slots", and
        // the difference matters: an engine that computes zero slots may simply
        // never ask. Report enough state to tell apart "resolved doing nothing",
        // "still on the stack", and "took control with no targeting".
        let state = runner.state();
        let opponents_bears = [
            ("P1-A", p1_first),
            ("P1-B", p1_second),
            ("P2-A", p2_first),
            ("P2-B", p2_second),
            ("P3-A", p3_first),
            ("P3-B", p3_second),
        ];
        let controllers: Vec<String> = opponents_bears
            .iter()
            .map(|(label, id)| {
                format!(
                    "{label}={:?}",
                    state.objects.get(id).map(|obj| obj.controller)
                )
            })
            .collect();
        // With two permanents per opponent, the STOLEN COUNT is what separates
        // the candidate explanations — that is the whole reason the fixture is
        // shaped this way, so report it rather than leaving it to be eyeballed.
        let stolen = opponents_bears
            .iter()
            .filter(|(_, id)| state.objects.get(id).map(|obj| obj.controller) == Some(P0))
            .count();
        panic!(
            "reach-guard: casting never reached a target-selection prompt.\n\
             trail: {trail:?}\n\
             spell zone: {:?}   stack depth: {}\n\
             caster {:?} now controls {stolen} of the 6 opponent permanents: {controllers:?}\n\
             reading: 6 => UNTARGETED FAN-OUT (targeting skipped entirely);\n\
             \x20        3 => one taken per opponent with no prompt offered;\n\
             \x20        0 => the spell resolved without doing anything.",
            state.objects.get(&thievery).map(|obj| obj.zone),
            state.stack.len(),
            P0,
        )
    });

    let (slots, selection) = slots;

    // SIX slots is the engine's representation of three permanent targets. The
    // fanout builds a pinned *binder* slot (the opponent, affected but NOT
    // targeted) followed by the real permanent slot scoped to that opponent.
    //
    // So the slot COUNT cannot answer the question this test exists to ask.
    // The cursor can: a binder the engine announces on the caster's behalf
    // leaves `current_slot` past it, while a binder the caster is prompted for
    // leaves the cursor sitting on slot 0. That is the whole measurement.
    assert_eq!(
        slots.len(),
        6,
        "3 opponents x (binder + permanent); got {:?}",
        slots.iter().map(|s| &s.legal_targets).collect::<Vec<_>>()
    );

    let binder = |index: usize, opponent: PlayerId| {
        assert_eq!(
            slots[index].legal_targets,
            vec![TargetRef::Player(opponent)],
            "slot {index} is the pinned binder for {opponent:?} and has exactly one legal value"
        );
        assert!(
            !slots[index].optional,
            "slot {index} binder is not optional"
        );
    };
    binder(0, P1);
    binder(2, P2);
    binder(4, P3);

    let objects_of = |index: usize| {
        assert_eq!(
            slots[index].legal_targets.len(),
            2,
            "slot {index} has exactly two permanent choices"
        );
        let mut ids: Vec<ObjectId> = slots[index]
            .legal_targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .collect();
        ids.sort();
        ids
    };
    let sorted_pair = |a: ObjectId, b: ObjectId| {
        let mut pair = vec![a, b];
        pair.sort();
        pair
    };

    // Each object slot is scoped to its OWN opponent — the binding the triage
    // claimed was missing. A slot offering all six would be the unbound-filter
    // shape; one offering the caster's bear would ignore "that player".
    assert_eq!(objects_of(1), sorted_pair(p1_first, p1_second));
    assert_eq!(objects_of(3), sorted_pair(p2_first, p2_second));
    assert_eq!(objects_of(5), sorted_pair(p3_first, p3_second));
    for index in [1, 3, 5] {
        assert!(
            !objects_of(index).contains(&mine),
            "slot {index} offers the CASTER's permanent, but \"that player\" is the opponent \
             being iterated — P0 is never their own opponent"
        );
    }

    // THE DISCRIMINATOR (caveat #1 on issue #7353): the binder must already be
    // announced, so the first decision the caster faces is a real one. If the
    // spell path failed to autofill, `current_slot` would be 0 and
    // `current_legal_targets` would offer a player.
    assert_eq!(
        selection.current_slot, 1,
        "the pinned binder is announced by the engine, not prompted for"
    );
    assert_eq!(
        selection.selected_slots,
        vec![Some(TargetRef::Player(P1))],
        "the auto-announced binder lands in the walk exactly as a click would"
    );
    assert_eq!(
        selection.current_legal_targets, slots[1].legal_targets,
        "the first prompt is P1's permanent slot"
    );
    assert!(
        !selection
            .current_legal_targets
            .iter()
            .any(|target| matches!(target, TargetRef::Player(_))),
        "CR 115.10a: an affected-but-untargeted opponent is never offered as a choice"
    );

    // Walk the three REAL choices, taking each opponent's first bear. Between
    // picks the cursor must skip the next binder and land on the next object
    // slot, scoped to that opponent alone — this is where cross-binding would
    // show up.
    let expect_cursor = |runner: &engine::game::scenario::GameRunner,
                         slot: usize,
                         want: Vec<ObjectId>| {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                assert_eq!(selection.current_slot, slot, "walk lands on slot {slot}");
                assert_eq!(
                    selection.current_legal_targets.len(),
                    want.len(),
                    "slot {slot} has only the expected permanent choices"
                );
                let mut got: Vec<ObjectId> = selection
                    .current_legal_targets
                    .iter()
                    .filter_map(|target| match target {
                        TargetRef::Object(id) => Some(*id),
                        TargetRef::Player(_) => None,
                    })
                    .collect();
                got.sort();
                assert_eq!(got, want, "no cross-binding at slot {slot}");
            }
            other => panic!("expected the next permanent prompt, got {other:?}"),
        }
    };

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(p1_first)),
        })
        .expect("P1's permanent must be selectable without announcing P1");
    expect_cursor(&runner, 3, sorted_pair(p2_first, p2_second));

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(p2_first)),
        })
        .expect("P2's permanent must be selectable without announcing P2");
    expect_cursor(&runner, 5, sorted_pair(p3_first, p3_second));

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(p3_first)),
        })
        .expect("P3's permanent must be selectable without announcing P3");

    runner.advance_until_stack_empty();

    // Resolution: exactly the three CHOSEN permanents change hands. The
    // unchosen halves are the negative control — without them this test would
    // still pass under an untargeted fan-out that swept all six.
    let controller_of = |id: ObjectId| runner.state().objects.get(&id).map(|obj| obj.controller);
    for (label, id) in [("P1", p1_first), ("P2", p2_first), ("P3", p3_first)] {
        assert_eq!(
            controller_of(id),
            Some(P0),
            "{label}'s CHOSEN permanent must be under the caster's control after resolution"
        );
    }
    for (label, id, owner) in [
        ("P1", p1_second, P1),
        ("P2", p2_second, P2),
        ("P3", p3_second, P3),
    ] {
        assert_eq!(
            controller_of(id),
            Some(owner),
            "{label}'s UNCHOSEN permanent must stay put — one target per opponent, not a sweep"
        );
    }
    assert_eq!(controller_of(mine), Some(P0), "the caster keeps their own");
}
