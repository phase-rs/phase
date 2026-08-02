//! Runtime cast-pipeline + parser-shape coverage for Captain Marvel, Apex Avenger
//! and the "reproduce the counters just placed" trigger class
//! (`Effect::ReproduceEventCounters`, CR 122.1 + CR 603.2c + CR 608.2h).
//!
//! Verbatim Oracle text (Scryfall, MSC #78): line 1 is three evergreen keywords;
//! line 2 is the whole task — "Whenever you put one or more counters on another
//! creature, if it's not a Kree, you may put the same number and kind of counters
//! on Captain Marvel."
//!
//! Built via the `/card-test` recipe: `GameScenario` +
//! `GameRunner::cast(..).resolve()` + `CastOutcome` counter deltas, on verbatim
//! Oracle text. Every negative assertion is paired with a positive reach-guard in
//! the same test (a sibling placement that DOES reproduce), so an upstream parse
//! failure cannot satisfy it vacuously.
//!
//! REVERT DISCRIMINATORS:
//! - `reproduces_same_kind_and_count_onto_itself` — neutralize
//!   `resolve_reproduce_event_counters` (or the matcher / parser) and Captain
//!   Marvel gains 0 counters; the `assert_counters(cm, .., 2)` fails.
//! - `multi_recipient_fires_once_per_recipient` — revert the per-recipient
//!   grouping to the all-in-one batched arm and the two `ReproduceEventCounters`
//!   resolutions collapse to one; the `== 2` firing-count assertion fails.
//!   (Because Captain Marvel's target is `SelfRef`, per-recipient and all-in-one
//!   are equivalent on the counter TOTAL — the firing COUNT is the only
//!   observable that discriminates them, hence the event-count assertion.)
//! - `multi_kind_single_creature_fires_exactly_once` — revert `def.batched` to a
//!   non-batched (per-event) firing and the single multi-kind placement fires
//!   twice; the `== 1` firing-count assertion fails.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, Effect, EffectKind, EventCounterReproductionCount, TargetFilter,
    TriggerCondition,
};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

/// Captain Marvel, Apex Avenger {5}{R}{W} — Legendary Creature — Human Kree Hero,
/// 4/4. Verbatim Oracle text.
const CAPTAIN_MARVEL: &str = "Flying, double strike, indestructible\nWhenever you put one or more \
                              counters on another creature, if it's not a Kree, you may put the \
                              same number and kind of counters on Captain Marvel.";

/// Put Captain Marvel onto `player`'s battlefield with its verbatim Oracle text.
fn add_captain_marvel(
    scenario: &mut GameScenario,
    player: engine::types::player::PlayerId,
) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, "Captain Marvel, Apex Avenger", 4, 4, CAPTAIN_MARVEL)
        .id()
}

/// Count how many `ReproduceEventCounters` effects resolved during the cast — the
/// number of times Captain Marvel's trigger fired (CR 603.2c firing granularity).
fn reproduction_firings(outcome: &CastOutcome) -> usize {
    outcome
        .events()
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::ReproduceEventCounters,
                    ..
                }
            )
        })
        .count()
}

/// Cast a single-target counter spell (P0) at `recipient` and drive Captain
/// Marvel's resulting "may" trigger with `accept`. Returns the outcome plus the
/// Captain Marvel and recipient object ids.
fn cast_at_recipient(
    recipient_subtypes: &[&str],
    spell_oracle: &str,
    accept: bool,
) -> (CastOutcome, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cm = add_captain_marvel(&mut scenario, P0);
    let recipient = scenario
        .add_creature(P0, "Recipient Bear", 2, 2)
        .with_subtypes(recipient_subtypes.to_vec())
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolster Rite", true, spell_oracle)
        .id();
    let mut runner = scenario.build();

    let cast = runner.cast(spell).target_objects(&[recipient]);
    let cast = if accept { cast.accept_optional() } else { cast };
    let outcome = cast.resolve();
    (outcome, cm, recipient)
}

// ---------------------------------------------------------------------------
// #1 — the primary revert-discriminator: reproduce same kind + count onto self.
// ---------------------------------------------------------------------------

#[test]
fn reproduces_same_kind_and_count_onto_itself() {
    let (outcome, cm, recipient) =
        cast_at_recipient(&[], "Put two +1/+1 counters on target creature.", true);
    // Reach guard: the counter placement happened, so Captain Marvel's trigger
    // had a live event to observe.
    outcome.assert_counters(recipient, CounterType::Plus1Plus1, 2);
    // The fix: same number AND kind reproduced onto Captain Marvel.
    outcome.assert_counters(cm, CounterType::Plus1Plus1, 2);
    // Exactly one firing (one non-Kree recipient).
    assert_eq!(reproduction_firings(&outcome), 1);
}

/// Kind fidelity (matrix #1 hostile): a non-P/T counter reproduces as that kind,
/// not as +1/+1.
#[test]
fn reproduces_the_exact_kind_placed() {
    let (outcome, cm, recipient) =
        cast_at_recipient(&[], "Put a shield counter on target creature.", true);
    outcome.assert_counters(recipient, CounterType::Shield, 1);
    outcome.assert_counters(cm, CounterType::Shield, 1);
    // No spurious +1/+1 reproduction.
    outcome.assert_counters(cm, CounterType::Plus1Plus1, 0);
}

// ---------------------------------------------------------------------------
// #2 — DELTA, not total (distinguishes from MoveCounters, which reads the
// recipient's whole counter map).
// ---------------------------------------------------------------------------

#[test]
fn reproduces_the_event_delta_not_the_recipients_total() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cm = add_captain_marvel(&mut scenario, P0);
    // Recipient already holds five +1/+1 counters.
    let recipient = scenario
        .add_creature(P0, "Recipient Bear", 2, 2)
        .with_plus_counters(5)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Bolster Rite",
            true,
            "Put a +1/+1 counter on target creature.",
        )
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[recipient])
        .accept_optional()
        .resolve();

    // Recipient now holds 6 (5 pre-existing + 1 placed).
    outcome.assert_counters(recipient, CounterType::Plus1Plus1, 6);
    // Captain Marvel reproduces only the DELTA the event placed (1), not the
    // recipient's total (6) — CR 122.1 delta semantics.
    outcome.assert_counters(cm, CounterType::Plus1Plus1, 1);
}

// ---------------------------------------------------------------------------
// #4 — intervening-if "if it's not a Kree" at trigger time (paired positive +
// negative in one test).
// ---------------------------------------------------------------------------

#[test]
fn intervening_if_gates_on_kree_recipient() {
    // Negative: recipient IS a Kree → no reproduction.
    let (kree_outcome, cm_kree, kree_recipient) = cast_at_recipient(
        &["Kree"],
        "Put two +1/+1 counters on target creature.",
        true,
    );
    kree_outcome.assert_counters(kree_recipient, CounterType::Plus1Plus1, 2);
    kree_outcome.assert_counters(cm_kree, CounterType::Plus1Plus1, 0);
    assert_eq!(
        reproduction_firings(&kree_outcome),
        0,
        "a Kree recipient must not fire the reproduction"
    );

    // Positive reach-guard (same class, different fixture): a non-Kree recipient
    // DOES reproduce — proving the negative above is the gate, not a dead trigger.
    let (ok_outcome, cm_ok, _) = cast_at_recipient(
        &["Beast"],
        "Put two +1/+1 counters on target creature.",
        true,
    );
    ok_outcome.assert_counters(cm_ok, CounterType::Plus1Plus1, 2);
    assert_eq!(reproduction_firings(&ok_outcome), 1);
}

// ---------------------------------------------------------------------------
// #6 — "you may" optionality (paired accept + decline).
// ---------------------------------------------------------------------------

#[test]
fn may_optionality_accept_and_decline() {
    // Decline: Captain Marvel gains nothing, even though a legal reproduction
    // was available (recipient got its counters — the reach guard).
    let (declined, cm_dec, recipient_dec) =
        cast_at_recipient(&[], "Put two +1/+1 counters on target creature.", false);
    declined.assert_counters(recipient_dec, CounterType::Plus1Plus1, 2);
    declined.assert_counters(cm_dec, CounterType::Plus1Plus1, 0);

    // Accept: Captain Marvel reproduces.
    let (accepted, cm_acc, _) =
        cast_at_recipient(&[], "Put two +1/+1 counters on target creature.", true);
    accepted.assert_counters(cm_acc, CounterType::Plus1Plus1, 2);
}

// ---------------------------------------------------------------------------
// #7 — recipient filter "another creature" excludes Captain Marvel itself.
// ---------------------------------------------------------------------------

#[test]
fn another_creature_excludes_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cm = add_captain_marvel(&mut scenario, P0);
    // Reach guard: a genuine other creature so the trigger is demonstrably live.
    let other = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let self_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Bolster Rite",
            true,
            "Put two +1/+1 counters on target creature.",
        )
        .id();
    let other_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Bolster Rite Two",
            true,
            "Put two +1/+1 counters on target creature.",
        )
        .id();
    let mut runner = scenario.build();

    // Placing counters directly on Captain Marvel must NOT fire the trigger
    // ("another creature" excludes the source). Captain Marvel holds exactly the
    // two placed by the spell — not four.
    let on_self = runner
        .cast(self_spell)
        .target_objects(&[cm])
        .accept_optional()
        .resolve();
    on_self.assert_counters(cm, CounterType::Plus1Plus1, 2);
    assert_eq!(
        reproduction_firings(&on_self),
        0,
        "putting counters on Captain Marvel itself must not fire (another creature)"
    );

    // Reach guard: placing on the OTHER creature does reproduce → Captain Marvel
    // gains two more (total 4), proving the trigger is live and the self case
    // above was gated by "another creature", not a dead trigger.
    let on_other = runner
        .cast(other_spell)
        .target_objects(&[other])
        .accept_optional()
        .resolve();
    on_other.assert_counters(cm, CounterType::Plus1Plus1, 4);
    assert_eq!(reproduction_firings(&on_other), 1);
}

// ---------------------------------------------------------------------------
// #3 — actor gate "you put" (paired opponent-actor negative + your-actor
// positive). The opponent case is driven by making P1 the active player.
// ---------------------------------------------------------------------------

#[test]
fn actor_gate_you_versus_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cm = add_captain_marvel(&mut scenario, P0);
    let opp_bear = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
    let you_bear = scenario.add_creature(P0, "Your Bear", 2, 2).id();
    let opp_spell = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Opp Bolster",
            true,
            "Put two +1/+1 counters on target creature.",
        )
        .id();
    let you_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Your Bolster",
            true,
            "Put two +1/+1 counters on target creature.",
        )
        .id();
    let mut runner = scenario.build();

    // Opponent (P1) places the counters. Make P1 the active player with priority.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let opp_outcome = runner
        .cast(opp_spell)
        .target_objects(&[opp_bear])
        .accept_optional()
        .resolve();
    // CR 603.2c: "whenever YOU put" does not fire on the opponent's placement.
    opp_outcome.assert_counters(opp_bear, CounterType::Plus1Plus1, 2);
    opp_outcome.assert_counters(cm, CounterType::Plus1Plus1, 0);
    assert_eq!(
        reproduction_firings(&opp_outcome),
        0,
        "opponent-placed counters must not fire a 'whenever you put' trigger"
    );

    // Reach guard: when YOU (P0) place the counters, the trigger fires — proving
    // the gate above rejected the actor, not a dead trigger.
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let you_outcome = runner
        .cast(you_spell)
        .target_objects(&[you_bear])
        .accept_optional()
        .resolve();
    you_outcome.assert_counters(cm, CounterType::Plus1Plus1, 2);
    assert_eq!(reproduction_firings(&you_outcome), 1);
}

// ---------------------------------------------------------------------------
// #12 — multi-KIND placement on ONE creature fires EXACTLY ONCE (per-recipient
// granularity vs. a non-batched per-event firing).
// ---------------------------------------------------------------------------

#[test]
fn multi_kind_single_creature_fires_exactly_once() {
    let (outcome, cm, recipient) = cast_at_recipient(
        &[],
        "Put a +1/+1 counter and a shield counter on target creature.",
        true,
    );
    // Both kinds land on the recipient in one placement event batch.
    outcome.assert_counters(recipient, CounterType::Plus1Plus1, 1);
    outcome.assert_counters(recipient, CounterType::Shield, 1);
    // Captain Marvel gains BOTH kinds from a SINGLE firing (one "may" decision,
    // one reproduction folding the recipient's whole multiset).
    outcome.assert_counters(cm, CounterType::Plus1Plus1, 1);
    outcome.assert_counters(cm, CounterType::Shield, 1);
    assert_eq!(
        reproduction_firings(&outcome),
        1,
        "a single multi-kind placement on one creature must fire the reproduction \
         exactly once (CR 603.2c per-recipient, not per-kind)"
    );
}

// ---------------------------------------------------------------------------
// #13 — multi-RECIPIENT placement fires ONCE PER RECIPIENT (per-recipient
// grouping vs. the all-in-one batched arm).
// ---------------------------------------------------------------------------

#[test]
fn multi_recipient_fires_once_per_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cm = add_captain_marvel(&mut scenario, P0);
    let bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Twin Bolster",
            true,
            "Put a +1/+1 counter on each of up to two target creatures.",
        )
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[bear_a, bear_b])
        .accept_optional()
        .resolve();

    // Both recipients got their counter (reach guard: the placement happened on
    // two distinct creatures in one event batch).
    outcome.assert_counters(bear_a, CounterType::Plus1Plus1, 1);
    outcome.assert_counters(bear_b, CounterType::Plus1Plus1, 1);
    // CR 603.2c: one firing PER recipient — two separate "may" reproductions.
    assert_eq!(
        reproduction_firings(&outcome),
        2,
        "one counter-placement event on two recipients must fire the reproduction \
         once per recipient (CR 603.2c), not once for the whole batch"
    );
    // Each firing reproduces its recipient's single +1/+1 → two onto Captain Marvel.
    outcome.assert_counters(cm, CounterType::Plus1Plus1, 2);
}

// ---------------------------------------------------------------------------
// #9 — parser SHAPE. Labeled SHAPE: asserts the parsed trigger structure via
// typed accessors (not internal dual-encoded bools).
// ---------------------------------------------------------------------------

#[test]
fn parse_shape_matches_reproduction_class() {
    let parsed = parse_oracle_text(
        CAPTAIN_MARVEL,
        "Captain Marvel, Apex Avenger",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Kree".to_string(), "Hero".to_string()],
    );
    let trig = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::CounterAdded)
        .expect("Captain Marvel has a CounterAdded trigger");

    // Actor gate: "you put".
    assert_eq!(trig.valid_target, Some(TargetFilter::Controller));
    // Recipient filter is "another creature" — a real filter, not self, not unset.
    assert!(
        matches!(&trig.valid_card, Some(f) if !matches!(f, TargetFilter::SelfRef)),
        "valid_card should be 'another creature', got {:?}",
        trig.valid_card
    );
    // "you may".
    assert!(trig.optional);
    // Stays batched so the per-recipient firing path is taken.
    assert!(trig.batched, "reproduction trigger must remain batched");
    // Intervening-if "if it's not a Kree" → Not(EventObjectMatchesFilter{..}).
    assert!(
        matches!(
            &trig.condition,
            Some(TriggerCondition::Not { condition })
                if matches!(**condition, TriggerCondition::EventObjectMatchesFilter { .. })
        ),
        "condition should be Not(EventObjectMatchesFilter), got {:?}",
        trig.condition
    );
    // Effect: reproduce onto self, same number and kind, with ZERO Unimplemented.
    let effect = trig
        .execute
        .as_deref()
        .map(|a| a.effect.as_ref())
        .expect("trigger has an execute effect");
    assert!(
        matches!(
            effect,
            Effect::ReproduceEventCounters {
                target: TargetFilter::SelfRef,
                per_kind_count: EventCounterReproductionCount::SameNumber,
            }
        ),
        "effect should be ReproduceEventCounters{{SelfRef, SameNumber}}, got {effect:?}"
    );
}

/// The "an opponent puts" sibling parses `valid_target == Opponent` (Bold
/// Plagiarist actor axis) — the negative of the "you put" shape above.
#[test]
fn parse_shape_opponent_actor_axis() {
    let parsed = parse_oracle_text(
        "Whenever an opponent puts one or more counters on a creature, you may draw a card.",
        "Test Watcher",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trig = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::CounterAdded)
        .expect("opponent-actor CounterAdded trigger");
    assert_eq!(trig.valid_target, Some(TargetFilter::Opponent));
}

// ---------------------------------------------------------------------------
// #14 — parser precedence: "if it's not a token" stays the zone-change token
// condition (finding 5), while "if it's not a <subtype>" routes to the new
// event-object combinator.
// ---------------------------------------------------------------------------

#[test]
fn parser_precedence_token_versus_subtype() {
    // "not a token" must NOT be consumed by the new subtype combinator — it falls
    // through to the pre-existing zone-change token condition (CR 111.1).
    let token_parsed = parse_oracle_text(
        "Whenever you put one or more counters on another creature, if it's not a token, you may \
         draw a card.",
        "Token Watcher",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let token_trig = token_parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::CounterAdded)
        .expect("token-condition CounterAdded trigger");
    assert!(
        matches!(
            &token_trig.condition,
            Some(TriggerCondition::ZoneChangeObjectMatchesFilter { .. })
        ),
        "\"if it's not a token\" must route to the zone-change token condition, got {:?}",
        token_trig.condition
    );

    // A recognized subtype ("Kree") routes to the new event-object combinator —
    // proving the subtype path is reached first for recognized types.
    let kree_parsed = parse_oracle_text(
        "Whenever you put one or more counters on another creature, if it's not a Kree, you may \
         draw a card.",
        "Kree Watcher",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let kree_trig = kree_parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::CounterAdded)
        .expect("Kree-condition CounterAdded trigger");
    assert!(
        matches!(
            &kree_trig.condition,
            Some(TriggerCondition::Not { condition })
                if matches!(**condition, TriggerCondition::EventObjectMatchesFilter { .. })
        ),
        "\"if it's not a Kree\" must route to Not(EventObjectMatchesFilter), got {:?}",
        kree_trig.condition
    );
}

// ===========================================================================
// Bold Plagiarist — sibling of the reproduction class with an OPPONENT actor
// gate and a "they control" recipient anaphor bound to that opponent.
//
// Verbatim Oracle text (Scryfall / MTGJSON): line 1 is Flash; line 2 is the
// task — "Whenever an opponent puts one or more counters on a creature they
// control, they put the same number and kind of counters on this creature."
// "they" = the opponent who placed the counters (the actor), so the recipient
// filter is "a creature the OPPONENT controls", not one you control.
// ===========================================================================

const BOLD_PLAGIARIST: &str =
    "Flash\nWhenever an opponent puts one or more counters on a creature \
                               they control, they put the same number and kind of counters on this \
                               creature.";

/// Top-level `controller` scope of a `TargetFilter::Typed`, if any.
fn typed_controller(filter: &Option<TargetFilter>) -> Option<ControllerRef> {
    match filter {
        Some(TargetFilter::Typed(tf)) => tf.controller.clone(),
        _ => None,
    }
}

/// SHAPE: the "they control" recipient anaphor binds to the OPPONENT actor gate,
/// not to `You`. Reverting the actor→`relative_player_scope` bridge in
/// `try_parse_counter_trigger` flips `valid_card`'s controller back to `You`,
/// firing on your creatures instead of the opponent's — this assertion fails.
#[test]
fn bold_plagiarist_recipient_filter_binds_to_opponent() {
    let parsed = parse_oracle_text(
        BOLD_PLAGIARIST,
        "Bold Plagiarist",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Rogue".to_string()],
    );
    let trig = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::CounterAdded)
        .expect("Bold Plagiarist has a CounterAdded trigger");
    // Actor gate: "an opponent puts".
    assert_eq!(trig.valid_target, Some(TargetFilter::Opponent));
    // Recipient: "a creature they control" — the opponent's creature.
    assert_eq!(
        typed_controller(&trig.valid_card),
        Some(ControllerRef::Opponent),
        "\"they control\" must bind to the opponent actor, got {:?}",
        trig.valid_card
    );
}

/// Drive an opponent (P1) casting "Put two +1/+1 counters on target creature"
/// at a creature owned by `recipient_owner`, while P0's Bold Plagiarist watches.
/// Returns `(outcome, bold_plagiarist_id, recipient_id)`.
fn bold_plagiarist_opponent_places_on(
    recipient_owner: engine::types::player::PlayerId,
) -> (CastOutcome, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bp = scenario
        .add_creature_from_oracle(P0, "Bold Plagiarist", 3, 2, BOLD_PLAGIARIST)
        .id();
    let recipient = scenario
        .add_creature(recipient_owner, "Recipient Bear", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Opp Bolster",
            true,
            "Put two +1/+1 counters on target creature.",
        )
        .id();
    let mut runner = scenario.build();
    // The opponent (P1) is the active player placing the counters.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(spell).target_objects(&[recipient]).resolve();
    (outcome, bp, recipient)
}

/// RUNTIME discriminator for the "they control" fix: the recipient-controller
/// gate must bind to the placing opponent, not to you. Both halves flip if the
/// binding is reverted to `You`.
#[test]
fn bold_plagiarist_binds_they_control_to_the_placing_opponent() {
    // Positive: the opponent places counters on THEIR OWN creature → Bold
    // Plagiarist reproduces the same number and kind onto itself (SelfRef).
    // (Under the reverted `You` binding this would NOT fire, since the recipient
    // is controlled by the opponent — so this assertion is a revert discriminator.)
    let (own, bp_own, opp_bear) = bold_plagiarist_opponent_places_on(P1);
    own.assert_counters(opp_bear, CounterType::Plus1Plus1, 2); // reach guard
    own.assert_counters(bp_own, CounterType::Plus1Plus1, 2);
    assert_eq!(
        reproduction_firings(&own),
        1,
        "an opponent placing counters on a creature THEY control must fire"
    );

    // Negative discriminator: the opponent places counters on a creature YOU
    // control. The actor gate (opponent) still passes, so only the "they
    // control" recipient gate can suppress it — and it must. (Under the reverted
    // `You` binding this WOULD wrongly fire.)
    let (yours, bp_yours, your_bear) = bold_plagiarist_opponent_places_on(P0);
    yours.assert_counters(your_bear, CounterType::Plus1Plus1, 2); // reach guard
    yours.assert_counters(bp_yours, CounterType::Plus1Plus1, 0);
    assert_eq!(
        reproduction_firings(&yours),
        0,
        "an opponent placing counters on a creature YOU control must not fire \
         (\"they\" is the opponent, not you)"
    );
}

// ===========================================================================
// Aragorn, Company Leader — the PerKind + targeted (non-SelfRef) sub-class.
//
// Verbatim Oracle text (Scryfall / MTGJSON): the reproduction trigger is
// "Whenever you put one or more counters on Aragorn, put one of each of those
// kinds of counters on up to one other target creature." PerKind(1) ignores the
// event's per-kind magnitude and the effect targets another creature (not self).
// ===========================================================================

const ARAGORN: &str = "Whenever the Ring tempts you, if you chose a creature other than Aragorn as \
                       your Ring-bearer, put your choice of a counter from among first strike, \
                       vigilance, deathtouch, and lifelink on Aragorn.\nWhenever you put one or more \
                       counters on Aragorn, put one of each of those kinds of counters on up to one \
                       other target creature.";

/// SHAPE: Aragorn reproduces one of EACH KIND (PerKind(1)) onto ANOTHER target
/// creature, gated on counters placed on Aragorn itself ("you put … on Aragorn").
#[test]
fn aragorn_reproduction_shape_is_per_kind_to_target() {
    let parsed = parse_oracle_text(
        ARAGORN,
        "Aragorn, Company Leader",
        &[],
        &["Creature".to_string()],
        &[
            "Human".to_string(),
            "Noble".to_string(),
            "Ranger".to_string(),
        ],
    );
    let trig = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::CounterAdded)
        .expect("Aragorn has a CounterAdded reproduction trigger");
    // Fires on counters placed on Aragorn itself.
    assert_eq!(trig.valid_card, Some(TargetFilter::SelfRef));
    // "you put".
    assert_eq!(trig.valid_target, Some(TargetFilter::Controller));
    let effect = trig
        .execute
        .as_deref()
        .map(|a| a.effect.as_ref())
        .expect("trigger has an execute effect");
    match effect {
        Effect::ReproduceEventCounters {
            target,
            per_kind_count,
        } => {
            assert_eq!(*per_kind_count, EventCounterReproductionCount::PerKind(1));
            assert!(
                !matches!(target, TargetFilter::SelfRef),
                "Aragorn reproduces onto another target creature, not self; got {target:?}"
            );
        }
        other => panic!("expected ReproduceEventCounters, got {other:?}"),
    }
}

/// RUNTIME: a multi-kind, count>1 placement on Aragorn reproduces exactly ONE of
/// each KIND (not the count) onto the chosen target creature. Reverting the
/// `PerKind` fold arm (counters.rs) to `SameNumber` makes the target gain TWO
/// +1/+1 counters and this assertion fails.
#[test]
fn aragorn_per_kind_reproduction_places_one_of_each_kind_on_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aragorn = scenario
        .add_creature_from_oracle(P0, "Aragorn, Company Leader", 3, 3, ARAGORN)
        .id();
    let ally = scenario.add_creature(P0, "Ally Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Twin Bolster",
            true,
            "Put two +1/+1 counters and a shield counter on target creature.",
        )
        .id();
    let mut runner = scenario.build();
    // Spell target (Aragorn) is consumed first; the reproduction trigger's "up
    // to one other target creature" slot then consumes the ally.
    let outcome = runner
        .cast(spell)
        .target_objects(&[aragorn, ally])
        .resolve();

    // Reach guard: the multi-kind, count-2 placement landed on Aragorn.
    outcome.assert_counters(aragorn, CounterType::Plus1Plus1, 2);
    outcome.assert_counters(aragorn, CounterType::Shield, 1);
    // PerKind(1): exactly ONE of each KIND on the target — the +1/+1 count of 2
    // is deliberately ignored (this is the SameNumber vs PerKind discriminator).
    outcome.assert_counters(ally, CounterType::Plus1Plus1, 1);
    outcome.assert_counters(ally, CounterType::Shield, 1);
    assert_eq!(reproduction_firings(&outcome), 1);
}

/// RUNTIME: the mandatory reproduction still fires, but "up to one other target
/// creature" may choose zero targets — reproducing nothing. Pairs the zero-target
/// negative with a positive reach guard (the trigger fired) so it is not vacuous.
#[test]
fn aragorn_up_to_one_target_declined_reproduces_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aragorn = scenario
        .add_creature_from_oracle(P0, "Aragorn, Company Leader", 3, 3, ARAGORN)
        .id();
    let bystander = scenario.add_creature(P0, "Bystander Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Solo Bolster",
            true,
            "Put a shield counter on target creature.",
        )
        .id();
    let mut runner = scenario.build();
    // Only Aragorn is declared. The optional "up to one other target creature"
    // slot receives no declared object, so the reproduction chooses zero targets.
    let outcome = runner.cast(spell).target_objects(&[aragorn]).resolve();

    // Reach guard: counters landed on Aragorn and the mandatory trigger fired.
    outcome.assert_counters(aragorn, CounterType::Shield, 1);
    assert_eq!(
        reproduction_firings(&outcome),
        1,
        "the mandatory reproduction fires even when 'up to one' targets nothing"
    );
    // Zero-target resolution places no counters on any other creature.
    outcome.assert_counters(bystander, CounterType::Shield, 0);
}

// ===========================================================================
// Captain Marvel — mixed multi-recipient placement (one Kree + one non-Kree)
// exercising the per-recipient intervening-if in the batched grouping path.
// ===========================================================================

/// RUNTIME (finding: per-recipient intervening-if): a single placement event on
/// two recipients — one Kree, one non-Kree — fires the reproduction ONCE, for
/// the non-Kree recipient only. The Kree recipient's event is filtered by the
/// per-candidate "if it's not a Kree" gate inside
/// `matching_counter_added_events_by_recipient`, so Captain Marvel gains only the
/// non-Kree recipient's multiset.
#[test]
fn mixed_kree_and_non_kree_recipients_fire_only_for_non_kree() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cm = add_captain_marvel(&mut scenario, P0);
    let kree = scenario
        .add_creature(P0, "Kree Bear", 2, 2)
        .with_subtypes(vec!["Kree"])
        .id();
    let beast = scenario
        .add_creature(P0, "Beast Bear", 2, 2)
        .with_subtypes(vec!["Beast"])
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Twin Bolster",
            true,
            "Put a +1/+1 counter on each of up to two target creatures.",
        )
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[kree, beast])
        .accept_optional()
        .resolve();

    // Reach guard: both recipients got their counter in one event batch.
    outcome.assert_counters(kree, CounterType::Plus1Plus1, 1);
    outcome.assert_counters(beast, CounterType::Plus1Plus1, 1);
    // Only the non-Kree recipient fires the reproduction (per-recipient gate).
    assert_eq!(
        reproduction_firings(&outcome),
        1,
        "the Kree recipient must be suppressed per-recipient; only the non-Kree fires"
    );
    // Captain Marvel gains only the non-Kree recipient's single +1/+1.
    outcome.assert_counters(cm, CounterType::Plus1Plus1, 1);
}
