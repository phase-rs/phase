//! "Permanents/Lands enter tapped this turn" — a turn-duration floating
//! enters-tapped replacement (CR 614.1d + CR 514.2 + CR 611.2a).
//!
//! Cards under test (verbatim Oracle text):
//!   - Due Respect (Instant): "Permanents enter tapped this turn.\nDraw a card."
//!   - Nahiri's Lithoforming (Sorcery): "Sacrifice X lands. For each land
//!     sacrificed this way, draw a card. You may play X additional lands this
//!     turn. Lands you control enter tapped this turn."
//!
//! Both previously left the enter-tapped clause as `Effect::Unimplemented`.
//! The fix lifts it to `Effect::AddTargetReplacement { target: None }` carrying
//! a `ChangeZone` replacement whose `execute` is a SelfRef tap, `expiry` is
//! end-of-turn, and whose `source_controller` is anchored to the resolving
//! ability's controller at install time.
//!
//! Tests 1–2 assert parser SHAPE on the verbatim Oracle text. Tests R1–R3 drive
//! the real replacement pipeline (production entry via cast / play-land /
//! `replace_event`) and assert measured tap-state.

use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, Effect, RestrictionExpiry, TapStateChange, TargetFilter, TypeFilter,
};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};
use engine::types::ObjectId;

const P1: PlayerId = PlayerId(1);

const DUE_RESPECT: &str = "Permanents enter tapped this turn.\nDraw a card.";
const NAHIRI: &str = "Sacrifice X lands. For each land sacrificed this way, draw a card. \
     You may play X additional lands this turn. Lands you control enter tapped this turn.";
// The load-bearing clause of Nahiri's Lithoforming, cast standalone to install
// the "Lands you control" floating replacement through the real cast pipeline
// (the full X-cost land-sacrifice cast is hard to drive; the plan permits a
// minimal install as long as the ENTRY runs through production).
const LANDS_YOU_CONTROL: &str = "Lands you control enter tapped this turn.";

// --- helpers ---------------------------------------------------------------

/// Collect every `Effect` across all abilities' `sub_ability` chains.
fn all_effects(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<&Effect> {
    let mut out = Vec::new();
    for ability in &parsed.abilities {
        let mut cur = Some(ability);
        while let Some(d) = cur {
            out.push(&*d.effect);
            cur = d.sub_ability.as_deref();
        }
    }
    out
}

/// The single lifted enters-tapped replacement carried by the parsed abilities.
/// Panics with a clear message if the `AddTargetReplacement { target: None }`
/// shape is absent (positive reach-guard: proves the clause parsed).
fn lifted_replacement(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> engine::types::ability::ReplacementDefinition {
    for effect in all_effects(parsed) {
        if let Effect::AddTargetReplacement {
            replacement,
            target,
        } = effect
        {
            assert_eq!(
                *target,
                TargetFilter::None,
                "the lift must use the no-per-target-binding mode (CR 611.2a)"
            );
            return (**replacement).clone();
        }
    }
    panic!("expected an AddTargetReplacement in the parsed abilities, found none");
}

fn assert_no_unimplemented(parsed: &engine::parser::oracle::ParsedAbilities) {
    for effect in all_effects(parsed) {
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "no clause may parse to Effect::Unimplemented, found {effect:?}"
        );
    }
}

/// Assert the carried replacement is the enters-tapped `ChangeZone` shape.
fn assert_enter_tapped_shape(def: &engine::types::ability::ReplacementDefinition) {
    // CR 614.1d: the event is a battlefield-entry replacement.
    assert_eq!(
        def.event,
        ReplacementEvent::ChangeZone,
        "event must be ChangeZone"
    );
    // The execute is a SelfRef single Tap (the enters-tapped modifier class).
    let execute_effect = def
        .execute
        .as_ref()
        .map(|ability| (*ability.effect).clone())
        .expect("replacement must carry an execute effect");
    assert!(
        matches!(
            execute_effect,
            Effect::SetTapState {
                state: TapStateChange::Tap,
                ..
            }
        ),
        "execute must be SetTapState{{Tap}}, got {execute_effect:?}"
    );
    // CR 514.2: expires at end of turn.
    assert_eq!(
        def.expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "replacement must expire at end of turn"
    );
}

/// Extract the `TypedFilter` from a `valid_card` filter.
fn valid_typed(
    def: &engine::types::ability::ReplacementDefinition,
) -> engine::types::ability::TypedFilter {
    match def.valid_card.clone() {
        Some(TargetFilter::Typed(tf)) => tf,
        other => panic!("expected valid_card = Typed(..), got {other:?}"),
    }
}

// --- Test 1: parser SHAPE (Due Respect) ------------------------------------

#[test]
fn due_respect_parses_permanents_enter_tapped_shape() {
    let parsed = parse_oracle_text(
        DUE_RESPECT,
        "Due Respect",
        &[],
        &["Instant".to_string()],
        &[],
    );

    // Positive reach-guard: zero Unimplemented anywhere.
    assert_no_unimplemented(&parsed);

    let def = lifted_replacement(&parsed);
    assert_enter_tapped_shape(&def);

    // "Permanents" → Typed { [Permanent] }, no controller scope.
    let tf = valid_typed(&def);
    assert!(
        matches!(tf.type_filters.as_slice(), [TypeFilter::Permanent]),
        "valid_card must be Typed{{[Permanent]}}, got {:?}",
        tf.type_filters
    );
    assert_eq!(tf.controller, None, "Due Respect's clause is uncontrolled");

    // The "Draw a card." sub-clause must still be present.
    assert!(
        all_effects(&parsed)
            .iter()
            .any(|e| matches!(e, Effect::Draw { .. })),
        "the Draw sub-effect must survive alongside the lifted replacement"
    );
}

// --- Test 2: parser SHAPE (Nahiri's Lithoforming) --------------------------

#[test]
fn nahiris_lithoforming_parses_lands_you_control_shape() {
    let parsed = parse_oracle_text(
        NAHIRI,
        "Nahiri's Lithoforming",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    // Positive reach-guard: the entire chain now parses (no Unimplemented).
    assert_no_unimplemented(&parsed);

    let def = lifted_replacement(&parsed);
    assert_enter_tapped_shape(&def);

    // "Lands you control" → Typed { [Land], controller: You }.
    let tf = valid_typed(&def);
    assert!(
        matches!(tf.type_filters.as_slice(), [TypeFilter::Land]),
        "valid_card must be Typed{{[Land]}}, got {:?}",
        tf.type_filters
    );
    assert_eq!(
        tf.controller,
        Some(ControllerRef::You),
        "Nahiri's clause is scoped to lands YOU control"
    );

    // The other clauses are unchanged and still present.
    let effects = all_effects(&parsed);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Sacrifice { .. })),
        "the Sacrifice clause must remain"
    );
    assert!(
        effects.iter().any(|e| matches!(e, Effect::Draw { .. })),
        "the per-land Draw clause must remain"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CastFromZone { .. })),
        "the play-additional-lands clause must remain"
    );
}

// --- Runtime helpers -------------------------------------------------------

fn zone_change_tap_state(runner: &mut GameRunner, object_id: ObjectId, from: Zone) -> EtbTapState {
    let mut events = Vec::new();
    let proposed = ProposedEvent::zone_change(object_id, from, Zone::Battlefield, None);
    match replace_event(runner.state_mut(), proposed, &mut events) {
        ReplacementResult::Execute(ProposedEvent::ZoneChange { enter_tapped, .. }) => enter_tapped,
        other => panic!("expected an Execute(ZoneChange) result, got {other:?}"),
    }
}

// --- Test R1: runtime — Due Respect taps later entries ---------------------

#[test]
fn r1_due_respect_taps_only_entries_after_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A card for Due Respect's own "Draw a card".
    scenario.add_spell_to_library_top(P0, "Filler", true);
    // A creature already on the battlefield BEFORE Due Respect resolves.
    let old_bear = scenario.add_creature(P0, "Old Bear", 2, 2).id();
    // The creature that will ENTER after Due Respect resolves.
    let new_bear = scenario.add_creature_to_hand(P0, "New Bear", 2, 2).id();
    let due_respect = scenario
        .add_spell_to_hand_from_oracle(P0, "Due Respect", true, DUE_RESPECT)
        .id();

    let mut runner = scenario.build();

    // Cast + resolve Due Respect: installs the replacement and draws 1.
    let outcome = runner.cast(due_respect).resolve();
    outcome.assert_hand_drawn(P0, 1);

    // A creature entering LATER this turn (production cast) enters tapped.
    runner.cast(new_bear).resolve();
    assert!(
        runner.state().objects[&new_bear].tapped,
        "a creature entering after Due Respect resolved must enter tapped"
    );

    // The creature that entered BEFORE the spell resolved is untouched.
    assert!(
        !runner.state().objects[&old_bear].tapped,
        "a creature already on the battlefield must not be tapped retroactively"
    );
}

// --- Test R2: runtime — expiry at cleanup ----------------------------------

#[test]
fn r2_due_respect_replacement_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_spell_to_library_top(P0, "Filler", true);
    // A land in P0's hand whose proposed entry we probe next turn.
    let probe_land = scenario.add_land_to_hand(P0, "Probe Land").id();
    let due_respect = scenario
        .add_spell_to_hand_from_oracle(P0, "Due Respect", true, DUE_RESPECT)
        .id();

    let mut runner = scenario.build();
    runner.cast(due_respect).resolve();

    // Sanity: the floating replacement is installed this turn.
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        1,
        "Due Respect must install exactly one floating replacement this turn"
    );

    // Advance past this turn's cleanup (CR 514.2) into the next turn.
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);

    // The floating store is empty after cleanup pruned the EndOfTurn expiry.
    assert!(
        runner.state().pending_damage_replacements.is_empty(),
        "the end-of-turn replacement must be pruned at cleanup"
    );

    // A permanent proposed to enter now is UNTAPPED — the duration expired.
    // (If expiry were not set, the surviving replacement would tap it.)
    let tap_state = zone_change_tap_state(&mut runner, probe_land, Zone::Hand);
    assert_eq!(
        tap_state,
        EtbTapState::Unspecified,
        "after the replacement expires, a new entry must be untapped"
    );
}

// --- Test R3: runtime — Nahiri multi-authority -----------------------------

#[test]
fn r3_nahiri_taps_your_lands_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Objects that will be proposed to enter through the replacement pipeline.
    let my_land = scenario.add_land_to_hand(P0, "My Land").id();
    let their_land = scenario.add_land_to_hand(P1, "Their Land").id();
    let my_creature = scenario.add_creature_to_hand(P0, "My Bear", 2, 2).id();

    // Install the "Lands you control enter tapped this turn" replacement through
    // the real cast pipeline (P0 is the resolving controller / anchor).
    let install = scenario
        .add_spell_to_hand_from_oracle(P0, "Litho Clause", true, LANDS_YOU_CONTROL)
        .id();

    let mut runner = scenario.build();
    runner.cast(install).resolve();

    // CR 109.4: the install anchors source_controller to P0 so the
    // controller-relative "you control" filter can resolve for a floating def.
    {
        let pending = &runner.state().pending_damage_replacements;
        assert_eq!(
            pending.len(),
            1,
            "exactly one floating replacement installed"
        );
        assert_eq!(
            pending[0].source_controller,
            Some(P0),
            "the resolver must anchor source_controller to the installing player"
        );
        assert_eq!(pending[0].event, ReplacementEvent::ChangeZone);
        assert_eq!(pending[0].expiry, Some(RestrictionExpiry::EndOfTurn));
    }

    // Production entry (CR 614.1d) via the real replacement pipeline:
    //   - P0's land matches [Land] + controller You  → enters TAPPED.
    assert_eq!(
        zone_change_tap_state(&mut runner, my_land, Zone::Hand),
        EtbTapState::Tapped,
        "a land YOU control must enter tapped"
    );
    //   - P1's land fails the controller:You gate      → enters UNTAPPED.
    assert_eq!(
        zone_change_tap_state(&mut runner, their_land, Zone::Hand),
        EtbTapState::Unspecified,
        "an opponent's land must not be tapped by 'Lands you control'"
    );
    //   - P0's creature fails the [Land] type gate      → enters UNTAPPED.
    assert_eq!(
        zone_change_tap_state(&mut runner, my_creature, Zone::Hand),
        EtbTapState::Unspecified,
        "a non-land you control must not be tapped by a Land-scoped replacement"
    );
}
