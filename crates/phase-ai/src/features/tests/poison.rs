//! Unit tests for `features::poison` — structural detection + calibration
//! anchors for the CR 104.3d poison clock. No `#[cfg(test)]` in SOURCE
//! files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerCounterKind;

use crate::ability_chain::AbilityScope;
use crate::features::poison::*;
use engine::types::ability::{AbilityKind, ControllerRef, ModalChoice, QuantityExpr, TypedFilter};
use engine::types::card::CardFace;
use engine::types::card_type::CardType;

fn creature(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn spell(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn land(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Land],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

fn infect_creature(name: &str) -> CardFace {
    let mut face = creature(name);
    face.keywords = vec![Keyword::Infect];
    face
}

/// CR 122.1f: gives poison to a player who can be an opponent.
fn poison_spell(name: &str, target: TargetFilter) -> CardFace {
    let mut face = spell(name);
    face.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GivePlayerCounter {
            counter_kind: PlayerCounterKind::Poison,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        },
    )];
    face
}

fn proliferate_spell(name: &str) -> CardFace {
    let mut face = spell(name);
    face.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Proliferate,
    )];
    face
}

#[test]
fn empty_deck_produces_defaults() {
    let feature = detect(&[]);
    assert_eq!(feature.source_count, 0);
    assert_eq!(feature.direct_count, 0);
    assert_eq!(feature.proliferate_count, 0);
    assert_eq!(feature.commitment, 0.0);
}

#[test]
fn vanilla_creature_not_registered() {
    let feature = detect(&[entry(creature("Grizzly Bears"), 4)]);
    assert_eq!(feature.source_count, 0);
    assert_eq!(feature.commitment, 0.0);
}

#[test]
fn detects_infect_toxic_and_poisonous_sources() {
    for keyword in [Keyword::Infect, Keyword::Toxic(1), Keyword::Poisonous(2)] {
        let mut face = creature("Source");
        face.keywords = vec![keyword.clone()];
        let feature = detect(&[entry(face, 4)]);
        assert_eq!(
            feature.source_count, 4,
            "keyword {keyword:?} must register as a poison source"
        );
    }
}

#[test]
fn detects_direct_poison_effect() {
    let feature = detect(&[entry(
        poison_spell("Virulent Wound", TargetFilter::Opponent),
        4,
    )]);
    assert_eq!(feature.direct_count, 4);
}

#[test]
fn detects_proliferate() {
    let feature = detect(&[entry(proliferate_spell("Contagion Clasp"), 2)]);
    assert_eq!(feature.proliferate_count, 2);
}

/// CR 702.164 / 702.90 / 702.70 are combat-damage abilities — a noncreature
/// face carrying the keyword is not a poison clock.
#[test]
fn noncreature_with_infect_does_not_count() {
    let mut face = spell("Not A Creature");
    face.keywords = vec![Keyword::Infect];
    let feature = detect(&[entry(face, 4)]);
    assert_eq!(feature.source_count, 0);
}

/// A drawback clause that poisons its OWN controller (Phyrexian Vatmother
/// shape) is not a payoff.
#[test]
fn self_poison_drawback_not_counted() {
    let feature = detect(&[entry(
        poison_spell("Self Poisoner", TargetFilter::Controller),
        4,
    )]);
    assert_eq!(feature.direct_count, 0);
}

/// Counts scale per playset copy, not per unique face.
#[test]
fn counts_scale_by_copy_count() {
    let feature = detect(&[entry(infect_creature("Glistener Elf"), 4)]);
    assert_eq!(feature.source_count, 4);
}

/// Calibration anchor: Modern Infect — 12 infect creatures + 2 proliferate over
/// 37 nonland cards → commitment ≈ 0.904. Two-sided: the lower bound is the
/// policy calibration; the upper bound pins the source weight, since
/// `weighted_sum` clamps at 1.0 and a one-sided `> 0.85` would still pass if the
/// source contribution were inflated toward saturation.
#[test]
fn modern_infect_calibration_is_pinned() {
    let deck = vec![
        entry(infect_creature("Glistener Elf"), 4),
        entry(infect_creature("Blighted Agent"), 4),
        entry(infect_creature("Phyrexian Crusader"), 4),
        entry(proliferate_spell("Contagion Clasp"), 2),
        entry(spell("Pump Spell"), 23),
    ];
    let feature = detect(&deck);
    assert_eq!(feature.source_count, 12);
    assert!(
        (0.894..=0.914).contains(&feature.commitment),
        "Modern Infect must land at 0.904 ± 0.01, got {}",
        feature.commitment
    );
}

/// Anti-calibration: a superfriends deck running proliferate but no poison
/// source stays at ≈ 0.061 — far below the floor. Two-sided so the assertion
/// pins the proliferate weight rather than leaving 5.7× of headroom to the
/// floor.
#[test]
fn superfriends_proliferate_is_pinned_below_floor() {
    let deck = vec![
        entry(proliferate_spell("Contagion Clasp"), 2),
        entry(spell("Planeswalker Filler"), 35),
    ];
    let feature = detect(&deck);
    assert!(
        (0.056..=0.066).contains(&feature.commitment),
        "proliferate-only must land at 0.061 ± 0.005, got {}",
        feature.commitment
    );
    assert!(feature.commitment < POISON_CLOCK_FLOOR);
}

/// Lands are excluded from the commitment denominator — they are not part of
/// the poison plan's nonland payload. This is a deck-modelling choice, not a
/// rules requirement, so it carries no CR citation. Two decks with identical
/// nonland content score identically no matter how many lands pad the manabase,
/// pinning the `CoreType::Land` guard in `detect`, whose removal would inflate
/// the denominator and silently lower every commitment.
#[test]
fn lands_are_excluded_from_commitment_denominator() {
    let core = |extra: Vec<DeckEntry>| {
        let mut deck = vec![
            entry(infect_creature("Glistener Elf"), 4),
            entry(infect_creature("Blighted Agent"), 4),
            entry(infect_creature("Phyrexian Crusader"), 4),
            entry(spell("Pump Spell"), 25),
        ];
        deck.extend(extra);
        detect(&deck).commitment
    };
    let landless = core(Vec::new());
    let with_lands = core(vec![entry(land("Inkmoth Nexus"), 20)]);
    assert_eq!(
        landless, with_lands,
        "20 lands must not change commitment; the nonland payload is identical"
    );
    assert!(landless > 0.0);
}

#[test]
fn control_deck_below_floor() {
    let deck = vec![entry(spell("Counterspell"), 37)];
    assert_eq!(detect(&deck).commitment, 0.0);
}

#[test]
fn commitment_clamps_to_one() {
    let deck = vec![entry(infect_creature("All Infect"), 37)];
    assert_eq!(detect(&deck).commitment, 1.0);
}

// ─── negated player scopes (CR 122.1f) ──────────────────────────────────────

fn poison_effect(target: TargetFilter) -> Effect {
    Effect::GivePlayerCounter {
        counter_kind: PlayerCounterKind::Poison,
        count: QuantityExpr::Fixed { value: 1 },
        target,
    }
}

fn not_filter(inner: TargetFilter) -> TargetFilter {
    TargetFilter::Not {
        filter: Box::new(inner),
    }
}

fn controlled_by(controller: ControllerRef) -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        controller: Some(controller),
        ..TypedFilter::default()
    })
}

/// A negated self-scope ("each player other than you") leaves every opponent
/// unmatched by the inner filter, so it IS opponent poison.
#[test]
fn negated_self_scope_is_opponent_poison() {
    for inner in [
        TargetFilter::SelfRef,
        TargetFilter::Controller,
        controlled_by(ControllerRef::You),
    ] {
        let feature = detect(&[entry(
            poison_spell("Negated Self", not_filter(inner.clone())),
            1,
        )]);
        assert_eq!(
            feature.direct_count, 1,
            "Not {{ {inner:?} }} must read as opponent poison"
        );
    }
}

/// The mirror image: negating a scope that already covers every opponent
/// resolves to the controller alone, which is a drawback, not a payoff.
#[test]
fn negated_opponent_scope_is_not_opponent_poison() {
    for inner in [
        TargetFilter::Opponent,
        TargetFilter::Player,
        TargetFilter::Any,
        controlled_by(ControllerRef::Opponent),
    ] {
        let feature = detect(&[entry(
            poison_spell("Negated Opponent", not_filter(inner.clone())),
            1,
        )]);
        assert_eq!(
            feature.direct_count, 0,
            "Not {{ {inner:?} }} resolves to the controller and must not count"
        );
    }
}

/// Double negation collapses back to the inner scope.
#[test]
fn double_negation_round_trips() {
    let poisons_opponents = detect(&[entry(
        poison_spell(
            "Twice Negated",
            not_filter(not_filter(TargetFilter::Opponent)),
        ),
        1,
    )]);
    assert_eq!(poisons_opponents.direct_count, 1);

    let poisons_self = detect(&[entry(
        poison_spell(
            "Twice Negated Self",
            not_filter(not_filter(TargetFilter::Controller)),
        ),
        1,
    )]);
    assert_eq!(poisons_self.direct_count, 0);
}

// ─── Or / And player scopes ─────────────────────────────────────────────────
//
// `Or`/`And` distribute their quantifier the OPPOSITE way between the two
// predicates: `filter_can_hit_opponent` is existential over `Or` / universal
// over `And` (some/every constraint must admit an opponent), while
// `filter_matches_every_opponent` — reached only through a negated scope — is
// universal over `Or` / existential over `And`. Each fixture below flips
// exactly one branch's outcome if its `.any`/`.all` is swapped, so a mistaken
// inversion in either predicate fails a test rather than passing silently.

fn or_filter(filters: Vec<TargetFilter>) -> TargetFilter {
    TargetFilter::Or { filters }
}

fn and_filter(filters: Vec<TargetFilter>) -> TargetFilter {
    TargetFilter::And { filters }
}

/// `filter_can_hit_opponent` over `Or` is existential: one opponent-admitting
/// branch is enough, and no branch admitting one means it cannot hit.
#[test]
fn or_scope_hits_opponent_iff_some_branch_does() {
    // Opponent branch admits an opponent → hits (swapping `.any`→`.all` here
    // would drop this, since the Controller branch does not).
    let hits = detect(&[entry(
        poison_spell(
            "Or Hits",
            or_filter(vec![TargetFilter::Controller, TargetFilter::Opponent]),
        ),
        1,
    )]);
    assert_eq!(hits.direct_count, 1);

    // No branch admits an opponent → cannot hit.
    let misses = detect(&[entry(
        poison_spell(
            "Or Misses",
            or_filter(vec![TargetFilter::Controller, TargetFilter::SelfRef]),
        ),
        1,
    )]);
    assert_eq!(misses.direct_count, 0);
}

/// `filter_can_hit_opponent` over `And` is universal (CR 109.3): every
/// constraint must admit an opponent, so one self-only constraint blocks it.
#[test]
fn and_scope_hits_opponent_iff_every_branch_does() {
    // Both constraints admit an opponent → hits.
    let hits = detect(&[entry(
        poison_spell(
            "And Hits",
            and_filter(vec![TargetFilter::Player, TargetFilter::Opponent]),
        ),
        1,
    )]);
    assert_eq!(hits.direct_count, 1);

    // Controller constraint cannot be an opponent → the conjunction cannot
    // (swapping `.all`→`.any` here would wrongly count it).
    let misses = detect(&[entry(
        poison_spell(
            "And Misses",
            and_filter(vec![TargetFilter::Opponent, TargetFilter::Controller]),
        ),
        1,
    )]);
    assert_eq!(misses.direct_count, 0);
}

/// `filter_matches_every_opponent` (reached via `Not`) over `Or` is universal:
/// `Not { Or }` hits an opponent only if some `Or` branch leaves one unmatched.
#[test]
fn negated_or_scope_pins_every_opponent_quantifier() {
    // `Or { Controller, Opponent }` covers every opponent (the Opponent branch
    // does), so its negation resolves to you alone → does not count. Swapping
    // the `Or` arm of `filter_matches_every_opponent` to `.all` would make the
    // Controller branch drag universality to false → wrongly count.
    let feature = detect(&[entry(
        poison_spell(
            "Not Or",
            not_filter(or_filter(vec![
                TargetFilter::Controller,
                TargetFilter::Opponent,
            ])),
        ),
        1,
    )]);
    assert_eq!(feature.direct_count, 0);
}

/// `filter_matches_every_opponent` over `And` is existential: `Not { And }`
/// hits an opponent unless every conjunct covers all opponents.
#[test]
fn negated_and_scope_pins_every_opponent_quantifier() {
    // `And { Opponent, Controller }` does NOT cover every opponent (the
    // Controller conjunct is not universal), so its negation leaves an opponent
    // matched → counts. Swapping the `And` arm to `.any` would make the
    // Opponent conjunct alone assert universality → wrongly drop it.
    let feature = detect(&[entry(
        poison_spell(
            "Not And",
            not_filter(and_filter(vec![
                TargetFilter::Opponent,
                TargetFilter::Controller,
            ])),
        ),
        1,
    )]);
    assert_eq!(feature.direct_count, 1);
}

// ─── modal / conditional branches (CR 700.2, CR 608.2c) ─────────────────────

/// A modal ACTIVATED ability keeps its modes in `mode_abilities`, which the
/// unconditional chain walk does not visit.
fn modal_poison_ability() -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GenericEffect {
            static_abilities: Vec::new(),
            duration: None,
            target: None,
            end_cost: None,
        },
    );
    ability.modal = Some(ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        ..ModalChoice::default()
    });
    ability.mode_abilities = vec![
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ),
        AbilityDefinition::new(
            AbilityKind::Activated,
            poison_effect(TargetFilter::Opponent),
        ),
    ];
    ability
}

/// CR 700.2: deck time asks "can this card ever poison", so a poison-only mode
/// must register — the gap that `AbilityScope::Potential` closes.
#[test]
fn detects_poison_inside_a_modal_ability_mode() {
    let mut face = creature("Modal Poisoner");
    face.abilities = vec![modal_poison_ability()];
    assert_eq!(detect(&[entry(face, 3)]).direct_count, 3);
}

/// CR 601.2b: the same card is NOT unconditional poison — nothing is decided
/// until a mode is chosen, which is the scope the live policy classifies at.
#[test]
fn modal_poison_mode_is_not_unconditional() {
    let abilities = vec![modal_poison_ability()];
    assert!(gives_opponents_poison_parts(
        &abilities,
        AbilityScope::Potential
    ));
    assert!(!gives_opponents_poison_parts(
        &abilities,
        AbilityScope::Unconditional
    ));
}

/// CR 608.2c: an "Otherwise, ..." branch is a reachable outcome, so deck-time
/// detection must see poison sitting in it.
#[test]
fn detects_poison_in_an_else_branch() {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    ability.else_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        poison_effect(TargetFilter::Opponent),
    )));

    let mut face = spell("Else Poisoner");
    face.abilities = vec![ability];
    assert_eq!(detect(&[entry(face, 2)]).direct_count, 2);
}

/// The proliferate axis walks the same scopes as the direct-poison axis.
#[test]
fn detects_proliferate_inside_a_modal_ability_mode() {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GenericEffect {
            static_abilities: Vec::new(),
            duration: None,
            target: None,
            end_cost: None,
        },
    );
    ability.modal = Some(ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 1,
        ..ModalChoice::default()
    });
    ability.mode_abilities = vec![AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Proliferate,
    )];

    let mut face = creature("Modal Proliferator");
    face.abilities = vec![ability];
    assert_eq!(detect(&[entry(face, 2)]).proliferate_count, 2);
}

// ─── combat conversion (CR 702.90b / 702.164c / 702.70a) ────────────────────

const CREATURE: &[CoreType] = &[CoreType::Creature];

/// CR 702.90b: infect converts the damage itself, so the yield is power.
#[test]
fn infect_yields_its_power_in_poison() {
    assert_eq!(poison_yield_parts(CREATURE, &[Keyword::Infect], 3), 3);
    assert_eq!(poison_yield_parts(CREATURE, &[Keyword::Infect], 0), 0);
    // A negative power deals no damage, so it converts nothing.
    assert_eq!(poison_yield_parts(CREATURE, &[Keyword::Infect], -2), 0);
}

/// CR 702.164b: total toxic value is the SUM of every toxic ability's N, and
/// CR 702.164c adds it on top of the damage's other results — so an ordinary
/// (non-infect) creature's power contributes nothing.
#[test]
fn toxic_sums_and_ignores_power() {
    assert_eq!(poison_yield_parts(CREATURE, &[Keyword::Toxic(2)], 5), 2);
    assert_eq!(
        poison_yield_parts(CREATURE, &[Keyword::Toxic(2), Keyword::Toxic(1)], 5),
        3,
        "CR 702.164b: total toxic value is the sum of all N"
    );
}

/// CR 702.70a is a separate triggered ability, and CR 702.164c is "in addition
/// to the damage's other results" — an infect creature with toxic gets both.
#[test]
fn infect_and_toxic_stack() {
    assert_eq!(
        poison_yield_parts(CREATURE, &[Keyword::Infect, Keyword::Toxic(2)], 3),
        5
    );
    assert_eq!(
        poison_yield_parts(CREATURE, &[Keyword::Poisonous(1), Keyword::Infect], 2),
        3
    );
}

#[test]
fn non_source_yields_no_poison() {
    assert_eq!(poison_yield_parts(CREATURE, &[Keyword::Flying], 7), 0);
    // CR 702.90/702.164/702.70 are all combat-damage abilities — a noncreature
    // face carrying one converts nothing.
    assert_eq!(
        poison_yield_parts(&[CoreType::Artifact], &[Keyword::Infect], 7),
        0
    );
}
