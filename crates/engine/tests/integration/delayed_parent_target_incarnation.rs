//! CR 400.7 / CR 603.7c — a delayed triggered ability's `ParentTarget` referent
//! is pinned to the incarnation it had when the trigger was created.
//!
//! CR 603.7c: "A delayed triggered ability that refers to a particular object
//! still affects it even if the object changes characteristics. However, if that
//! object is no longer in the zone it's expected to be in at the time the
//! delayed triggered ability resolves, the ability won't affect it. (Note that
//! if that object left that zone and then returned, it's a new object and thus
//! won't be affected. See rule 400.7.)"
//!
//! The driver bug: Goryo's Vengeance reanimates a creature and schedules
//! "Exile it at the beginning of the next end step". Blinking that creature with
//! Ephemerate makes it a NEW object (CR 400.7) that merely reuses the same
//! storage `ObjectId`, so the delayed trigger must no longer affect it.
//!
//! Controls in this file assert the OTHER direction of CR 603.7c's operative
//! test: a delayed trigger whose own condition IS the referent's zone change
//! (Saffi Eriksdotter's "when that creature dies", Lagrella's "when an exiled
//! card enters") expects the referent to have moved, and must keep working.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// ---------------------------------------------------------------- Oracle text
// Verbatim Oracle text (Scryfall). A paraphrase can take a different parser
// branch and go green while the real card stays broken.

const GORYOS_VENGEANCE: &str = "Return target legendary creature card from your graveyard to the battlefield. That creature gains haste. Exile it at the beginning of the next end step.";

const EPHEMERATE: &str =
    "Exile target creature you control, then return it to the battlefield under its owner's control.";

const SAFFI_ERIKSDOTTER: &str = "Sacrifice Saffi Eriksdotter: When target creature is put into your graveyard this turn, return that card to the battlefield.";

/// A plain removal spell used to move a referent to the graveyard through the
/// real cast pipeline rather than by mutating state behind the engine's back.
const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";

// -------------------------------------------------------------------- helpers

fn mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect()
}

/// Coloured mana for cards whose costs are not generic-payable from `mana()`'s
/// black pool — Whippoorwill's `{G}{G}` activation and Ephemerate's `{W}`.
fn mana_of(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

/// Give both players a library so crossing a turn boundary does not end the
/// game by decking (CR 704.5b).
///
/// Required by any test whose delayed trigger resolves on a LATER turn — T-Z5's
/// `AtNextPhaseForPlayer { Upkeep }` crosses two draw steps. Without it the
/// advance loop dies with `GameOver`, which would be a harness artifact
/// masquerading as a result.
fn stock_libraries(scenario: &mut GameScenario) {
    for i in 0..40 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
        scenario.add_card_to_library_top(P1, &format!("Filler {i}"));
    }
}

/// Advance until every delayed trigger has fired and the stack is empty,
/// returning every event the engine emitted along the way.
///
/// Adapted from `issue_2424_goryos_vengeance.rs:122-154` (declare-attackers /
/// declare-blockers / pass-priority with a 256-iteration guard). Kept local so
/// the existing Goryo's file is not modified.
fn advance_until_delayed_triggers_resolve(
    runner: &mut engine::game::scenario::GameRunner,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "delayed trigger never resolved; phase = {:?}, waiting_for = {:?}, dt = {}, stack = {}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            // CR 514.1: a delayed trigger that resolves on a LATER turn (T-Z5's
            // `AtNextPhaseForPlayer { Upkeep }`) makes the loop cross a cleanup
            // step, where a stocked library has pushed the player over seven
            // cards. Discarding down is required to keep advancing; a
            // `PassPriority` here is rejected outright.
            WaitingFor::DiscardToHandSize { count, cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            },
            _ => GameAction::PassPriority,
        };
        match runner.act(action) {
            Ok(result) => events.extend(result.events),
            Err(e) => panic!(
                "advancing to the delayed trigger failed: {e:?} (waiting_for = {:?})",
                runner.state().waiting_for
            ),
        }
    }
    events
}

/// Advance to the end of the current turn, regardless of whether any delayed
/// trigger fires.
///
/// `advance_until_delayed_triggers_resolve` drains the delayed-trigger list and
/// is the right tool when a trigger is EXPECTED to fire. It is the wrong tool
/// for a negative arm whose trigger legitimately never fires — a "this turn"
/// `WhenDies` trigger on a creature that does not die stays installed until
/// cleanup, so draining would spin until the iteration guard trips and report a
/// harness stall as a result.
fn advance_past_end_of_turn(runner: &mut engine::game::scenario::GameRunner) {
    let start_turn = runner.state().turn_number;
    let mut guard = 0;
    while runner.state().turn_number == start_turn {
        guard += 1;
        assert!(
            guard < 256,
            "turn never ended; phase = {:?}, waiting_for = {:?}",
            runner.state().phase,
            runner.state().waiting_for,
        );
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::DiscardToHandSize { count, cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            },
            _ => GameAction::PassPriority,
        };
        // Never `return` on an error. A silent early exit means no game time
        // passes, and every "the referent survived to end of turn" assertion
        // downstream then holds trivially — the negative arm would pass for the
        // wrong reason. Panic instead, matching
        // `advance_until_delayed_triggers_resolve` above.
        if let Err(e) = runner.act(action) {
            panic!(
                "advancing past end of turn failed: {e:?} (phase = {:?}, waiting_for = {:?})",
                runner.state().phase,
                runner.state().waiting_for,
            );
        }
    }
    assert!(
        runner.state().turn_number > start_turn,
        "reach-guard: the turn must actually have ended for a survival assertion to mean anything"
    );
}

/// True when an `EffectResolved` event names this source — the observable proof
/// that a trigger DID fire and DID resolve (CR 603.7b), even when it affected
/// nothing.
fn effect_resolved_from(events: &[GameEvent], source: ObjectId) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::EffectResolved { source_id, .. } if *source_id == source
        )
    })
}

// ============================================================ T-a (control)

/// T-a — ANTI-VACUITY CONTROL. With no blink, Goryo's delayed trigger really
/// does exile the reanimated creature at the next end step.
///
/// This must fail if the "fix" merely disables the delayed trigger, which is
/// why it is a control rather than a nice-to-have.
#[test]
fn t_a_goryos_exiles_reanimated_creature_at_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));

    let legendary = scenario
        .add_creature_to_graveyard(P0, "Legendary Bear", 4, 4)
        .as_legendary()
        .id();
    let goryos = scenario
        .add_spell_to_hand_from_oracle(P0, "Goryo's Vengeance", true, GORYOS_VENGEANCE)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(goryos).target_object(legendary).resolve();
    // Positive reach-guard: the reanimation actually happened, so the end-step
    // assertion below is about the delayed trigger and not about a fizzled cast.
    assert_eq!(
        outcome.zone_of(legendary),
        Zone::Battlefield,
        "reach-guard: Goryo's must return the creature to the battlefield"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: Goryo's must install exactly one delayed exile trigger"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    assert_eq!(
        runner.state().objects[&legendary].zone,
        Zone::Exile,
        "T-a: with no blink the delayed trigger must exile the creature"
    );
}

// ============================================================ T-b (the bug)

/// T-b — THE BUG. Blinking the reanimated creature with Ephemerate makes it a
/// new object (CR 400.7); the delayed trigger must no longer affect it.
///
/// Two independent halves:
///   1. the creature ends on the battlefield (the CR 603.7c note), and
///   2. the trigger STILL fired and resolved as a no-op (CR 603.7b) — evidenced
///      by an `EffectResolved` naming Goryo's.
///
/// Half 2 is the direct test of the early-return event rule: it goes red if the
/// guard returns without pushing `EffectResolved`, while half 1 stays green.
#[test]
fn t_b_blinked_referent_is_not_exiled_but_trigger_still_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));

    let legendary = scenario
        .add_creature_to_graveyard(P0, "Legendary Bear", 4, 4)
        .as_legendary()
        .id();
    let goryos = scenario
        .add_spell_to_hand_from_oracle(P0, "Goryo's Vengeance", true, GORYOS_VENGEANCE)
        .id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
        .id();

    let mut runner = scenario.build();

    let reanimated = runner.cast(goryos).target_object(legendary).resolve();
    assert_eq!(
        reanimated.zone_of(legendary),
        Zone::Battlefield,
        "reach-guard: Goryo's must return the creature before it can be blinked"
    );
    let goryos_source = goryos;

    let blinked = runner.cast(ephemerate).target_object(legendary).resolve();
    // Reach-guard: the blink really happened. Without this, "still on the
    // battlefield" at the end step could pass on a creature that never moved.
    assert_eq!(
        blinked.zone_of(legendary),
        Zone::Battlefield,
        "reach-guard: Ephemerate must return the creature to the battlefield"
    );
    assert!(
        blinked.events().iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. } if *object_id == legendary
        )),
        "reach-guard: Ephemerate must actually exile the creature (blink leg 1)"
    );

    let events = advance_until_delayed_triggers_resolve(&mut runner);

    // Half 1 — CR 603.7c: it left the zone and returned, so it is a new object.
    assert_eq!(
        runner.state().objects[&legendary].zone,
        Zone::Battlefield,
        "T-b: a blinked referent is a NEW object and must not be exiled by the \
         delayed trigger (CR 400.7 / CR 603.7c)"
    );

    // Half 2 — CR 603.7b: the trigger still fired and still resolved.
    assert!(
        effect_resolved_from(&events, goryos_source),
        "T-b: the delayed trigger must still fire and resolve as a no-op, \
         emitting EffectResolved (CR 603.7b)"
    );
}

// ============================================================ T-c (ruling)

/// T-c — the official ruling's literal case: "If the returned creature leaves
/// the battlefield before the end step, it will remain in its current zone. It
/// won't be exiled."
///
/// The referent left and did NOT return, so it must stay in the graveyard —
/// explicitly distinguished from `Zone::Exile`.
#[test]
fn t_c_referent_that_left_and_did_not_return_stays_put() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(12));

    let legendary = scenario
        .add_creature_to_graveyard(P0, "Legendary Bear", 4, 4)
        .as_legendary()
        .id();
    let goryos = scenario
        .add_spell_to_hand_from_oracle(P0, "Goryo's Vengeance", true, GORYOS_VENGEANCE)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();

    let reanimated = runner.cast(goryos).target_object(legendary).resolve();
    assert_eq!(
        reanimated.zone_of(legendary),
        Zone::Battlefield,
        "reach-guard: Goryo's must return the creature first"
    );

    let killed = runner.cast(removal).target_object(legendary).resolve();
    assert_eq!(
        killed.zone_of(legendary),
        Zone::Graveyard,
        "reach-guard: the removal spell must put the creature in the graveyard"
    );

    let events = advance_until_delayed_triggers_resolve(&mut runner);

    let final_zone = runner.state().objects[&legendary].zone;
    // The two zones are asserted SEPARATELY and explicitly. This was a live
    // pre-fix red (the engine exiled the creature out of the graveyard,
    // contradicting the official ruling), so the `Exile` case is named rather
    // than merely implied: a mis-written single assertion could otherwise pass
    // against the exact zone this test exists to rule out.
    assert_ne!(
        final_zone,
        Zone::Exile,
        "T-c: the delayed trigger must NOT exile a referent that left the \
         battlefield and did not return (CR 603.7c; official ruling: \"If the \
         returned creature leaves the battlefield before the end step, it will \
         remain in its current zone. It won't be exiled.\")"
    );
    assert_eq!(
        final_zone,
        Zone::Graveyard,
        "T-c: the referent left the battlefield and did not return, so it stays \
         in its current zone"
    );
    // Positive reach-guard so the negatives above cannot pass vacuously on a
    // trigger that never fired at all.
    assert!(
        effect_resolved_from(&events, goryos),
        "T-c reach-guard: the delayed trigger must still have fired and resolved"
    );
}

// ====================================================== T-Z1 (MUST STAY GREEN)

/// T-Z1 — MUST-STAY-GREEN CONTROL (Saffi Eriksdotter).
///
/// "Sacrifice Saffi Eriksdotter: When target creature is put into your graveyard
/// this turn, return that card to the battlefield."
///
/// Saffi's delayed trigger's own CONDITION is the referent's zone change, so the
/// referent is EXPECTED to have moved (CR 603.7c operative test; CR 400.7e
/// affirmatively grants that such an ability can find the object in the zone it
/// moved to). Pinning it would make the card a permanent no-op.
///
/// This must pass BOTH before and after the fix. A control that was never green
/// pre-fix proves nothing about regression.
#[test]
fn t_z1_saffi_eriksdotter_still_returns_the_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(12));

    let saffi = scenario
        .add_creature_from_oracle(P0, "Saffi Eriksdotter", 2, 2, SAFFI_ERIKSDOTTER)
        .id();
    let victim = scenario.add_creature(P0, "Doomed Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();

    let activation = runner.activate(saffi, 0).target_object(victim).resolve();
    assert_eq!(
        activation.zone_of(saffi),
        Zone::Graveyard,
        "reach-guard: activating Saffi sacrifices her"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: Saffi's activation must install exactly one delayed trigger"
    );

    let killed = runner.cast(removal).target_object(victim).resolve();
    // Reach-guard on the EVENT, not on the final zone. A zone snapshot cannot
    // serve here: Saffi's trigger fires on the death and returns the card
    // within this same resolution, so the victim legitimately ends on the
    // battlefield — which is indistinguishable from "the removal never resolved
    // and it never left". Asserting `Battlefield` as both the guard and the
    // conclusion is what made this control vacuous. Prove the death happened
    // instead.
    assert!(
        killed.events().iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Graveyard,
                ..
            } if *object_id == victim
        )),
        "reach-guard: the victim must actually have been put into the graveyard \
         for Saffi's delayed trigger to have anything to return"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "T-Z1: Saffi's delayed trigger names the referent's OWN zone change, so \
         the referent is expected to have moved and must still be affected \
         (CR 603.7c operative test + CR 400.7e)"
    );
}

// ====================================================== T-Z4 (MUST STAY GREEN)

/// T-Z4 — MUST-STAY-GREEN CONTROL (Lagrella, the Magpie), the ENTRY direction.
///
/// Every other control in this file is a departure case. This is the only test
/// that distinguishes the `WhenEntersBattlefield` arm: the referent is expected
/// to have moved ONTO the battlefield, and `zones.rs:816` bumps the incarnation
/// unconditionally on `to == Battlefield`, so pinning this condition would make
/// the card a permanent no-op at 100% of firings.
///
/// It also exercises the `counters.rs` direct read and the tracked-set condition
/// erasure at the same time: Lagrella's CDT is `uses_tracked_set: true`, so its
/// condition is rewritten at `bind_tracked_set_to_condition` — which is exactly
/// why the expected-zone gate must read the PARSER-EMITTED condition.
///
/// Must pass BOTH before and after the fix.
#[test]
fn t_z4_lagrella_still_places_counters_on_the_returned_card() {
    // The card's own `oracle_text` as card-data stores it (what the engine
    // actually parses), not a paraphrase.
    const LAGRELLA: &str = "When Lagrella enters, exile any number of other target creatures controlled by different players until Lagrella leaves the battlefield. When an exiled card enters under your control this way, put two +1/+1 counters on it.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(12));

    let lagrella = scenario
        .add_creature_to_hand_from_oracle(P0, "Lagrella, the Magpie", 3, 3, LAGRELLA)
        .as_legendary()
        .id();
    let opposing = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();

    let entered = runner.cast(lagrella).target_object(opposing).resolve();
    // Reach-guard 1: the exile leg actually happened.
    assert_eq!(
        entered.zone_of(opposing),
        Zone::Exile,
        "reach-guard: Lagrella's ETB must exile the opposing creature"
    );

    let removed = runner.cast(removal).target_object(lagrella).resolve();
    assert_eq!(
        removed.zone_of(lagrella),
        Zone::Graveyard,
        "reach-guard: Lagrella must leave the battlefield to return the card"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    // Reach-guard 2: the card really is on the battlefield when the delayed
    // trigger resolves, so "counters placed" cannot pass vacuously on a card
    // that never moved.
    assert_eq!(
        runner.state().objects[&opposing].zone,
        Zone::Battlefield,
        "reach-guard: the exiled card must return to the battlefield"
    );

    let counters = runner.state().objects[&opposing]
        .counters
        .get(&engine::types::counter::CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 2,
        "T-Z4: the delayed WhenEntersBattlefield trigger names the referent's \
         OWN entry, so the referent is expected to have moved and must still be \
         affected (CR 603.7c operative test + CR 400.7e)"
    );
}

// ==================================================== T-H2 (inertness control)

/// T-H2 — SIBLING / NEGATIVE. A `ParentTarget` reference in a NON-delayed chain
/// resolves unchanged, proving the guard is inert outside the delayed path.
///
/// Goryo's own "That creature gains haste" is a `GenericEffect{ParentTarget}`
/// resolved immediately in the same chain, so it exercises the no-pin arm of the
/// keyed predicate (`find(..).is_none_or(..)`).
#[test]
fn t_h2_non_delayed_parent_target_reference_is_unaffected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));

    let legendary = scenario
        .add_creature_to_graveyard(P0, "Legendary Bear", 4, 4)
        .as_legendary()
        .id();
    let goryos = scenario
        .add_spell_to_hand_from_oracle(P0, "Goryo's Vengeance", true, GORYOS_VENGEANCE)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(goryos).target_object(legendary).resolve();

    assert_eq!(
        outcome.zone_of(legendary),
        Zone::Battlefield,
        "T-H2: the immediate ChangeZone link resolves unchanged"
    );
    // The haste grant is the immediate `GenericEffect{ParentTarget}` link. It
    // must still find its referent — no pin exists for a non-delayed chain.
    assert!(
        outcome
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| {
                matches!(
                    tce.affected,
                    engine::types::ability::TargetFilter::SpecificObject { id } if id == legendary
                )
            }),
        "T-H2: the immediate ParentTarget haste grant must still reach its \
         referent — the pin must be inert outside the delayed path"
    );
}

// ================================================ T-D1 (PLACEMENT DETECTOR)

/// T-D1 — **THE §5.3(b) PLACEMENT DETECTOR.** Role: PLACEMENT DETECTOR, and it
/// is the ONLY test in this file that can detect a §5.3(b) misplacement.
///
/// # Why this test exists
///
/// §5.3(b)'s gate must be called BEFORE the two condition binders, because they
/// rewrite `WhenDies { filter: ParentTarget }` into `WhenDies { filter:
/// SpecificObject { id } }` and the gate can no longer recognize the anaphor.
/// That constraint was **unfalsifiable** until this test existed: implementation
/// round 1 sank the call past both binders — reproducing the exact defect the
/// seam exists to prevent — and every then-existing test still passed.
///
/// A detector for this seam needs BOTH properties, and the two tests previously
/// named here satisfy at most one each:
///
/// - **(P1) a pin is actually STAMPED** — requires `parent_target_snapshot` to
///   return a NON-EMPTY list. `saffi eriksdotter` and `adarkar valkyrie` FAIL
///   this: despite Oracle text reading "target creature", neither parse declares
///   a target slot at all, so the snapshot is `[]` and the seam is inert in BOTH
///   placements. **Verify by parse, never by Oracle prose.**
/// - **(P2) the pin is actually READ** — requires the delayed effect to reach a
///   guarded terminal read. `lagrella, the magpie` FAILS this: a pin IS stamped,
///   but her `PutCounter` returns at `counters.rs`'s ungated event-context arm
///   before the guarded read, so the counters land either way.
///
/// `whippoorwill` is the only in-class card clean on both, verified at the card
/// data rather than assumed:
/// - **P1** — its root ability is `kind: Activated`, `effect: GenericEffect` with
///   a real `target: Typed { type_filters: [Creature] }` slot, and the delayed
///   trigger hangs off it via a `SequentialSibling` sub-ability chain
///   (`AddRestriction` → `CreateDelayedTrigger`). `parent_target_snapshot`
///   therefore returns through `parent_chain_targets_from_root` — the branch
///   Saffi and Adarkar Valkyrie never reach.
/// - **P2** — the delayed effect is `ChangeZone { destination: Exile, target:
///   ParentTarget }`, which routes through `resolved_targets` to the guarded
///   terminal read.
/// - `uses_tracked_set: false`, so `bind_contextual_filter_to_condition` is the
///   ONLY rewrite and a red here is single-cause.
///
/// # How it goes red
///
/// With the gate misplaced, `condition_expects_referent_move` is `false`, so a
/// pin IS stamped while the creature is on the battlefield. The creature then
/// dies, which bumps its incarnation, so the pin is stale when the trigger
/// resolves — the guard drops the referent and NOTHING is exiled. The card stays
/// in the graveyard and the `Zone::Exile` assertion fails.
///
/// With the gate correctly placed, no pin is stamped (the condition names the
/// referent's own zone change), the snapshot resolves unfiltered, and the
/// graveyard card is exiled. That is also the shipped behavior today, so this
/// test is green both pre-fix and post-fix — **the red is its job, not the
/// green.**
#[test]
fn t_d1_whippoorwill_exiles_the_dead_referent_from_the_graveyard() {
    // Verbatim Oracle text (Scryfall). A paraphrase can take a different parser
    // branch and go green while the real card stays broken.
    const WHIPPOORWILL: &str = "{G}{G}, {T}: Target creature can't be regenerated this turn. Damage that would be dealt to that creature this turn can't be prevented or dealt instead to another permanent or player. When the creature dies this turn, exile the creature.";

    // ---- Arm 1: the referent dies. The delayed trigger must exile it. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, [mana_of(ManaType::Green, 4), mana(8)].concat());
    stock_libraries(&mut scenario);

    let bird = scenario
        .add_creature_from_oracle(P0, "Whippoorwill", 1, 1, WHIPPOORWILL)
        .id();
    let victim = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();

    runner.activate(bird, 0).target_object(victim).resolve();

    // Reach-guard (P1): the activation really did install the delayed trigger.
    // Without this, a parse that silently dropped the CDT would make the whole
    // test vacuous.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: activating Whippoorwill must install exactly one delayed trigger"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "reach-guard: the referent is on the battlefield when the trigger is created"
    );

    let killed = runner.cast(removal).target_object(victim).resolve();

    // Paired positive reach-guard: the referent actually DIED. Without it,
    // "ends in Exile" could pass on a creature exiled by something else, and
    // "not in Graveyard" could pass vacuously on a creature that never died.
    // Both `Graveyard` and `Exile` are accepted here because the delayed trigger
    // may already have resolved inside `resolve()`; the one zone that would
    // falsify the premise is `Battlefield`.
    assert_ne!(
        killed.zone_of(victim),
        Zone::Battlefield,
        "reach-guard: the referent must have died for this test to mean anything"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    let final_zone = runner.state().objects[&victim].zone;
    // The two zones are asserted DISTINCTLY: this is a card-moved-out-of-the-
    // graveyard test, so `Graveyard` is the failure state, not a neutral one.
    assert_ne!(
        final_zone,
        Zone::Graveyard,
        "T-D1: the delayed WhenDies trigger names the referent's OWN zone change, \
         so no pin may be stamped and the dead creature must still be exiled \
         (CR 603.7c operative test + CR 400.7e). Still in the graveyard means a \
         pin WAS stamped — the §5.3(b) gate ran after the condition binders saw \
         the anaphor rewritten"
    );
    assert_eq!(
        final_zone,
        Zone::Exile,
        "T-D1: the referent must end in exile, not merely somewhere other than \
         the graveyard"
    );

    // ---- Arm 2: no kill. The P1 reach-guard for the pair. ----
    // A `[]` snapshot could never have produced arm 1's move, so the two arms
    // together prove a referent was genuinely captured — the property `saffi`
    // silently lacks and the reason round 1's revert-check produced no red.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, [mana_of(ManaType::Green, 4), mana(8)].concat());
    stock_libraries(&mut scenario);

    let bird = scenario
        .add_creature_from_oracle(P0, "Whippoorwill", 1, 1, WHIPPOORWILL)
        .id();
    let survivor = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();

    let mut runner = scenario.build();

    runner.activate(bird, 0).target_object(survivor).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the no-kill arm must install the same delayed trigger"
    );

    // NOT the drain helper: this trigger is SUPPOSED never to fire, so waiting
    // for the delayed-trigger list to empty would stall rather than measure.
    advance_past_end_of_turn(&mut runner);

    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "T-D1 arm 2: a referent that never died must be untouched — the trigger's \
         condition was never met"
    );
}

// ================================================= T-Z5 (PLACEMENT DETECTOR)

/// T-Z5 — **THE §5.5(e) DETECTOR** for the `counters.rs` guarded terminal read.
/// Role: PLACEMENT DETECTOR.
///
/// This is the FIRST executed test of that seam. Plan round 8 claimed `T-Z4`
/// (`lagrella`) covered it; round 1 measured that Lagrella never reaches the
/// line — her `PutCounter` returns earlier, at the ungated `resolve_event_
/// context_targets` arm. The seam was guarded but untested, not mis-tested.
///
/// `cycle of life` was chosen from the ten pinned counters-family pairs because
/// its `count` is `Fixed(1)`. The rejected alternatives and why:
/// `sacred boon` / `scars of the veteran` use `EventContextAmount` counts, which
/// would make a red ambiguous between "no counter" and "a differently-sized
/// counter"; `side quest` is an Un-set card; `infinite authority` carries an
/// intervening-if that adds a second failure mode.
///
/// Verified at the card data: root `GenericEffect` with a real
/// `target: Typed { type_filters: [Creature] }` slot, delayed
/// `AtNextPhaseForPlayer { phase: Upkeep }` → `PutCounter { counter_type: P1P1,
/// count: Fixed(1), target: ParentTarget }`, `uses_tracked_set: false`.
///
/// **Revert-failing:** revert §5.5(e)'s substitution and the blink arm places
/// the counter anyway, turning this test red.
#[test]
fn t_z5_cycle_of_life_places_no_counter_on_a_blinked_referent() {
    // Verbatim Oracle text (Scryfall).
    const CYCLE_OF_LIFE: &str = "Return this enchantment to its owner's hand: Target creature you cast this turn has base power and toughness 0/1 until your next upkeep. At the beginning of your next upkeep, put a +1/+1 counter on that creature.";

    // The `AtNextPhaseForPlayer { Upkeep }` condition does NOT name the
    // referent's zone change, so this card IS pinned — the opposite verdict from
    // T-D1's `WhenDies`, through the same gate. That contrast is deliberate.
    fn counters_on(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
        runner.state().objects[&id]
            .counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    // ---- Arm 1 (MANDATORY reach-guard): no blink, the counter IS placed. ----
    // Without this arm, "no counter" in arm 2 would pass vacuously on a trigger
    // that never fired at all.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, [mana_of(ManaType::White, 4), mana(8)].concat());
    stock_libraries(&mut scenario);

    let cycle = scenario
        .add_enchantment_from_oracle(P0, "Cycle of Life", CYCLE_OF_LIFE)
        .id();
    let subject = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    runner.activate(cycle, 0).target_object(subject).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: activating Cycle of Life must install exactly one delayed trigger"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    assert_eq!(
        counters_on(&runner, subject),
        1,
        "T-Z5 arm 1: with no blink the delayed upkeep trigger must place its \
         +1/+1 counter — this is the reach-guard that makes arm 2 non-vacuous"
    );

    // ---- Arm 2 (the detector): blink the referent, no counter may land. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, [mana_of(ManaType::White, 4), mana(8)].concat());
    stock_libraries(&mut scenario);

    let cycle = scenario
        .add_enchantment_from_oracle(P0, "Cycle of Life", CYCLE_OF_LIFE)
        .id();
    let subject = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
        .id();

    let mut runner = scenario.build();

    runner.activate(cycle, 0).target_object(subject).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the blink arm must install the same delayed trigger"
    );

    let blinked = runner.cast(ephemerate).target_object(subject).resolve();
    // Reach-guard: the blink really happened and the creature really came back,
    // so "no counter" cannot pass on a creature that is simply gone.
    assert_eq!(
        blinked.zone_of(subject),
        Zone::Battlefield,
        "reach-guard: Ephemerate must return the creature to the battlefield"
    );

    let events = advance_until_delayed_triggers_resolve(&mut runner);

    assert_eq!(
        counters_on(&runner, subject),
        0,
        "T-Z5 arm 2: the blinked referent is a NEW object (CR 400.7), so the \
         pinned delayed trigger must place no counter on it"
    );
    // CR 603.7b: the trigger still fires and still resolves — it just affects
    // nothing. Asserting the no-op WITHOUT this would also pass on a fix that
    // wrongly suppressed the trigger entirely.
    assert!(
        effect_resolved_from(&events, cycle),
        "T-Z5 arm 2: the delayed trigger must still have fired and resolved as a \
         no-op (CR 603.7b)"
    );
}
