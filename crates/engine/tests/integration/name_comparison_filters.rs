//! CR 201.2a: the printed name-comparison relative clause
//! `<filter> that (doesn't|does not|don't|do not) (have|has) the same name as
//! <reference>` in target/search filter positions.
//!
//! This file owns the SAGA-CHAPTER targeting row of that class — The
//! Apprentice's Folly — because the existing Saga-chapter drivers are
//! module-private to their own test files, so this row carries its own park
//! and chapter driver below. Its siblings live with the cards whose plumbing
//! they share:
//! Curse of Misfortunes' search rows are in
//! `issue_5290_curse_misfortunes_upkeep.rs` and Yenna's activated-ability rows
//! are in `s07_b3_target_identity.rs`, so the shared resolve/search/activation
//! flow is reused rather than re-implemented here.
//!
//! Oracle: `I, II — Choose target nontoken creature you control that doesn't
//! have the same name as a token you control. Create a token that's a copy of
//! it, except it isn't legendary, is a Reflection in addition to its other
//! types, and has haste.`
//!
//! Before the fix the relative clause was dropped from the chapter's stored
//! target filter (`Typed{[Creature], You, [NonToken]}`), so a nontoken creature
//! sharing a name with a token you control was offered as a legal target.

use engine::game::keywords::has_keyword;
use engine::game::morph::apply_face_down_creature_characteristics;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{FaceDownProfile, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::ObjectId;

/// Verbatim MTGJSON Oracle text. The reminder line is part of the printed text
/// and carries the CR 714.3a lore-counter entry behavior the parser keys on.
const FOLLY: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II — Choose target nontoken creature you control that doesn't have the same name as a token you control. Create a token that's a copy of it, except it isn't legendary, is a Reflection in addition to its other types, and has haste.\nIII — Sacrifice all Reflections you control.";

fn lore_count(runner: &GameRunner, saga_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&saga_id)
        .and_then(|obj| obj.counters.get(&CounterType::Lore).copied())
        .unwrap_or(0)
}

/// Parks the runner at the beginning of P0's own turn so the driver below reaches
/// P0's precombat main — the phase whose CR 714.3c turn-based action puts the
/// Saga's lore counter on it.
///
/// `issue_5978_roar_chapter_ii_creature_mana.rs` parks at `Phase::End` instead,
/// because it needs the NEXT P0 precombat main after a Saga that already carries
/// a lore counter. In a two-player game that park crosses P1's turn first, so it
/// lands on P1's precombat main (measured: `phase=PreCombatMain active=P1`,
/// lore 0) and cannot observe P0's first counter. Parking at the same turn's
/// Untap keeps the driver inside P0's turn and reaches the identical
/// CR 714.3c milestone without crossing another player's turn. Self-contained
/// because every Saga helper in that file is module-private.
fn park_at_p0_untap(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::Untap;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

fn token_count_named(
    runner: &GameRunner,
    name: &str,
    controller: engine::types::player::PlayerId,
) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token && obj.name == name && obj.controller == controller)
        .count()
}

/// Drives CR 714.3c (add a lore counter as P0's precombat main phase begins) and
/// the chapter's `CounterAdded` trigger to the chapter's target prompt, then
/// returns the legal targets of the announced slot.
///
/// The driver is phase-level rather than prompt-level so it does not depend on
/// how many priority windows the turn scaffold opens between the park and the
/// chapter: it advances toward the precombat main whenever the stack is empty
/// and otherwise passes priority to let the queued chapter trigger ask for its
/// target.
fn reach_chapter_target_selection(runner: &mut GameRunner, saga_id: ObjectId) -> Vec<TargetRef> {
    park_at_p0_untap(runner);
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                // CR 714.3c reach-guard: the prompt can only exist because the
                // precombat main added a lore counter.
                assert!(
                    lore_count(runner, saga_id) >= 1,
                    "the chapter trigger must come from an added lore counter (CR 714.3c)"
                );
                return target_slots
                    .get(selection.current_slot)
                    .map(|slot| slot.legal_targets.clone())
                    .unwrap_or_default();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().phase != Phase::PreCombatMain {
                    runner.advance_to_phase(Phase::PreCombatMain);
                } else {
                    runner.pass_both_players();
                }
            }
            other => panic!("unexpected window while driving the Saga chapter: {other:?}"),
        }
    }
    panic!(
        "the chapter trigger never asked for a target: phase={:?} active={:?} waiting_for={:?} lore={}",
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for,
        lore_count(runner, saga_id)
    );
}

#[test]
fn apprentices_folly_chapter_target_excludes_only_your_token_names() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let saga = scenario
        .add_creature(P0, "The Apprentice's Folly", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(FOLLY)
        .id();

    // P0: a nontoken "Alpha" beside a token "Alpha"; a LEGENDARY nontoken
    // "Beta"; and a second uniquely named nontoken creature "Zeta" so the
    // narrowed filter still leaves TWO legal targets and the engine cannot
    // auto-announce a forced unique assignment (CR 601.2c).
    let alpha_nontoken = scenario.add_creature(P0, "Alpha", 2, 2).id();
    let alpha_token = scenario.add_creature(P0, "Alpha", 2, 2).id();
    let beta = scenario.add_creature(P0, "Beta", 2, 2).as_legendary().id();
    scenario.add_creature(P0, "Zeta", 2, 2);
    // P1's token "Beta" — NOT "a token you control", so it must not constrain.
    let opponent_beta_token = scenario.add_creature(P1, "Beta", 2, 2).id();

    let filler = ["Plains"; 10];
    scenario.with_library_top(P0, &filler);
    scenario.with_library_top(P1, &filler);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&alpha_token).unwrap().is_token = true;
        state
            .objects
            .get_mut(&opponent_beta_token)
            .unwrap()
            .is_token = true;
        // The scenario builder seeds objects directly and never runs the ETB
        // replacement, so the CR 714.3a entry counter is not present: start from
        // zero and let CR 714.3c add the Saga's first lore counter below, which
        // is what fires chapter I.
        state
            .objects
            .get_mut(&saga)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 0);
    }

    let legal = reach_chapter_target_selection(&mut runner, saga);
    // Discriminating: at base the target filter carries no name comparison, so
    // the legal set is [Alpha (nontoken), Beta, Zeta]. A reference population
    // that ignored the "you control" scope would drop Beta too (P1 controls a
    // token named Beta), leaving [Zeta].
    let mut legal_names: Vec<String> = legal
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => runner.state().objects[id].name.clone(),
            other => panic!("expected object targets, got {other:?}"),
        })
        .collect();
    legal_names.sort();
    assert_eq!(
        legal_names,
        vec!["Beta", "Zeta"],
        "a nontoken creature sharing a name with a token YOU control is illegal; \
         a token an opponent controls does not constrain the comparison (CR 201.2a)"
    );

    // Positive reach-guard: the chapter resolves the copy of the legal target.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(beta)),
        })
        .expect("choose Beta");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "the chapter ability must finish resolving, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !runner.state().objects[&alpha_nontoken].is_token,
        "the excluded creature stays an ordinary permanent"
    );

    // The copy payload: the source is legendary, the token is not — which is
    // what makes the printed exception do work (reverting it would leave a
    // legendary token and the legend rule would resolve the duplicate away).
    let source_beta = &runner.state().objects[&beta];
    assert!(
        source_beta
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        "the copied creature must be legendary for the exception to matter, got {:?}",
        source_beta.card_types.supertypes
    );
    assert_eq!(
        token_count_named(&runner, "Beta", P0),
        1,
        "the chapter must create one copy token of the chosen creature"
    );
    let copy_id = runner
        .state()
        .objects
        .iter()
        .find(|(_, obj)| obj.is_token && obj.name == "Beta" && obj.controller == P0)
        .map(|(id, _)| *id)
        .expect("the copy token is on the battlefield");
    let copy = &runner.state().objects[&copy_id];
    assert!(
        !copy.card_types.supertypes.contains(&Supertype::Legendary),
        "the copy is not legendary, got {:?}",
        copy.card_types.supertypes
    );
    assert!(
        copy.card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Reflection"),
        "the copy is a Reflection in addition to its other types, got {:?}",
        copy.card_types.subtypes
    );
    assert!(has_keyword(copy, &Keyword::Haste), "the copy has haste");
}

/// CR 201.2a + CR 708.2a: "doesn't have the same name as" is the literal
/// negation of same-name. A face-down creature has no name ("no text, no name,
/// no subtypes, and no mana cost"), so it does not have the same name as any
/// token its controller controls and remains a legal Folly target even when the
/// reference population is non-empty. The named nontoken `Alpha` beside a token
/// `Alpha` is illegal and proves the comparison is actually being evaluated;
/// dropping the clause would make it legal too.
#[test]
fn apprentices_folly_chapter_target_accepts_a_nameless_face_down_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let saga = scenario
        .add_creature(P0, "The Apprentice's Folly", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(FOLLY)
        .id();

    // Reference population: a token named Alpha. The nontoken Alpha therefore
    // shares a name with a token you control and must be excluded.
    let alpha_token = scenario.add_creature(P0, "Alpha", 2, 2).id();
    let alpha_nontoken = scenario.add_creature(P0, "Alpha", 2, 2).id();
    let hidden = scenario.add_creature(P0, "Hidden", 2, 2).id();
    // A second legal target keeps the forced-unique auto-announcement (CR
    // 601.2c) from hiding the prompt this test reads.
    let zeta = scenario.add_creature(P0, "Zeta", 2, 2).id();

    let filler = ["Plains"; 10];
    scenario.with_library_top(P0, &filler);
    scenario.with_library_top(P1, &filler);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&alpha_token).unwrap().is_token = true;
        apply_face_down_creature_characteristics(
            state.objects.get_mut(&hidden).unwrap(),
            &FaceDownProfile::vanilla_2_2(),
        );
        state
            .objects
            .get_mut(&saga)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 0);
    }
    assert_eq!(
        runner.state().objects[&hidden].name,
        "",
        "reach guard: the face-down creature is nameless"
    );
    assert!(
        runner.state().objects[&alpha_token].is_token,
        "reach guard: the reference token is a token"
    );

    let legal = reach_chapter_target_selection(&mut runner, saga);
    let mut legal_ids: Vec<ObjectId> = legal
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => *id,
            other => panic!("expected object targets, got {other:?}"),
        })
        .collect();
    legal_ids.sort();
    let mut expected = vec![hidden, zeta];
    expected.sort();
    assert_eq!(
        legal_ids, expected,
        "the nameless face-down creature is legal (CR 201.2a), the same-named nontoken Alpha is not, \
         and the token itself is excluded by the nontoken filter"
    );
    assert!(
        !legal_ids.contains(&alpha_nontoken),
        "the named nontoken Alpha shares a name with a token you control (CR 201.2a)"
    );
}
