//! Issue #7240 — "after this phase" must anchor to the phase the effect
//! RESOLVES in, and the turn must resume at that anchor's natural successor.
//!
//! https://github.com/phase-rs/phase/issues/7240
//!
//! Three defects, one root cause:
//!
//! 1. The parser only recognized the "after this **main** phase" wording, so
//!    every bare "after this phase" card fell through to a hard-coded
//!    `Phase::EndCombat` anchor. Cast in a main phase, the scheduled entry was
//!    anchored at a combat phase that had already ended (postcombat) or that
//!    would consume it (precombat), so the extra combat phase never happened.
//! 2. `advance_phase_once` REPLACED the natural successor with the inserted
//!    phase instead of inserting before it. With every anchor previously in use
//!    (EndCombat/Upkeep/End/Untap) the anchor's successor and the inserted
//!    phase's own successor coincide, so the defect was invisible — for
//!    main-phase anchors they diverge, swallowing the turn's own combat phase
//!    (precombat) or duplicating its main phase (postcombat).
//! 3. `count > 1` re-anchored every bundle after the first at `EndCombat`,
//!    which is exactly the slot the turn's natural combat would resume in.
//!
//! CR references:
//!   - CR 500.8: extra phases are added directly after the specified phase; if
//!     several are created after the same phase, the most recent occurs first.
//!     This is also the authority for "additional" meaning ADDED, never
//!     substituted for a phase the turn already has.
//!   - CR 608.2c: "read the whole text and apply the rules of English" — the
//!     authority for "this phase" denoting the phase the effect resolves in, and
//!     for a phase-type-qualified "this MAIN phase" carrying an implicit
//!     precondition on that resolving phase.
//!   - CR 505.1a: classifies which main phase of such a turn is the precombat
//!     one; it corroborates but does not establish the "additional" reading.
//!   - CR 506.1: a combat phase is its five steps, in order.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::ExtraPhase;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

// ---------------------------------------------------------------------------
// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
// ---------------------------------------------------------------------------

const OVERPOWERING_ATTACK: &str = "Freerunning {2}{R} (You may cast this spell for its freerunning cost if you dealt combat damage to a player this turn with an Assassin or commander.)\nUntap all creatures you control that attacked this turn. If it's your main phase, there is an additional combat phase after this phase, followed by an additional main phase.";

const RELENTLESS_ASSAULT: &str = "Untap all creatures that attacked this turn. After this main phase, there is an additional combat phase followed by an additional main phase.";

const FULL_THROTTLE: &str = "After this main phase, there are two additional combat phases.\nAt the beginning of each combat this turn, untap all creatures that attacked this turn.";

const MORAUG: &str = "Each creature you control gets +1/+0 for each time it has attacked this turn.\nLandfall — Whenever a land you control enters, if it's your main phase, there's an additional combat phase after this phase. At the beginning of that combat, untap all creatures you control.";

const AURELIA: &str = "Flying, vigilance, haste\nWhenever Aurelia attacks for the first time each turn, untap all creatures you control. After this phase, there is an additional combat phase.";

const ALL_OUT_ASSAULT: &str = "Creatures you control get +1/+1 and have deathtouch.\nWhen this enchantment enters, if it's your main phase, there is an additional combat phase after this phase followed by an additional main phase. When you next attack this turn, untap each creature you control.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// CR 506.1: the five steps of one combat phase, in order.
fn combat() -> [Phase; 5] {
    [
        Phase::BeginCombat,
        Phase::DeclareAttackers,
        Phase::DeclareBlockers,
        Phase::CombatDamage,
        Phase::EndCombat,
    ]
}

/// Walk the production phase machine and record every phase entered, stopping
/// at the end step (or after `limit` hops).
fn walk_phases(runner: &mut GameRunner, limit: usize) -> Vec<Phase> {
    let mut seq = Vec::new();
    let mut events = Vec::new();
    for _ in 0..limit {
        engine::game::turns::advance_phase(runner.state_mut(), &mut events);
        seq.push(runner.state().phase);
        if runner.state().phase == Phase::End {
            break;
        }
    }
    seq
}

/// Cast a 0-cost copy of `oracle` from `player`'s hand in `phase` and return the
/// runner positioned at that phase with the spell resolved.
fn cast_in_phase(name: &str, oracle: &str, phase: Phase) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().extra_phases.is_empty(),
        "reach guard: {name} must schedule at least one extra phase; \
         extra_phases={:?}",
        runner.state().extra_phases
    );
    runner
}

// ---------------------------------------------------------------------------
// The "after this MAIN phase" precondition (Group A).
// ---------------------------------------------------------------------------

const FURY_OF_THE_HORDE: &str = "You may exile two red cards from your hand rather than pay this spell's mana cost.\nUntap all creatures that attacked this turn. After this main phase, there is an additional combat phase followed by an additional main phase.";

/// CR 608.2c + CR 500.8: the "after this **main** phase" wording names a phase
/// TYPE, so outside a main phase there is no specified phase to add anything
/// after and NOTHING is created — while the rest of the instruction still
/// happens. Gatherer, verbatim:
///
///   * Fury of the Horde / Waves of Aggression — "If it's somehow not a main
///     phase when [this] resolves, all it does is untap all creatures that
///     attacked that turn. **No new phases are created.**"
///   * Relentless Assault — "creates an additional combat and main phase **only
///     if it resolves during a main phase**."
///   * Full Throttle — "If you somehow cast this spell when it's not a main
///     phase, the second ability still takes effect, but **there are no
///     additional combat phases this turn**."
///
/// Reachable in production: Vedalken Orrery and Leyline of Anticipation give
/// these flash. The fixture models that by casting them as instants.
///
/// This is the discriminating test for
/// `oracle_effect::imperative::this_phase_anchor_gate` — returning `None` from
/// its `Main` arm makes every row schedule a real extra combat phase and turns
/// this test red.
#[test]
fn main_qualified_anchor_creates_no_phases_outside_a_main_phase() {
    for (name, oracle) in [
        ("Relentless Assault", RELENTLESS_ASSAULT),
        ("Fury of the Horde", FURY_OF_THE_HORDE),
        ("Full Throttle", FULL_THROTTLE),
    ] {
        for phase in [Phase::DeclareBlockers, Phase::End] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(phase);
            // A creature that attacked this turn and is still tapped — the
            // observable for "all it does is untap".
            let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
            let spell = scenario
                .add_spell_to_hand_from_oracle(P0, name, true, oracle)
                .with_mana_cost(ManaCost::generic(0))
                .id();
            let mut runner = scenario.build();
            runner.state_mut().objects[&attacker].tapped = true;
            runner
                .state_mut()
                .creatures_attacked_this_turn
                .insert(attacker);

            runner.cast(spell).resolve();
            runner.advance_until_stack_empty();

            assert!(
                runner.state().extra_phases.is_empty(),
                "{name} resolving in {phase:?} must create NO extra phases; got {:?}",
                runner.state().extra_phases,
            );

            // The paired positive reach-guard: the spell really resolved and its
            // ungated sibling clause still executed. Full Throttle's other
            // ability is a delayed trigger with no resolution-time observable,
            // so it is exempt.
            if name != "Full Throttle" {
                assert!(
                    !runner.state().objects[&attacker].tapped,
                    "{name} resolving in {phase:?} must still untap the creature that \
                     attacked this turn — the gate is scoped to the additional-phase \
                     clause, not to the whole spell",
                );
            }
        }
    }
}

/// The gate-true half of the pair above: the SAME cards resolving inside a main
/// phase still schedule their phases (CR 500.8). Without this row the test above
/// would pass just as well if the gate were `AbilityCondition::Never`.
#[test]
fn main_qualified_anchor_still_schedules_inside_a_main_phase() {
    for (name, oracle, expected_entries) in [
        ("Relentless Assault", RELENTLESS_ASSAULT, 2),
        ("Fury of the Horde", FURY_OF_THE_HORDE, 2),
        ("Full Throttle", FULL_THROTTLE, 2),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, name, false, oracle)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        runner.cast(spell).resolve();
        runner.advance_until_stack_empty();

        assert_eq!(
            runner.state().extra_phases.len(),
            expected_entries,
            "{name} resolving in a main phase must still schedule its phases; got {:?}",
            runner.state().extra_phases,
        );
        assert!(
            runner
                .state()
                .extra_phases
                .iter()
                .all(|ep| ep.anchor == Phase::PostCombatMain),
            "{name}: every entry anchors at the main phase it resolved in; got {:?}",
            runner.state().extra_phases,
        );
    }
}

// ---------------------------------------------------------------------------
// Overpowering Attack — the reported card.
// ---------------------------------------------------------------------------

/// CR 500.8: cast in the postcombat main phase, Overpowering Attack
/// grants exactly ONE extra combat phase and ONE extra main phase, then the turn
/// ends.
///
/// Reverting STEP 1/2 (the parser sentinel + the resolver remap) leaves the
/// anchor at `EndCombat`, which the postcombat main phase never returns to — no
/// extra combat at all, the reported bug. Reverting the resume frame gives a
/// SECOND postcombat main phase (the turn resumed at
/// `next_phase(PostCombatMain)` of the *inserted* main instead of at the
/// anchor's successor).
#[test]
fn overpowering_attack_in_postcombat_main_grants_one_combat_and_one_main() {
    let mut runner = cast_in_phase(
        "Overpowering Attack",
        OVERPOWERING_ATTACK,
        Phase::PostCombatMain,
    );

    let seq = walk_phases(&mut runner, 24);

    let mut expected: Vec<Phase> = Vec::new();
    expected.extend(combat());
    expected.push(Phase::PostCombatMain); // the additional main phase
    expected.push(Phase::End); // …and then the turn ends
    assert_eq!(seq, expected);
    assert_eq!(
        seq.iter().filter(|p| **p == Phase::PostCombatMain).count(),
        1,
        "CR 500.8: exactly one additional main phase — not two",
    );
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 500.8: cast in the PREcombat main phase, the inserted combat phase runs
/// directly after that main phase and the turn's OWN combat phase still follows.
/// Reverting the resume frame collapses this to a single combat phase — the
/// inserted one eats the natural one's slot.
#[test]
fn overpowering_attack_in_precombat_main_inserts_before_the_natural_combat() {
    let mut runner = cast_in_phase(
        "Overpowering Attack",
        OVERPOWERING_ATTACK,
        Phase::PreCombatMain,
    );

    let seq = walk_phases(&mut runner, 32);

    let mut expected: Vec<Phase> = Vec::new();
    expected.extend(combat()); // inserted combat phase
    expected.push(Phase::PostCombatMain); // inserted main phase
    expected.extend(combat()); // the turn's own combat phase
    expected.push(Phase::PostCombatMain); // the turn's own postcombat main
    expected.push(Phase::End);
    assert_eq!(seq, expected);
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 500.8 + CR 506.1 (B4.3): because the inserted combat phase now runs FIRST,
/// it is the turn's first combat phase. `combat_phases_started_this_turn` is the
/// sole input of the `FirstCombatPhaseOfTurn` gate (`game/triggers.rs` and
/// `game/effects/mod.rs` both compare it to 1), so cards gated on "the first
/// combat phase of the turn" (Genji Glove, Hexplate Wallbreaker, Karlach,
/// Finest Hour, Raiyuu, Raph & Leo, Balthier and Fran) now fire in the inserted
/// combat rather than the natural one. This is CR-correct but a real behavior
/// change, so it is pinned here.
#[test]
fn precombat_insert_shifts_which_combat_is_the_turns_first() {
    let mut runner = cast_in_phase(
        "Overpowering Attack",
        OVERPOWERING_ATTACK,
        Phase::PreCombatMain,
    );
    assert_eq!(
        runner.state().combat_phases_started_this_turn,
        0,
        "no combat phase has begun yet this turn",
    );

    let mut events = Vec::new();
    // For each combat phase entered: (ordinal the gate reads, is this an
    // INSERTED combat?). A live resume frame means the turn is inside an
    // insertion and still owes a return point, so it identifies the inserted
    // combat without reaching into the parser or the AST.
    let mut combats: Vec<(u32, bool)> = Vec::new();
    for _ in 0..32 {
        engine::game::turns::advance_phase(runner.state_mut(), &mut events);
        if runner.state().phase == Phase::BeginCombat {
            combats.push((
                runner.state().combat_phases_started_this_turn,
                !runner.state().extra_phase_resume.is_empty(),
            ));
        }
        if runner.state().phase == Phase::End {
            break;
        }
    }

    assert_eq!(
        combats,
        vec![(1, true), (2, false)],
        "the INSERTED combat is the turn's FIRST combat phase and the turn's own \
         combat phase is its second; before the fix the order was reversed (the \
         insertion was anchored at a combat phase that had not happened yet)",
    );
}

// ---------------------------------------------------------------------------
// Relentless Assault — the follow-up main double count (D4).
// ---------------------------------------------------------------------------

/// CR 500.8: one extra combat phase and one extra main phase. Before
/// the fix the "followed by an additional main phase" entry was anchored at
/// `PostCombatMain` while the turn resumed at `next_phase(EndCombat)`, which is
/// also `PostCombatMain` — three main phases for a card that grants one.
#[test]
fn relentless_assault_in_postcombat_main_does_not_double_the_follow_up_main() {
    let mut runner = cast_in_phase(
        "Relentless Assault",
        RELENTLESS_ASSAULT,
        Phase::PostCombatMain,
    );

    let seq = walk_phases(&mut runner, 24);

    let mut expected: Vec<Phase> = Vec::new();
    expected.extend(combat());
    expected.push(Phase::PostCombatMain);
    expected.push(Phase::End);
    assert_eq!(seq, expected);
    assert_eq!(
        seq.iter().filter(|p| **p == Phase::PostCombatMain).count(),
        1,
        "CR 500.8: the follow-up main phase must be entered exactly once",
    );
}

/// CR 500.8 — a SECOND bundle created inside the additional main phase the first
/// one created. This is Aggravated Assault's primary line (`{3}{R}{R}: Untap all
/// creatures you control. After this main phase, there is an additional combat
/// phase followed by an additional main phase. Activate only as a sorcery.` —
/// re-activated in the extra main it just made); the fixture drives the same
/// clause through the cast pipeline with two Relentless Assaults, whose printed
/// text carries that sentence verbatim, so no mana or "activate only as a
/// sorcery" plumbing sits between the test and the phase machine.
///
/// CR 500.8 is positional: each bundle is added "directly after the specified
/// phase". The second cast's specified phase is the ADDITIONAL main phase it
/// resolves in, so its combat and follow-up main run back to back there, ahead
/// of the turn's own combat phase — which the FIRST bundle's anchor still owes.
///
/// Regression: the second bundle's follow-up main was deferred past the natural
/// combat phase and only picked up by the natural postcombat main's own anchor
/// scan, giving `… extra combat, extra main, extra combat, NATURAL combat,
/// natural main, extra main, end`. Both stacks still drained, so the defect was
/// pure ordering — a count-only assertion cannot see it, which is why the whole
/// phase sequence is asserted here.
#[test]
fn a_second_bundle_created_in_an_additional_main_precedes_the_natural_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Relentless Assault", false, RELENTLESS_ASSAULT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Relentless Assault", false, RELENTLESS_ASSAULT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(first).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().extra_phases.len(),
        2,
        "reach guard: the first cast schedules a combat + main bundle; got {:?}",
        runner.state().extra_phases,
    );

    let mut events = Vec::new();
    let mut seq = Vec::new();
    let mut recast = false;
    for _ in 0..40 {
        engine::game::turns::advance_phase(runner.state_mut(), &mut events);
        seq.push(runner.state().phase);
        // Re-cast in the FIRST additional main phase — the phase the first
        // bundle created — so the second bundle's anchor is that phase.
        if !recast && runner.state().phase == Phase::PostCombatMain {
            recast = true;
            runner.cast(second).resolve();
            runner.advance_until_stack_empty();
            assert_eq!(
                runner.state().phase,
                Phase::PostCombatMain,
                "reach guard: the second cast resolves in the additional main phase",
            );
            assert_eq!(
                runner.state().extra_phases.len(),
                2,
                "reach guard: the second cast schedules its own bundle; got {:?}",
                runner.state().extra_phases,
            );
        }
        if runner.state().phase == Phase::End {
            break;
        }
    }
    assert!(recast, "reach guard: an additional main phase was entered");

    let mut expected: Vec<Phase> = Vec::new();
    expected.extend(combat()); // first bundle's combat
    expected.push(Phase::PostCombatMain); // first bundle's main — re-cast here
    expected.extend(combat()); // second bundle's combat
    expected.push(Phase::PostCombatMain); // second bundle's main
    expected.extend(combat()); // the turn's OWN combat phase — still owed
    expected.push(Phase::PostCombatMain); // the turn's own postcombat main
    expected.push(Phase::End);
    assert_eq!(
        seq, expected,
        "CR 500.8: the second bundle is inserted directly after the main phase it \
         resolved in, not after the first bundle's anchor",
    );
    assert!(
        runner.state().extra_phases.is_empty(),
        "no entry may be stranded: {:?}",
        runner.state().extra_phases,
    );
    assert!(
        runner.state().extra_phase_resume.is_empty(),
        "no return point may be stranded: {:?}",
        runner.state().extra_phase_resume,
    );
}

// ---------------------------------------------------------------------------
// Full Throttle — count > 1 (H2).
// ---------------------------------------------------------------------------

/// CR 500.8: "there are two additional combat phases" means two
/// combats ADDITIONAL to the turn's own — three in total when cast precombat.
/// The old `EndCombat` re-anchor for the second bundle made it reachable only by
/// consuming the natural combat's slot, so the turn ran two.
#[test]
fn full_throttle_precombat_grants_three_combat_phases() {
    let mut runner = cast_in_phase("Full Throttle", FULL_THROTTLE, Phase::PreCombatMain);
    assert_eq!(runner.state().extra_phases.len(), 2);

    let seq = walk_phases(&mut runner, 40);

    assert_eq!(
        seq.iter().filter(|p| **p == Phase::BeginCombat).count(),
        3,
        "two additional combat phases plus the turn's own; got {seq:?}",
    );
    let mut expected: Vec<Phase> = Vec::new();
    expected.extend(combat());
    expected.extend(combat());
    expected.extend(combat());
    expected.push(Phase::PostCombatMain);
    expected.push(Phase::End);
    assert_eq!(seq, expected);
    assert!(runner.state().extra_phase_resume.is_empty());
}

// ---------------------------------------------------------------------------
// Moraug — the trigger path, precombat (H3: ordered sequence, not a count).
// ---------------------------------------------------------------------------

/// Build Moraug on the battlefield with a land in hand, play the land in the
/// precombat main phase, and let the landfall trigger resolve.
fn moraug_landfall_in_precombat_main() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Moraug, Fury of Akoum", 6, 6, MORAUG);
    let land = scenario.add_land_to_hand(P0, "Mountain").id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("play the land");
    runner.advance_until_stack_empty();

    // Reach guard: the landfall trigger really resolved and scheduled a phase
    // anchored at the main phase it resolved in (CR 500.8 + CR 608.2c).
    assert_eq!(
        runner
            .state()
            .extra_phases
            .iter()
            .map(|ep| (ep.anchor, ep.phase))
            .collect::<Vec<_>>(),
        vec![(Phase::PreCombatMain, Phase::BeginCombat)],
        "Moraug's landfall trigger must schedule one combat phase after THIS main phase",
    );
    runner
}

/// CR 500.8 (H3): assert the ORDERED sequence, not the combat count — a count
/// assertion passes both before and after the fix (the pre-fix engine also
/// reached one combat phase, just the wrong one). The discriminating property is
/// that the inserted combat comes BEFORE the turn's own combat phase and both
/// occur.
#[test]
fn moraug_landfall_in_precombat_main_inserts_before_the_natural_combat() {
    let mut runner = moraug_landfall_in_precombat_main();

    let seq = walk_phases(&mut runner, 32);

    let mut expected: Vec<Phase> = Vec::new();
    expected.extend(combat()); // the landfall combat, directly after this main phase
    expected.extend(combat()); // the turn's own combat phase
    expected.push(Phase::PostCombatMain);
    expected.push(Phase::End);
    assert_eq!(seq, expected);
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 500.8 (B2 — the nested-insertion guard): a combat-triggered extra combat
/// (Port Razer / Combat Celebrant / Scourge of the Throne, all of which schedule
/// `anchor: EndCombat, phase: BeginCombat` when they resolve during combat —
/// proven by `current_phase_sentinel_resolves_to_end_combat_from_every_combat_step`)
/// firing INSIDE Moraug's inserted combat creates a nested return point. Both
/// return points end at the same `EndCombat`, so the resume must unwind to the
/// OUTERMOST one; inspecting only the innermost frame orphans Moraug's and the
/// turn loses its natural combat phase.
#[test]
fn moraug_precombat_with_a_repeatable_combat_trigger_keeps_the_natural_combat() {
    let mut runner = moraug_landfall_in_precombat_main();

    let mut events = Vec::new();
    let mut seq = Vec::new();
    let mut injected = false;
    for _ in 0..40 {
        engine::game::turns::advance_phase(runner.state_mut(), &mut events);
        seq.push(runner.state().phase);
        // The Port Razer-shaped trigger connects during the FIRST (inserted)
        // combat and schedules another combat after "this phase".
        if !injected && runner.state().phase == Phase::CombatDamage {
            injected = true;
            runner.state_mut().extra_phases.push(ExtraPhase {
                anchor: Phase::EndCombat,
                phase: Phase::BeginCombat,
                attacker_restriction: None,
                attacker_restriction_source: None,
            });
        }
        if runner.state().phase == Phase::End {
            break;
        }
    }
    assert!(injected, "the nested extra combat must have been scheduled");

    let mut expected: Vec<Phase> = Vec::new();
    expected.extend(combat()); // Moraug's inserted combat
    expected.extend(combat()); // the nested trigger's combat
    expected.extend(combat()); // the turn's OWN combat phase — still owed
    expected.push(Phase::PostCombatMain);
    expected.push(Phase::End);
    assert_eq!(seq, expected);
    assert!(
        runner.state().extra_phase_resume.is_empty(),
        "every return point is repaid by the end of the turn",
    );
}

// ---------------------------------------------------------------------------
// Behavior preservation for the 30+ combat-resolved cards (Group B).
// ---------------------------------------------------------------------------

/// CR 500.8 + CR 506.1: Aurelia's "after this phase" resolves during combat, so
/// the sentinel remaps to `EndCombat` — byte-identical scheduling to the
/// pre-change hard-coded default, and an unchanged phase sequence. Green before
/// AND after; this is the regression guard for the whole class.
#[test]
fn aurelia_attack_trigger_extra_combat_sequence_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let aurelia = scenario
        .add_creature_from_oracle(P0, "Aurelia, the Warleader", 3, 4, AURELIA)
        .id();
    let mut runner = scenario.build();

    // Reach guard: the card parsed into a real additional-combat trigger, not an
    // `Unimplemented` gap.
    let triggers = format!("{:?}", runner.state().objects[&aurelia].trigger_definitions);
    assert!(
        triggers.contains("AdditionalPhase"),
        "Aurelia must carry a parsed AdditionalPhase trigger, got {triggers}",
    );
    assert!(
        !triggers.contains("Unimplemented"),
        "Aurelia's triggers must contain no Unimplemented gap",
    );

    // Schedule the extra combat exactly as the trigger's resolver does when it
    // resolves during the declare-attackers step (`last_step_of_phase` →
    // `EndCombat`).
    runner.state_mut().extra_phases.push(ExtraPhase {
        anchor: Phase::EndCombat,
        phase: Phase::BeginCombat,
        attacker_restriction: None,
        attacker_restriction_source: None,
    });

    let seq = walk_phases(&mut runner, 24);

    assert_eq!(
        seq,
        vec![
            // the rest of the natural combat
            Phase::DeclareBlockers,
            Phase::CombatDamage,
            Phase::EndCombat,
            // the extra combat, directly after it
            Phase::BeginCombat,
            Phase::DeclareAttackers,
            Phase::DeclareBlockers,
            Phase::CombatDamage,
            Phase::EndCombat,
            // then the turn resumes at the anchor's natural successor
            Phase::PostCombatMain,
            Phase::End,
        ]
    );
    assert!(runner.state().extra_phase_resume.is_empty());
}

// ---------------------------------------------------------------------------
// All-Out Assault — the ETB trigger path (Group C).
// ---------------------------------------------------------------------------

/// CR 500.8: the same shape as Overpowering Attack, reached through
/// an enters-the-battlefield trigger with an "if it's your main phase" gate
/// rather than through a spell's resolution.
#[test]
fn all_out_assault_etb_in_postcombat_main() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let assault = scenario
        .add_spell_to_hand_from_oracle(P0, "All-Out Assault", false, ALL_OUT_ASSAULT)
        .with_mana_cost(ManaCost::generic(0))
        .as_enchantment()
        .id();
    let mut runner = scenario.build();
    runner.cast(assault).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .extra_phases
            .iter()
            .map(|ep| (ep.anchor, ep.phase))
            .collect::<Vec<_>>(),
        vec![
            (Phase::PostCombatMain, Phase::PostCombatMain),
            (Phase::PostCombatMain, Phase::BeginCombat),
        ],
        "the ETB trigger anchors both entries at the main phase it resolved in",
    );

    let seq = walk_phases(&mut runner, 24);

    let mut expected: Vec<Phase> = Vec::new();
    expected.extend(combat());
    expected.push(Phase::PostCombatMain);
    expected.push(Phase::End);
    assert_eq!(seq, expected);
}
