//! Heroic Return / Recommission: a reflexive CR 608.2c "enters this way" rider
//! must not make the spell's reanimation line a CR 614.1c replacement.
//!
//! Before the fix, `is_replacement_pattern` scanned the WHOLE text unit for
//! CR 614.1c classifier tokens. Both cards supply "enters" and "counter"
//! entirely from their rider sentence, so Priority 8 claimed the line, the head
//! "Return target creature card from your graveyard to the battlefield"
//! instruction was dropped on the floor, and the card published a bogus
//! `Moved`/`Battlefield` replacement putting the +1/+1 counters on `SelfRef` —
//! the instant itself, which never enters the battlefield, so the effect was not
//! merely mislocated but unresolvable. `abilities` was empty.
//!
//! The fix head-scopes classification: a CR 608.2c back-reference to an earlier
//! instruction in the same ability contributes no CR 614.1c head tokens.
//!
//! Built via the `/card-test` recipe: `GameScenario` +
//! `GameRunner::cast(..).resolve()` + `CastOutcome` deltas, on verbatim Oracle
//! text from `data/card-data.json`. Every negative assertion is paired with a
//! positive reach-guard in the same test.
//!
//! REVERT DISCRIMINATOR: restore the whole-line token scan in
//! `oracle_classifier::is_replacement_pattern` and `heroic_return_reanimates_hero_with_two_extra_counters`
//! fails at its very first structural guard (`abilities` is empty again), while
//! `heroic_return_parses_to_reanimation_with_conditional_entry_counters` fails on
//! `replacements.is_empty()`.

use engine::game::casting::legal_target_slots_for_castable_spell;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    Effect, EffectKind, QuantityExpr, TargetFilter, TargetRef, TypeFilter,
};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// True when the parse reported a swallowed `Condition_If` clause.
fn has_condition_if_swallow(parsed: &engine::parser::oracle::ParsedAbilities) -> bool {
    parsed.parse_warnings.iter().any(|w| {
        matches!(
            w,
            OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If"
        )
    })
}

/// Heroic Return {5}{W} Instant — verbatim Oracle text, printed line index 1.
/// (Line index 0 is the cost-reduction static and is included so the fixture is
/// the real card, not a trimmed paraphrase.)
const HEROIC_RETURN: &str = "This spell costs {2} less to cast if a creature is attacking you.\n\
     Return target creature card from your graveyard to the battlefield. If a Hero enters this \
     way, it enters with two additional +1/+1 counters on it.";

/// Recommission {1}{W} Sorcery — verbatim Oracle text, single printed line.
const RECOMMISSION: &str = "Return target artifact or creature card with mana value 3 or less \
     from your graveyard to the battlefield. If a creature enters this way, it enters with an \
     additional +1/+1 counter on it.";

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, engine::types::identifiers::ObjectId(0), false, vec![]); n]
}

/// V2 (SHAPE): the head instruction survives as a real `ChangeZone`, and the
/// CR 608.2c rider is folded into `conditional_enter_with_counters` rather than
/// becoming a bogus replacement.
///
/// The positive shape assertions (exactly one ability, zero `Effect::Unimplemented`,
/// a `ChangeZone{Graveyard -> Battlefield}` head) are what stop the
/// `replacements.is_empty()` negative from passing vacuously on a card that
/// simply failed to parse.
#[test]
fn heroic_return_parses_to_reanimation_with_conditional_entry_counters() {
    let parsed = parse_oracle_text(
        HEROIC_RETURN,
        "Heroic Return",
        &[],
        &["Instant".to_string()],
        &[],
    );

    assert_eq!(
        parsed.abilities.len(),
        1,
        "the reanimation instruction must survive as the card's one spell ability: {parsed:?}"
    );
    let ability = &parsed.abilities[0];
    assert!(
        !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. }),
        "no Unimplemented residual may survive: {ability:?}"
    );
    assert!(
        ability.sub_ability.is_none(),
        "the rider must be CONSUMED by the fold, not left as a sub-ability: {ability:?}"
    );

    // CR 614.1c: nothing on this card is a replacement effect. Before the fix
    // this held a `PutCounter { target: SelfRef }` on the instant itself.
    assert!(
        parsed.replacements.is_empty(),
        "a CR 608.2c back-reference must not classify the line as a CR 614.1c \
         replacement: {parsed:?}"
    );

    let Effect::ChangeZone {
        origin,
        destination,
        target,
        conditional_enter_with_counters,
        ..
    } = ability.effect.as_ref()
    else {
        panic!("head must be ChangeZone, got {:#?}", ability.effect);
    };
    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_eq!(*destination, Zone::Battlefield);
    assert!(
        matches!(target, TargetFilter::Typed(t) if t.type_filters.contains(&TypeFilter::Creature)),
        "target must be the graveyard creature card, got {target:?}"
    );

    // CR 122.1 + CR 614.12: the rider's counters ride the ENTRY, keyed on the
    // Hero-ness of the entering object.
    let [(filter, counter_type, count)] = conditional_enter_with_counters.as_slice() else {
        panic!("expected exactly one conditional entry rider: {conditional_enter_with_counters:?}");
    };
    assert!(
        matches!(filter, TargetFilter::Typed(t)
            if t.type_filters.contains(&TypeFilter::Subtype("Hero".to_string()))),
        "rider filter must be the Hero subtype, got {filter:?}"
    );
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
}

/// V2 (SHAPE), sibling axis: Recommission exercises the SAME seam with a
/// different filter (disjunctive type + mana-value property) and a different
/// count, so the fix is proven class-level rather than card-shaped.
#[test]
fn recommission_parses_to_reanimation_with_conditional_entry_counters() {
    let parsed = parse_oracle_text(
        RECOMMISSION,
        "Recommission",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    assert_eq!(parsed.abilities.len(), 1, "{parsed:?}");
    assert!(
        parsed.replacements.is_empty(),
        "Recommission must not publish a replacement: {parsed:?}"
    );
    let ability = &parsed.abilities[0];
    assert!(
        !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. }),
        "{ability:?}"
    );

    let Effect::ChangeZone {
        origin,
        destination,
        target,
        conditional_enter_with_counters,
        ..
    } = ability.effect.as_ref()
    else {
        panic!("head must be ChangeZone, got {:#?}", ability.effect);
    };
    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_eq!(*destination, Zone::Battlefield);
    // The disjunctive artifact-or-creature subject must survive the head parse.
    assert!(
        matches!(target, TargetFilter::Or { filters } if filters.len() == 2),
        "target must be the artifact-or-creature disjunction, got {target:?}"
    );

    let [(filter, counter_type, count)] = conditional_enter_with_counters.as_slice() else {
        panic!("expected exactly one conditional entry rider: {conditional_enter_with_counters:?}");
    };
    assert!(
        matches!(filter, TargetFilter::Typed(t) if t.type_filters.contains(&TypeFilter::Creature)),
        "rider filter must be Creature, got {filter:?}"
    );
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
}

/// V3 + V4 (RUNTIME): the primary regression.
///
/// CR 614.12: a replacement effect that modifies how a permanent enters the
/// battlefield applies AT ENTRY, before the object is on the battlefield — so
/// the Hero arrives already carrying its two extra counters, and no separate
/// post-move `PutCounter` effect resolves (V4).
///
/// Multi-authority hostile fixture, all in ONE game so the branches are
/// distinguished rather than merely exercised:
///   * P0's graveyard holds a Hero AND a non-Hero creature — proves the counter
///     rider binds to the Hero-ness of the ENTERING OBJECT
///     (`matches_target_filter` inside `enter_with_counters_for_object`), not to
///     the spell;
///   * P1's graveyard holds a Hero too — proves the reanimation target binds to
///     `controller: You` + `InZone: Graveyard`, so an opponent's Hero is never a
///     legal target.
#[test]
fn heroic_return_reanimates_hero_with_two_extra_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Heroic Return", true, HEROIC_RETURN)
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let my_hero = scenario
        .add_creature_to_graveyard(P0, "Graveyard Hero", 2, 2)
        .with_subtypes(vec!["Hero"])
        .id();
    let my_non_hero = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let enemy_hero = scenario
        .add_creature_to_graveyard(P1, "Enemy Hero", 2, 2)
        .with_subtypes(vec!["Hero"])
        .id();
    scenario.with_mana_pool(P0, {
        let mut pool = mana(ManaType::White, 1);
        pool.extend(mana(ManaType::Colorless, 5));
        pool
    });
    let mut runner = scenario.build();

    // Structural reach-guard: the card really parsed to the reanimation ability.
    // Without this, the "0 counters" and "no PutCounter event" negatives below
    // would pass just as well on the pre-fix card, whose `abilities` was empty.
    let spell_abilities = &runner.state().objects[&spell].abilities;
    assert_eq!(
        spell_abilities.len(),
        1,
        "premise: Heroic Return must carry its reanimation ability: {spell_abilities:?}"
    );
    assert!(
        matches!(
            spell_abilities[0].effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        ),
        "premise: the ability must be the reanimation ChangeZone: {spell_abilities:?}"
    );

    // CR 109.5: "your" on an object refers to that object's controller, so
    // "from YOUR graveyard" restricts the reanimation to P0's graveyard — an
    // opponent's graveyard Hero is never a legal target. This proves the head's
    // `controller: You` + `InZone: Graveyard` filter survived head-scoping.
    let slots = legal_target_slots_for_castable_spell(runner.state(), spell);
    let slot = slots
        .first()
        .expect("the reanimation ability must publish a target slot");
    let legal: Vec<_> = slot.legal_targets.iter().collect();
    assert!(
        legal
            .iter()
            .any(|t| matches!(t, TargetRef::Object(id) if *id == my_hero)),
        "your own graveyard Hero must be targetable: {legal:?}"
    );
    assert!(
        legal
            .iter()
            .any(|t| matches!(t, TargetRef::Object(id) if *id == my_non_hero)),
        "your own graveyard non-Hero must be targetable (the Hero filter gates \
         COUNTERS, not targeting): {legal:?}"
    );
    assert!(
        !legal
            .iter()
            .any(|t| matches!(t, TargetRef::Object(id) if *id == enemy_hero)),
        "an OPPONENT's graveyard Hero must never be a legal target: {legal:?}"
    );

    let outcome = runner.cast(spell).target_objects(&[my_hero]).resolve();

    // CR 614.12: the Hero enters, already carrying exactly two extra +1/+1
    // counters. Exact, not `>= 1` — the count axis is part of the claim.
    outcome.assert_zone(&[my_hero], Zone::Battlefield);
    outcome.assert_counters(my_hero, CounterType::Plus1Plus1, 2);

    // The untouched fixtures stay put: the effect returns ONE target.
    outcome.assert_zone(&[my_non_hero, enemy_hero], Zone::Graveyard);

    // V4: the counters rode the ENTRY pipeline. A post-move `PutCounter` would
    // be a different (and rules-wrong) implementation — it would apply after the
    // object is already on the battlefield, so CR 614.12's "as it enters"
    // ordering, and anything keyed on the entering characteristics, would differ.
    assert!(
        !outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::PutCounter,
                ..
            }
        )),
        "the rider's counters must ride the entry, not resolve as a separate \
         PutCounter effect: {:?}",
        outcome.events()
    );
}

/// V3 (RUNTIME), the negative branch of the same seam: a non-Hero reanimated by
/// the same spell in the same shape gets NO extra counters.
///
/// This is the discriminator for `matches_target_filter` inside
/// `enter_with_counters_for_object` — an implementation that applied the counters
/// unconditionally passes the positive test above and fails here.
#[test]
fn heroic_return_gives_a_non_hero_no_extra_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Heroic Return", true, HEROIC_RETURN)
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let bear = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    scenario.with_mana_pool(P0, {
        let mut pool = mana(ManaType::White, 1);
        pool.extend(mana(ManaType::Colorless, 5));
        pool
    });
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[bear]).resolve();

    // Positive reach-guard: the reanimation genuinely happened, so the zero-count
    // assertion cannot pass because nothing resolved.
    outcome.assert_zone(&[bear], Zone::Battlefield);
    outcome.assert_counters(bear, CounterType::Plus1Plus1, 0);
}

/// V5 (COVERAGE HONESTY): the represented `Condition_If` gate must stop being
/// reported as a swallowed clause.
///
/// `check_swallowed_clauses` early-returns on `Effect::Unimplemented`, so the
/// negative is paired with the positive AST shape in the same test.
#[test]
fn heroic_return_reports_no_swallowed_condition() {
    for (name, oracle, types) in [
        ("Heroic Return", HEROIC_RETURN, "Instant"),
        ("Recommission", RECOMMISSION, "Sorcery"),
    ] {
        let parsed = parse_oracle_text(oracle, name, &[], &[types.to_string()], &[]);
        // Reach-guards.
        assert_eq!(parsed.abilities.len(), 1, "{name}: {parsed:?}");
        assert!(
            !matches!(
                parsed.abilities[0].effect.as_ref(),
                Effect::Unimplemented { .. }
            ),
            "{name} must parse with zero Unimplemented: {parsed:?}"
        );
        assert!(
            matches!(
                parsed.abilities[0].effect.as_ref(),
                Effect::ChangeZone { conditional_enter_with_counters, .. }
                    if !conditional_enter_with_counters.is_empty()
            ),
            "{name}'s rider must be represented by the typed slot: {parsed:?}"
        );
        assert!(
            !has_condition_if_swallow(&parsed),
            "{name}: a represented CR 608.2c entry rider must not report a swallowed \
             clause: {:?}",
            parsed.parse_warnings
        );
    }
}
