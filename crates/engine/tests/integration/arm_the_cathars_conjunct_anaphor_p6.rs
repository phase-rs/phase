//! Phase 6 — comma-joined targeted P/T conjuncts (U6a) and plural declared-target
//! anaphors (U6b), driven through the real cast pipeline.
//!
//! Arm the Cathars, verbatim: "Until end of turn, target creature gets +3/+3, up
//! to one other target creature gets +2/+2, and up to one other target creature
//! gets +1/+1. Those creatures gain vigilance until end of turn."
//!
//! Two CR facts drive every row here:
//!
//!   * **CR 601.2c** — each instance of the word "target" is a SEPARATE
//!     announced choice. Three instances are three declarations, not one.
//!   * **CR 115.6** — an "up to one" instance may be declared with ZERO
//!     targets, and a declined instance contributes nothing to what a later
//!     "Those creatures" (CR 608.2c) names. The affected set is fixed when the
//!     continuous effect begins (CR 611.2c).
//!
//! Board convention, shared by every Arm the Cathars row: M0, M1 and M2 are 2/2
//! creatures P0 controls; **M3 is a 2/2 P1 controls and is NEVER declared** — it
//! is the undeclared-creature reach guard, so no negative assertion here is
//! vacuous. Every row that asserts "M3 got nothing" also asserts a positive
//! pump-and-vigilance on the same board.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const ARM_THE_CATHARS: &str = "Until end of turn, target creature gets +3/+3, up to one other \
target creature gets +2/+2, and up to one other target creature gets +1/+1. Those creatures gain \
vigilance until end of turn.";

const ROOKIE_MISTAKE: &str =
    "Until end of turn, target creature gets +0/+2 and another target creature gets -2/-0.";

const BLUE_DRAGON: &str = "Flying\nLightning Breath — When this creature enters, until your next \
turn, target creature an opponent controls gets -3/-0, up to one other target creature gets \
-2/-0, and up to one other target creature gets -1/-0.";

const JUMP_SCARE: &str = "Until end of turn, target creature gets +2/+2, gains flying, and \
becomes a Horror enchantment creature in addition to its other types.";

const RHINO: &str = "Trample\nWhen Rhino enters, destroy target artifact or land. Distribute \
three +1/+1 counters among up to three other target creatures. They gain trample until end of \
turn.";

/// Synthetic U6b runtime input: a declined "up to one" instance contributes
/// nothing to the anaphor.
const DECLINE_PROBE: &str = "Target creature gets +1/+1, and up to one other target creature \
gets +1/+1. Those creatures gain trample until end of turn.";

/// Synthetic S5-gate input, modelled on Triton Tactics: a targeted `Pump` with a
/// NON-`Pump` instruction between it and the anaphor. The `PutCounter` head is
/// what makes the anaphor bind the chain tracked set at PHASE_BASE, so this row
/// reads a real published population on both sides.
const GATE_PROBE: &str = "Put a +1/+1 counter on target creature. Target creature gets +1/+1. \
Tap target creature. Those creatures gain trample until end of turn.";

/// One 2/2 per player-slot, plus the never-declared guard M3.
struct Board {
    runner: GameRunner,
    m0: ObjectId,
    m1: ObjectId,
    m2: ObjectId,
    m3: ObjectId,
    spell: ObjectId,
}

fn board(name: &str, oracle: &str) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let m0 = scenario.add_creature(P0, "M0", 2, 2).id();
    let m1 = scenario.add_creature(P0, "M1", 2, 2).id();
    let m2 = scenario.add_creature(P0, "M2", 2, 2).id();
    let m3 = scenario.add_creature(P1, "M3", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(ManaCost::zero())
        .id();
    let runner: GameRunner = scenario.build();
    Board {
        runner,
        m0,
        m1,
        m2,
        m3,
        spell,
    }
}

fn settle(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn pt(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = &runner.state().objects[&id];
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

fn has(runner: &GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state().objects[&id].has_keyword(keyword)
}

/// The live target-slot prompt, whether it was raised by a SPELL
/// (`TargetSelection`) or by a TRIGGERED ability being put on the stack
/// (`TriggerTargetSelection`, CR 603.3d). Both carry the same slot vector.
fn prompt_slots_raw(state: &GameState) -> &[TargetSelectionSlot] {
    match &state.waiting_for {
        WaitingFor::TargetSelection { target_slots, .. }
        | WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots,
        other => panic!("expected a target-slot prompt, got {other:?}"),
    }
}

/// Slot metadata as `(optional, legal_target_count)` per slot. Read as
/// STRUCTURED slot data — no row here keys an assertion on a prompt string,
/// because #8953's `backfill_chain_description()` pushes the head's printed
/// text onto every chained link and so makes that text unstable.
fn first_prompt_slots(state: &GameState) -> Vec<(bool, usize)> {
    prompt_slots_raw(state)
        .iter()
        .map(|slot| (slot.optional, slot.legal_targets.len()))
        .collect()
}

/// The legal-target list of one slot of the live prompt.
fn first_prompt_legal(state: &GameState, slot: usize) -> Vec<TargetRef> {
    prompt_slots_raw(state)[slot].legal_targets.clone()
}

/// Drive a live `TargetSelection` prompt slot by slot through the PRODUCTION
/// reducer path, one `GameAction::ChooseTarget` per slot. `None` DECLINES that
/// slot's instance (CR 115.6) — the `SpellCast` builder cannot express a
/// middle-slot skip, because `target_objects` fills the first unused slot.
fn choose_slots(runner: &mut GameRunner, choices: &[Option<ObjectId>]) {
    for (index, choice) in choices.iter().enumerate() {
        runner
            .act(GameAction::ChooseTarget {
                target: choice.map(TargetRef::Object),
            })
            .unwrap_or_else(|err| panic!("slot {index} must accept {choice:?}: {err:?}"));
    }
}

/// Every published tracked set, id-ordered, each as a sorted list of raw ids.
fn tracked_sets(state: &GameState) -> Vec<Vec<u64>> {
    let mut sets: Vec<(u64, Vec<u64>)> = state
        .tracked_object_sets
        .iter()
        .map(|(id, members)| {
            let mut ids: Vec<u64> = members.iter().map(|o| o.0).collect();
            ids.sort_unstable();
            (id.0, ids)
        })
        .collect();
    sets.sort();
    sets.into_iter().map(|(_, ids)| ids).collect()
}

// ---------------------------------------------------------------------------
// H-2 — Arm the Cathars, one row per admissible declaration.
// ---------------------------------------------------------------------------

/// **H-2.1.** All three instances declared. CR 601.2c: three announced choices,
/// three conjuncts, in declaration order. CR 608.2c: "Those creatures" names
/// every one of them.
///
/// PAIR: M-1, M-2, M-3, M-4 (each of the four sites reverted alone).
#[test]
fn arm_the_cathars_all_three_declared() {
    let mut b = board("Arm the Cathars", ARM_THE_CATHARS);
    let (m0, m1, m2, m3, spell) = (b.m0, b.m1, b.m2, b.m3, b.spell);
    b.runner.cast(spell).target_objects(&[m0, m1, m2]).resolve();
    settle(&mut b.runner);

    assert_eq!(pt(&b.runner, m0), (5, 5), "1st conjunct: +3/+3");
    assert_eq!(pt(&b.runner, m1), (4, 4), "2nd conjunct: +2/+2");
    assert_eq!(pt(&b.runner, m2), (3, 3), "3rd conjunct: +1/+1");
    assert!(has(&b.runner, m0, &Keyword::Vigilance));
    assert!(has(&b.runner, m1, &Keyword::Vigilance));
    assert!(has(&b.runner, m2, &Keyword::Vigilance));
    // REACH GUARD for the negative: the three positives above are on this board.
    assert_eq!(
        pt(&b.runner, m3),
        (2, 2),
        "undeclared creature is untouched"
    );
    assert!(
        !has(&b.runner, m3, &Keyword::Vigilance),
        "CR 608.2c: an undeclared creature is not named by 'Those creatures'"
    );
}

/// **H-2.2.** The THIRD instance declined (CR 115.6). The two declared creatures
/// take the first two conjuncts and are vigilant; the third creature on the
/// board contributes nothing and receives nothing.
///
/// PAIR: M-1, M-3, M-4.
#[test]
fn arm_the_cathars_third_declined() {
    let mut b = board("Arm the Cathars", ARM_THE_CATHARS);
    let (m0, m1, m2, m3, spell) = (b.m0, b.m1, b.m2, b.m3, b.spell);
    b.runner.cast(spell).target_objects(&[m0, m1]).resolve();
    settle(&mut b.runner);

    assert_eq!(pt(&b.runner, m0), (5, 5));
    assert_eq!(pt(&b.runner, m1), (4, 4));
    assert!(has(&b.runner, m0, &Keyword::Vigilance));
    assert!(has(&b.runner, m1, &Keyword::Vigilance));
    // REACH GUARD: the two positives above.
    assert_eq!(
        pt(&b.runner, m2),
        (2, 2),
        "the declined instance declared none"
    );
    assert!(!has(&b.runner, m2, &Keyword::Vigilance));
    assert_eq!(pt(&b.runner, m3), (2, 2));
    assert!(!has(&b.runner, m3, &Keyword::Vigilance));
}

/// **H-2.3 — the MIDDLE instance declined, the third declared.** The sharpest
/// CR 115.6 + CR 608.2c discriminator the class admits: it is the only
/// declaration where "the declared set MINUS the declined instances" and "the
/// first N declared objects in slot order" give different answers. M2 takes the
/// **third** conjunct here (+1/+1 -> 3/3); through the `SpellCast` builder the
/// same two objects give M2 the **second** conjunct (+2/+2 -> 4/4), which is
/// `arm_the_cathars_declaration_order_not_board_order`.
///
/// Driven through the PRODUCTION reducer path — `GameAction::ChooseTarget` with
/// `None` on slot 1 — because `target_objects` fills the first unused slot and
/// so cannot express a middle-slot skip.
///
/// PAIR: M-1, M-2, M-3, M-4.
#[test]
fn arm_the_cathars_middle_instance_declined() {
    let mut b = board("Arm the Cathars", ARM_THE_CATHARS);
    let (m0, m1, m2, m3, spell) = (b.m0, b.m1, b.m2, b.m3, b.spell);
    cast_without_targets(&mut b.runner, spell);

    // CR 601.2c: three announced instances -> three slots. Slot 0 is mandatory
    // ("target creature"); slots 1 and 2 are "up to one" instances and so are
    // declinable (CR 115.6). Structured slot data, never prompt text.
    let slots = first_prompt_slots(b.runner.state());
    assert_eq!(
        slots.len(),
        3,
        "three announced target instances: {slots:?}"
    );
    assert!(!slots[0].0, "slot 0 is mandatory: {slots:?}");
    assert!(slots[1].0, "slot 1 is an 'up to one' instance: {slots:?}");
    assert!(slots[2].0, "slot 2 is an 'up to one' instance: {slots:?}");

    choose_slots(&mut b.runner, &[Some(m0), None, Some(m2)]);
    settle(&mut b.runner);

    assert_eq!(pt(&b.runner, m0), (5, 5), "1st conjunct");
    assert_eq!(
        pt(&b.runner, m2),
        (3, 3),
        "CR 115.6: the declined instance contributes none, so M2 takes the THIRD conjunct"
    );
    assert!(has(&b.runner, m0, &Keyword::Vigilance));
    assert!(has(&b.runner, m2, &Keyword::Vigilance));
    // REACH GUARD: the two positives above.
    assert_eq!(pt(&b.runner, m1), (2, 2));
    assert!(!has(&b.runner, m1, &Keyword::Vigilance));
    assert_eq!(pt(&b.runner, m3), (2, 2));
    assert!(!has(&b.runner, m3, &Keyword::Vigilance));
}

/// **H-2.3b — declaration order, not board order.** ADDITIONAL to H-2.3, never
/// a substitute for it. The same two objects submitted through the `SpellCast`
/// builder fill the first two slots, so M2 takes the SECOND conjunct at 4/4 —
/// and M1, sitting BETWEEN the two declared creatures on the board, gets
/// nothing.
///
/// PAIR: M-1, M-2, M-3, M-4.
#[test]
fn arm_the_cathars_declaration_order_not_board_order() {
    let mut b = board("Arm the Cathars", ARM_THE_CATHARS);
    let (m0, m1, m2, m3, spell) = (b.m0, b.m1, b.m2, b.m3, b.spell);
    b.runner.cast(spell).target_objects(&[m0, m2]).resolve();
    settle(&mut b.runner);

    assert_eq!(pt(&b.runner, m0), (5, 5));
    assert_eq!(pt(&b.runner, m2), (4, 4), "M2 fills the SECOND slot");
    assert!(has(&b.runner, m0, &Keyword::Vigilance));
    assert!(has(&b.runner, m2, &Keyword::Vigilance));
    // REACH GUARD: the two positives above.
    assert_eq!(
        pt(&b.runner, m1),
        (2, 2),
        "board order is not declaration order"
    );
    assert!(!has(&b.runner, m1, &Keyword::Vigilance));
    assert_eq!(pt(&b.runner, m3), (2, 2));
    assert!(!has(&b.runner, m3, &Keyword::Vigilance));
}

/// **H-2.4.** Both "up to one" instances declined (CR 115.6). Only the head is
/// pumped, and only the head is vigilant. GREEN AT BASE on its assertions.
///
/// PAIR: M-5 (the U6a-only instrument: S4 + S5 reverted together, which is the
/// charter's landing-order claim) and M-4.
#[test]
fn arm_the_cathars_both_optional_declined() {
    let mut b = board("Arm the Cathars", ARM_THE_CATHARS);
    let (m0, m1, m2, m3, spell) = (b.m0, b.m1, b.m2, b.m3, b.spell);
    b.runner.cast(spell).target_objects(&[m0]).resolve();
    settle(&mut b.runner);

    assert_eq!(pt(&b.runner, m0), (5, 5));
    assert!(
        has(&b.runner, m0, &Keyword::Vigilance),
        "CR 608.2c: the head's own declared target is named by 'Those creatures'"
    );
    // REACH GUARD: the positive above.
    for other in [m1, m2, m3] {
        assert_eq!(pt(&b.runner, other), (2, 2));
        assert!(!has(&b.runner, other, &Keyword::Vigilance));
    }
}

/// **H-2.5 — Rookie Mistake**, a bare `" and "` join with an "another target"
/// subject. Verbatim Oracle.
///
/// PAIR: M-2.
#[test]
fn rookie_mistake_both_conjuncts() {
    let mut b = board("Rookie Mistake", ROOKIE_MISTAKE);
    let (m0, m1, m3, spell) = (b.m0, b.m1, b.m3, b.spell);
    b.runner.cast(spell).target_objects(&[m0, m1]).resolve();
    settle(&mut b.runner);

    assert_eq!(pt(&b.runner, m0), (2, 4), "1st conjunct: +0/+2");
    assert_eq!(pt(&b.runner, m1), (0, 2), "2nd conjunct: -2/-0");
    // REACH GUARD: the two positives above.
    assert_eq!(
        pt(&b.runner, m3),
        (2, 2),
        "undeclared creature is untouched"
    );
}

// ---------------------------------------------------------------------------
// H-3b — U6b building-block runtime rows (synthetic input, no card name).
// ---------------------------------------------------------------------------

/// **H-3b.3.** CR 115.6 + CR 608.2c at runtime: a declined "up to one" instance
/// declared no object, so the anaphor names only the head's.
///
/// PAIR: M-3, M-4.
#[test]
fn plural_anaphor_runtime_declined_slot_contributes_none() {
    // Both declared: both gain trample. This is the reach guard for the
    // declined row below — without it the negative would be vacuous.
    let mut b = board("P6 Decline Probe", DECLINE_PROBE);
    let (m0, m1, m3, spell) = (b.m0, b.m1, b.m3, b.spell);
    b.runner.cast(spell).target_objects(&[m0, m1]).resolve();
    settle(&mut b.runner);
    assert!(has(&b.runner, m0, &Keyword::Trample));
    assert!(has(&b.runner, m1, &Keyword::Trample));
    assert!(!has(&b.runner, m3, &Keyword::Trample));

    // Second instance declined: only the head tramples.
    let mut b = board("P6 Decline Probe", DECLINE_PROBE);
    let (m0, m1, m3, spell) = (b.m0, b.m1, b.m3, b.spell);
    b.runner.cast(spell).target_objects(&[m0]).resolve();
    settle(&mut b.runner);
    assert!(has(&b.runner, m0, &Keyword::Trample));
    assert!(
        !has(&b.runner, m1, &Keyword::Trample),
        "CR 115.6: a declined instance contributes no object to the anaphor"
    );
    assert!(!has(&b.runner, m3, &Keyword::Trample));
}

/// **H-3b.H — the S5 gate's hostile neighbour.** GREEN AT BASE.
///
/// This row is a BASE-PRESERVATION LOCK and M-6's discriminator. It is NOT a
/// rules claim: no card prints this synthetic text, and nothing in CR 608.2c says
/// an intervening instruction severs antecedence. What it pins is that the gate
/// keeps S5 narrow — a `Pump` run interrupted by a non-`Pump` instruction
/// publishes nothing, so such a card's population stays whatever decided it at
/// base.
///
/// Urge to Feed ("... put a +1/+1 counter on each of those Vampires") is the
/// corpus witness for a consumer that names its OWN population. Triton Tactics
/// and Colossal Heroics are deliberately NOT cited here: they print "Untap those
/// creatures", where the pump IS the antecedent. They are preserved because their
/// consumer binds `ParentTarget`, not the tracked set.
///
/// PAIR: M-6 (delete the gate). Deleting it adds the pumped creature to the
/// published set and to the trample grant, so this row goes red. A gate is
/// never kept on an unmeasured claim.
#[test]
fn pump_then_tap_then_those_publishes_no_pump_set() {
    let mut b = board("P6 Gate Probe", GATE_PROBE);
    let (countered, pumped, tapped, untouched, spell) = (b.m0, b.m1, b.m2, b.m3, b.spell);
    b.runner
        .cast(spell)
        .target_objects(&[countered, pumped, tapped])
        .resolve();
    settle(&mut b.runner);

    // REACH GUARD: the two instructions that DO publish are in the set, so the
    // negative below is a real negative and not an empty-set artefact.
    assert_eq!(
        tracked_sets(b.runner.state()),
        vec![vec![countered.0, tapped.0]],
        "the counter and the tap publish their own populations; the pump does not"
    );
    assert!(has(&b.runner, countered, &Keyword::Trample));
    assert!(has(&b.runner, tapped, &Keyword::Trample));
    assert!(
        !has(&b.runner, pumped, &Keyword::Trample),
        "BASE-PRESERVATION: the gate holds S5 to a pure `Pump` run, so an \
         interrupted run publishes nothing and this population is decided by \
         whatever decided it at base. M-6 (ungate S5) is EXPECTED to trip this."
    );
    assert!(!has(&b.runner, untouched, &Keyword::Trample));
}

// ---------------------------------------------------------------------------
// H-6 — preservation rows (P6-C7 (d)).
// ---------------------------------------------------------------------------

/// **H-6a — a comma-joined sentence OUTSIDE the class.** GREEN AT BASE, and
/// **NON-DISCRIMINATING: phase 6 changes Jump Scare's reading not at all**
/// `[measured]`. The candidate and PHASE_BASE split this text into the SAME two
/// chunks — `["target creature gets +2/+2", "gains flying, and becomes a Horror
/// enchantment creature in addition to its other types"]` — because
/// `starts_clause_text_or_conjugated` is FIRST in that `if` and already admits
/// any conjugated verb, `"gains "` included. The targeted-P/T disjunct this
/// phase adds was never on that path, so no phase-6 mutation can redden this
/// row: under the mutation that targeted it, the corpus-wide export changed the
/// SAME three cards as the candidate and not one card more `[measured]`.
///
/// PAIR: **NONE.** Kept as a preservation lock, not as a discriminating row.
/// Jump Scare prints ONE subject with a predicate list, and the property that
/// matters is the one asserted below: the list announces NO second target
/// instance, so a second creature on the board takes none of the three
/// predicates. The chunks re-merge onto the carried subject downstream, so the
/// runtime reading is one continuous effect over one declared target.
#[test]
fn jump_scare_predicate_list_announces_no_second_target() {
    let mut b = board("Jump Scare", JUMP_SCARE);
    let (m0, m1, m3, spell) = (b.m0, b.m1, b.m3, b.spell);
    b.runner.cast(spell).target_objects(&[m0]).resolve();
    settle(&mut b.runner);

    assert_eq!(pt(&b.runner, m0), (4, 4));
    assert!(has(&b.runner, m0, &Keyword::Flying));
    // REACH GUARD: the two positives above. A second creature on the board must
    // take none of the three predicates — the predicate list is not three
    // announced instances.
    assert_eq!(pt(&b.runner, m1), (2, 2));
    assert!(!has(&b.runner, m1, &Keyword::Flying));
    assert_eq!(pt(&b.runner, m3), (2, 2));
    assert!(!has(&b.runner, m3, &Keyword::Flying));
}

/// Cast a spell through the production reducer with NO pre-declared targets, so
/// the engine raises its own slot prompt (CR 601.2c announces per instance).
fn cast_without_targets(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("the spell is castable with no pre-declared targets");
}

/// Advance until a `TargetSelection` prompt is raised (triggered abilities
/// choose their targets as they are put on the stack, CR 603.3d), then return.
fn advance_to_target_prompt(runner: &mut GameRunner) {
    for _ in 0..64 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ) {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    panic!(
        "no target-slot prompt was raised: {:?}",
        std::mem::discriminant(&runner.state().waiting_for)
    );
}

/// **H-2.6 — Blue Dragon, residue #32's KNOWN-BAD LOCK** (lead ruling, ledger
/// entry 89 S-a/S-b). Verbatim Oracle.
///
/// The row asserts the CLASS axis is correct — three declared creatures take
/// -3/-0, -2/-0 and -1/-0 — and PINS two readings that are **known bad** and
/// are accepted residue, so a future fix trips this row **on purpose**:
///
///   * **(i)** the head slot is DECLINABLE although "target creature an
///     opponent controls" is printed mandatory. Cause (`[source-read]`,
///     outside phase 6's items): `oracle_trigger.rs`'s `has_up_to` stamps the
///     head ability's `optional_targeting` whenever the trigger text contains
///     "up to".
///   * **(ii)** an "other target creature" conjunct lowers to other-than-**the
///     source**, where Oracle's "other" means other than the **earlier target
///     instance**. No rule defines "other" or "another" in card text; the
///     meaning is plain English, reached through CR 608.2c ("apply the rules of
///     English"). CR 601.2c is NOT the authority for it and says the converse —
///     absent a restriction, the same object may be chosen once for EACH
///     instance of the word "target". So the source is wrongly excluded.
///
///     Only the source exclusion is pinned, and that is deliberate `[measured]`.
///     The engine already REJECTS the earlier instance's creature for a later
///     slot: after `ChooseTarget(Some(foe))` fills slot 0, submitting `foe` for
///     slot 1 returns `InvalidAction("Illegal target selected")`. The prompt's
///     precomputed `legal_targets` still lists it, because that list is not
///     narrowed as slots are filled — so an assertion read off `legal_targets`
///     could not tell a correct implementation from a broken one anyway: a
///     correct one MUST still offer that creature there, since slot 0 might yet
///     take a different one.
///
/// Neither reading changes between PHASE_BASE and the candidate; both are
/// residue #32, not phase-6 regressions.
///
/// PAIR: M-1, M-2 for the pump assertions.
#[test]
fn blue_dragon_three_conjuncts_with_known_bad_lock() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let foe = scenario.add_creature(P1, "Foe", 5, 5).id();
    let mine_a = scenario.add_creature(P0, "Mine A", 5, 5).id();
    let mine_b = scenario.add_creature(P0, "Mine B", 5, 5).id();
    let bystander = scenario.add_creature(P1, "Bystander", 5, 5).id();
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Blue Dragon", 4, 4, BLUE_DRAGON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner: GameRunner = scenario.build();
    cast_without_targets(&mut runner, dragon);
    advance_to_target_prompt(&mut runner);

    let slots = first_prompt_slots(runner.state());
    assert_eq!(slots.len(), 3, "three announced instances: {slots:?}");
    // KNOWN BAD (i) — residue #32. Oracle prints the head mandatory.
    assert!(
        slots[0].0,
        "KNOWN BAD (residue #32 (i)): the mandatory head is stamped declinable. \
         A fix for `has_up_to` is EXPECTED to trip this assertion on purpose."
    );
    assert!(
        slots[1].0,
        "the 'up to one' conjuncts are declinable (CR 115.6)"
    );
    assert!(slots[2].0);

    // KNOWN BAD (ii) — residue #32. "other" is read as other-than-the-source.
    let other_slot = first_prompt_legal(runner.state(), 1);
    assert!(
        !other_slot.contains(&TargetRef::Object(dragon)),
        "KNOWN BAD (residue #32 (ii)): the SOURCE is excluded from an 'other \
         target creature' slot, though Oracle's 'other' means other than the \
         earlier target INSTANCE (CR 608.2c: apply the rules of English). \
         A fix is EXPECTED to trip this."
    );

    choose_slots(&mut runner, &[Some(foe), Some(mine_a), Some(mine_b)]);
    settle(&mut runner);

    // CLASS AXIS — correct, and what the phase fixes.
    assert_eq!(pt(&runner, foe), (2, 5), "1st conjunct: -3/-0");
    assert_eq!(pt(&runner, mine_a), (3, 5), "2nd conjunct: -2/-0");
    assert_eq!(pt(&runner, mine_b), (4, 5), "3rd conjunct: -1/-0");
    // REACH GUARD: the three positives above.
    assert_eq!(
        pt(&runner, bystander),
        (5, 5),
        "undeclared creature untouched"
    );
}

/// **H-6d — Rhino, Terrible Trampler. EXPLICITLY NON-DISCRIMINATING
/// preservation** (lead ruling, ledger entry 92 (b)). Verbatim Oracle.
/// Asserts the TRAMPLE BINDING ONLY.
///
/// **NO phase-6 mutation can make this row red.** That is not a gap to be
/// closed; it is the measured fact the lead ordered recorded here, for two
/// INDEPENDENTLY SUFFICIENT reasons:
///
///   1. **The pronoun predicate is FALSE.**
///      `parser/oracle_effect/mod.rs::contains_explicit_tracked_set_pronoun`
///      is a CLOSED disjunction of eleven literal phrases ("those cards",
///      "those exiled cards", "the copies", "those permanents", "those
///      creatures", "those tokens", "those auras", "those enchantments", "the
///      exiled card", "the exiled permanent", "the exiled creature") with NO
///      bare "they"/"them" arm. Rhino prints "**They** gain trample until end
///      of turn."
///   2. **`any_prior_publishes` is TRUE, so that branch is never evaluated.**
///      `publishes_tracked_set_from_resolution` matches `Effect::PutCounter`
///      unconditionally, and Rhino's chain is
///      `Destroy -> PutCounter{multi_target 0..3} -> GenericEffect(ParentTarget,
///      [Trample])`, so control enters the FIRST branch.
///
/// Rhino's `ParentTarget` stamp comes from `imperative.rs`'s subject map
/// (`"it" | "they" | "them" | "" => ParentTarget`), a file phase 6 does not
/// touch. All four phase-6 sites are unreachable from this text: the conjunct
/// recognizer needs a `target <np> gets +-N/+-M` conjunct, which Rhino prints
/// none of, and the publish arm matches `Effect::Pump`, of which Rhino's chain
/// has none.
///
/// Rhino's own DISTRIBUTE defect (three counters on *each* declared creature
/// rather than three distributed among them) is residue #5 and is deliberately
/// NOT asserted here.
///
/// PAIR: **NONE.** Documented vacuity is acceptable; undocumented vacuity is not.
#[test]
fn rhino_trample_binds_declared_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_land_from_oracle(P1, "Doomed Land", "").id();
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let c = scenario.add_creature(P1, "C", 2, 2).id();
    let undeclared = scenario.add_creature(P1, "Undeclared", 2, 2).id();
    let rhino = scenario
        .add_creature_to_hand_from_oracle(P0, "Rhino, Terrible Trampler", 4, 4, RHINO)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner: GameRunner = scenario.build();
    cast_without_targets(&mut runner, rhino);
    advance_to_target_prompt(&mut runner);
    choose_slots(&mut runner, &[Some(land), Some(a), Some(b), Some(c)]);
    settle(&mut runner);

    for declared in [a, b, c] {
        assert!(
            has(&runner, declared, &Keyword::Trample),
            "CR 608.2c: 'They' names every creature the one multi-target \
             instance declared"
        );
    }
    // REACH GUARD: the three positives above.
    assert!(
        !has(&runner, undeclared, &Keyword::Trample),
        "an undeclared creature is not named by 'They'"
    );
}
