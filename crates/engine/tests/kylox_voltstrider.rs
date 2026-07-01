//! Kylox's Voltstrider (Murders at Karlov Manor) — S01 reflexive-if completion,
//! Commit 1 (K0 + K1).
//!
//! Oracle (verified against data/card-data.json):
//!   "Collect evidence 6: This Vehicle becomes an artifact creature until end of
//!    turn.
//!    Whenever this Vehicle attacks, you may cast an instant or sorcery spell
//!    from among cards exiled with it. If that spell would be put into a
//!    graveyard, put it on the bottom of its owner's library instead.
//!    Crew 2"
//!
//! Two defects this commit fixes:
//!   * K0 — line 0 ("Collect evidence 6: ...") never dispatched as an activated
//!     ability because the colon-cost gate used a hardcoded verb allowlist that
//!     omitted keyword-action costs. The gate now probes the real cost parser
//!     (`parse_single_cost`), so "Collect evidence N" dispatches as an activated
//!     ability whose effect is the Earthrumbler-shaped self-animate (CR 205.1b
//!     becomes-an-artifact-creature; CR 701.59a collect evidence).
//!   * K1 — the cast trigger's "if that spell would be put into a graveyard, put
//!     it on the bottom of its owner's library instead" rider (CR 614.1a) was
//!     mis-routed (mistaken for the Sanwell free-cast bottom-cleanup) into a
//!     bogus `PutAtLibraryPosition{ ExiledBySource, count 0 }` in BOTH the
//!     sub-ability and a duplicate `else_ability`. It now lowers to the canonical
//!     `PutAtLibraryPosition{ ParentTarget, count 1, Bottom }` rider with no
//!     else-branch — the cast spell (not the exiled pool) is bottomed.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, Effect, LibraryPosition, QuantityExpr,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const KYLOX_ORACLE: &str = "Collect evidence 6: This Vehicle becomes an artifact creature until end of turn.\nWhenever this Vehicle attacks, you may cast an instant or sorcery spell from among cards exiled with it. If that spell would be put into a graveyard, put it on the bottom of its owner's library instead.\nCrew 2";

fn parse_kylox() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        KYLOX_ORACLE,
        "Kylox's Voltstrider",
        &[],
        &["Artifact".to_string()],
        &["Vehicle".to_string()],
    )
}

fn cost_has_collect_evidence(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::CollectEvidence { .. } => true,
        AbilityCost::Composite { costs } => costs.iter().any(cost_has_collect_evidence),
        _ => false,
    }
}

/// K0 (Row 1) — the line-0 "Collect evidence 6: ..." dispatches as an activated
/// ability carrying a `CollectEvidence { amount: 6 }` cost and the self-animate
/// effect (AddType Artifact + AddType Creature). REVERT-PROBE: restore the
/// `cost_starters` `starts_with` allowlist in `cost_prefix_is_activated` and the
/// `abilities` list is empty (the `Duration_UntilEndOfTurn` swallow returns) —
/// the `amount == 6` / AddType assertions then fail.
#[test]
fn kylox_line_zero_dispatches_collect_evidence_animate() {
    let parsed = parse_kylox();
    assert_eq!(
        parsed.abilities.len(),
        1,
        "line 0 must dispatch as exactly one activated ability, got {:?}",
        parsed.abilities
    );
    let ability = &parsed.abilities[0];
    // CR 701.59a: the cost is collect evidence 6.
    match ability.cost.as_ref() {
        Some(cost) if cost_has_collect_evidence(cost) => match cost {
            AbilityCost::CollectEvidence { amount } => {
                assert_eq!(*amount, 6, "collect evidence 6")
            }
            other => panic!("expected bare CollectEvidence cost, got {other:?}"),
        },
        other => panic!("line-0 ability must carry a CollectEvidence cost, got {other:?}"),
    }
    // CR 205.1b: the effect is the becomes-an-artifact-creature self-animate
    // (Earthrumbler shape: a continuous SelfRef GenericEffect adding both types).
    let added = added_core_types(ability);
    assert!(
        added.contains(&CoreType::Artifact) && added.contains(&CoreType::Creature),
        "line-0 ability must add Artifact + Creature (becomes-an-artifact-creature), got {added:?}"
    );
    // The swallow warnings (Duration_UntilEndOfTurn for line 0, Condition_If for
    // the rider) must both be cleared.
    assert!(
        parsed.parse_warnings.is_empty(),
        "Kylox must parse with zero swallow warnings, got {:?}",
        parsed.parse_warnings
    );
}

/// Collect the core types an activated ability's continuous self-animate adds.
fn added_core_types(ability: &AbilityDefinition) -> Vec<CoreType> {
    let Effect::GenericEffect {
        static_abilities, ..
    } = ability.effect.as_ref()
    else {
        return vec![];
    };
    let mut out = vec![];
    for sd in static_abilities {
        for m in &sd.modifications {
            if let ContinuousModification::AddType { core_type } = m {
                out.push(*core_type);
            }
        }
    }
    out
}

/// K1 (Row 3, parser shape) — the cast trigger's graveyard-redirect rider lowers
/// to the canonical `PutAtLibraryPosition{ ParentTarget, count 1, Bottom }`
/// sub-ability with NO bogus `else_ability`. The runtime effect of this redirect
/// (the cast spell lands on the library bottom) is proven by the unit test
/// `library_bottom_rider_bottoms_resolved_spell_on_resolution` in casting_costs.
/// REVERT-PROBE: revert the lower.rs Phase-1 fold and the rider becomes
/// `PutAtLibraryPosition{ ExiledBySource, count 0 }` duplicated into
/// `else_ability` — both `ParentTarget`/`count 1` and the `else_ability.is_none`
/// assertions fail.
#[test]
fn kylox_cast_rider_lowers_to_library_bottom_parent_target() {
    let parsed = parse_kylox();
    let trigger = parsed
        .triggers
        .first()
        .expect("Kylox has the attack-cast trigger");
    let execute = trigger
        .execute
        .as_deref()
        .expect("the trigger has an execute body");
    assert!(
        matches!(execute.effect.as_ref(), Effect::CastFromZone { .. }),
        "the trigger body is a CastFromZone, got {:?}",
        execute.effect
    );
    let sub = execute
        .sub_ability
        .as_deref()
        .expect("the cast carries the graveyard-redirect rider as its sub_ability");
    match sub.effect.as_ref() {
        Effect::PutAtLibraryPosition {
            target,
            count,
            position,
        } => {
            assert_eq!(
                *target,
                TargetFilter::ParentTarget,
                "the rider must bottom the CAST SPELL (ParentTarget), not the exiled pool"
            );
            assert_eq!(
                *count,
                QuantityExpr::Fixed { value: 1 },
                "exactly the one cast spell is bottomed"
            );
            assert_eq!(*position, LibraryPosition::Bottom, "bottom of library");
        }
        other => panic!("expected PutAtLibraryPosition rider, got {other:?}"),
    }
    assert!(
        execute.else_ability.is_none(),
        "the redirect rider is a replacement, not an if-you-don't branch — no else_ability"
    );
}

/// K0 negatives — a non-cost line with a top-level colon-like em-dash must not be
/// misclassified as an activated ability, and the Earthrumbler sibling (exile
/// cost, the analog whose animate Kylox reuses) still dispatches. Proves the
/// probe is strictly more precise than the old allowlist, not just wider.
#[test]
fn kylox_k0_probe_is_precise_not_just_wider() {
    // Sibling: Earthrumbler's "Exile the top card of your library: ..." animate
    // still dispatches (the allowlist already covered "exile"; the probe must
    // not regress it).
    let earthrumbler = parse_oracle_text(
        "Exile the top card of your library: This Vehicle becomes an artifact creature until end of turn.\nCrew 2",
        "Earthrumbler",
        &[],
        &["Artifact".to_string()],
        &["Vehicle".to_string()],
    );
    assert_eq!(
        earthrumbler.abilities.len(),
        1,
        "Earthrumbler's exile-cost animate must still dispatch, got {:?}",
        earthrumbler.abilities
    );

    // Negative: a pure triggered-ability line ("When ~ dies, ...") has no
    // activation cost and must not produce a spurious activated ability.
    let dies = parse_oracle_text(
        "When this creature dies, draw a card.",
        "Test Dier",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        dies.abilities.is_empty(),
        "a non-cost 'When ~ dies' line must not dispatch as an activated ability, got {:?}",
        dies.abilities
    );
}

fn add_mv_fodder(runner: &mut GameRunner, name: &str, mv: u32) -> ObjectId {
    let id = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == name)
        .map(|(id, _)| *id);
    if let Some(id) = id {
        return id;
    }
    // Place a generic-cost card in P0's graveyard as collect-evidence fodder.
    let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
    let oid = engine::game::zones::create_object(
        runner.state_mut(),
        card_id,
        P0,
        name.to_string(),
        engine::types::zones::Zone::Graveyard,
    );
    runner.state_mut().objects.get_mut(&oid).unwrap().mana_cost = ManaCost::generic(mv);
    oid
}

/// Build Kylox on the battlefield as an Artifact Vehicle (NOT a creature until
/// animated), synthesizing its abilities from Oracle text. Returns the runner +
/// Kylox's id + the index of its collect-evidence activated ability.
fn kylox_on_battlefield() -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let kylox = scenario
        .add_creature_from_oracle(P0, "Kylox's Voltstrider", 4, 4, KYLOX_ORACLE)
        .id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&kylox).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Vehicle".to_string()];
        obj.base_card_types = obj.card_types.clone();
    }
    let idx = runner.state().objects[&kylox]
        .abilities
        .iter()
        .position(|a| matches!(&a.cost, Some(c) if cost_has_collect_evidence(c)))
        .expect("K0: Kylox must carry a collect-evidence activated ability");
    (runner, kylox, idx)
}

/// K0 + animate + cost (Row 2, RUNTIME, BOTH AXES) — activating Kylox's
/// collect-evidence ability drives the real production path: `ActivateAbility` →
/// `CollectEvidenceChoice` → `SelectCards` (the cost is CHARGED: MV-6 of
/// graveyard cards are EXILED, CR 701.59a) → the self-animate resolves and makes
/// the Vehicle an artifact creature (CR 205.1b).
///
/// REVERT-PROBES (each assertion is revert-failing):
///   * revert K0 (restore the `cost_starters` allowlist) ⇒ no ability dispatches
///     ⇒ `position(...)` panics in `kylox_on_battlefield` — nothing animates.
///   * revert the CollectEvidence detour in `handle_activate_ability` ⇒ the cost
///     is a no-op, the fodder is NOT exiled (stays in graveyard) ⇒ the
///     `Zone::Exile` assertions fail (free-animate regression caught).
#[test]
fn kylox_collect_evidence_charges_cost_and_animates() {
    let (mut runner, kylox, idx) = kylox_on_battlefield();
    assert!(
        !runner.state().objects[&kylox]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "precondition: Kylox is not a creature before activation"
    );

    let fodder_a = add_mv_fodder(&mut runner, "Evidence A", 3);
    let fodder_b = add_mv_fodder(&mut runner, "Evidence B", 3);
    runner
        .act(GameAction::ActivateAbility {
            source_id: kylox,
            ability_index: idx,
        })
        .expect("activating Kylox's collect-evidence ability must be accepted");

    // CR 701.59a: collect evidence is an interactive cost — the detour prompts
    // for which graveyard cards to exile BEFORE the ability reaches the stack.
    match runner.state().waiting_for.clone() {
        WaitingFor::CollectEvidenceChoice {
            minimum_mana_value, ..
        } => assert_eq!(minimum_mana_value, 6, "collect evidence 6"),
        other => panic!(
            "the collect-evidence cost must prompt during activation (the detour), got {other:?}"
        ),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder_a, fodder_b],
        })
        .expect("paying collect evidence 6 with two MV-3 cards must be accepted");

    // CR 701.59a: the cost was CHARGED — both MV-3 fodder cards are now exiled.
    assert_eq!(
        runner.state().objects[&fodder_a].zone,
        engine::types::zones::Zone::Exile,
        "collect-evidence fodder A must be exiled (the cost is charged, not free)"
    );
    assert_eq!(
        runner.state().objects[&fodder_b].zone,
        engine::types::zones::Zone::Exile,
        "collect-evidence fodder B must be exiled (the cost is charged, not free)"
    );

    runner.advance_until_stack_empty();
    let types = &runner.state().objects[&kylox].card_types.core_types;
    assert!(
        types.contains(&CoreType::Creature),
        "CR 205.1b: Kylox must become a creature after the animate resolves, got {types:?}"
    );
    assert!(
        types.contains(&CoreType::Artifact),
        "CR 205.1b: Kylox remains an artifact (becomes an ARTIFACT creature), got {types:?}"
    );
}

/// CR 701.59b (RUNTIME, the refusal axis) — activating Kylox's collect-evidence
/// ability with an UNPAYABLE cost (graveyard total mana value < 6) is REFUSED,
/// and nothing animates. With this and the test above, the cost gate is proven
/// discriminating on both axes: payable→charged, unpayable→refused. REVERT-PROBE:
/// remove the CR 701.59b `is_payable`/`can_collect_evidence` gate and the
/// activation would be allowed (free-animate with an empty graveyard).
#[test]
fn kylox_collect_evidence_unpayable_is_refused() {
    let (mut runner, kylox, idx) = kylox_on_battlefield();
    // Only MV-2 in the graveyard (one MV-2 card) — below the collect evidence 6
    // threshold, so the cost is unpayable.
    add_mv_fodder(&mut runner, "Lone Evidence", 2);

    let result = runner.act(GameAction::ActivateAbility {
        source_id: kylox,
        ability_index: idx,
    });
    assert!(
        result.is_err(),
        "CR 701.59b: activation with graveyard MV < 6 must be REFUSED (no free animate)"
    );
    assert!(
        !runner.state().objects[&kylox]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "a refused activation must not animate Kylox"
    );
}
