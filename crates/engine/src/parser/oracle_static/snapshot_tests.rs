/// Snapshot tests locking current static parser output before/after the IR split.
/// These verify behavioral parity: identical snapshots before and after the
/// `parse_static_line_ir` / `lower_static_ir` refactor.
use super::prelude::*;
use super::support::*;
use super::type_change::*;
use super::*;

#[test]
fn static_continuous_buff() {
    let def = parse_static_line("Creatures you control get +1/+1.").unwrap();
    insta::assert_json_snapshot!(def);
}

#[test]
fn static_cda_power_hand_size() {
    let def = parse_static_line("~'s power is equal to the number of cards in your hand.").unwrap();
    insta::assert_json_snapshot!(def);
}

#[test]
fn static_conditional_as_long_as() {
    let def = parse_static_line("~ gets +2/+2 as long as you control another creature.").unwrap();
    insta::assert_json_snapshot!(def);
}

#[test]
fn static_granted_keyword() {
    let def = parse_static_line("Creatures you control have flying.").unwrap();
    insta::assert_json_snapshot!(def);
}

/// Issue #327: "of that color" anaphor (post-Choose) is the equivalent of
/// "of the chosen color" and must lower to a filter with IsChosenColor.
#[test]
fn parse_chosen_qualifier_subject_recognizes_that_color_anaphor() {
    let lower = "creatures of that color".to_string();
    let tp = TextPair::new("creatures of that color", &lower);
    let filter = parse_chosen_qualifier_subject(&tp).expect("anaphor form should parse");
    match filter {
        TargetFilter::Typed(tf) => {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::IsChosenColor)),
                "expected IsChosenColor in properties, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed creature filter, got {other:?}"),
    }
}

/// Issue #327: "of the chosen color" (explicit form) must still produce
/// the same IsChosenColor filter so the two grammatical forms unify.
#[test]
fn parse_chosen_qualifier_subject_recognizes_chosen_color_explicit() {
    let lower = "creatures of the chosen color".to_string();
    let tp = TextPair::new("creatures of the chosen color", &lower);
    let filter = parse_chosen_qualifier_subject(&tp).expect("explicit form should parse");
    match filter {
        TargetFilter::Typed(tf) => {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::IsChosenColor)),
                "expected IsChosenColor in properties, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed creature filter, got {other:?}"),
    }
}

/// CR 613.1d + CR 613.1g: `parse_pronoun_becomes_type_static` on the
/// canonical effect clause must emit AddType for each type and dynamic
/// set-P/T scoped to the object's mana value (Recipient scope).
#[test]
fn pronoun_becomes_type_static_dynamic_pt_by_mana_value() {
    let text = "it's an artifact creature with power and toughness each equal to its mana value";
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let def = parse_pronoun_becomes_type_static(&tp, text).expect("expected a become-type static");
    let mods = &def.modifications;
    assert!(
        mods.contains(&ContinuousModification::AddType {
            core_type: CoreType::Artifact
        }),
        "expected AddType(Artifact) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddType {
            core_type: CoreType::Creature
        }),
        "expected AddType(Creature) in {mods:?}"
    );
    let mv_ref = QuantityExpr::Ref {
        qty: QuantityRef::ObjectManaValue {
            scope: ObjectScope::Recipient,
        },
    };
    assert!(
        mods.contains(&ContinuousModification::SetPowerDynamic {
            value: mv_ref.clone()
        }),
        "expected SetPowerDynamic(ObjectManaValue Recipient) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::SetToughnessDynamic { value: mv_ref }),
        "expected SetToughnessDynamic(ObjectManaValue Recipient) in {mods:?}"
    );
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
}

/// CR 205.2 + CR 613.1d + CR 613.4b: March of the Machines (global,
/// no controller scope) — every noncreature artifact becomes an
/// artifact creature with dynamic mana-value P/T.
#[test]
fn parses_march_of_the_machines_static() {
    let text = "Each noncreature artifact is an artifact creature with power and \
                    toughness each equal to its mana value.";
    let def = parse_static_line(text).expect("March of the Machines must parse");

    // Membership-style assertions throughout (S3) to hedge against TypedFilter normalization.
    let TargetFilter::Typed(ref tf) = def.affected.as_ref().expect("affected must be set") else {
        panic!("expected TargetFilter::Typed, got {:?}", def.affected);
    };

    assert!(
        tf.type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Artifact)),
        "expected Artifact in type_filters; got {:?}",
        tf.type_filters
    );
    assert!(
        tf.type_filters.iter().any(|f| matches!(
            f,
            TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Creature)
        )),
        "expected Non(Creature) in type_filters; got {:?}",
        tf.type_filters
    );
    assert!(
        tf.controller.is_none(),
        "global — no controller scope expected for March"
    );

    let mods = &def.modifications;
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddType {
                core_type: CoreType::Creature
            }
        )),
        "expected AddType(Creature); got {:?}",
        mods
    );
    let expected_mv = QuantityExpr::Ref {
        qty: QuantityRef::ObjectManaValue {
            scope: ObjectScope::Recipient,
        },
    };
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::SetPowerDynamic { value } if value == &expected_mv
        )),
        "expected SetPowerDynamic with ObjectManaValue(Recipient); got {:?}",
        mods
    );
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::SetToughnessDynamic { value } if value == &expected_mv
        )),
        "expected SetToughnessDynamic with ObjectManaValue(Recipient); got {:?}",
        mods
    );
}

/// CR 205.2 + CR 613.1d + CR 613.4b + CR 109.5: Karn-shape, controller-scoped
/// (`you control`). The `controller` field on the typed filter must be set.
#[test]
fn parses_karn_each_noncreature_artifact_you_control_static() {
    let text = "Each noncreature artifact you control is an artifact creature with \
                    power and toughness each equal to its mana value.";
    let def = parse_static_line(text).expect("Karn-shape must parse");

    let TargetFilter::Typed(ref tf) = def.affected.as_ref().expect("affected must be set") else {
        panic!("expected TargetFilter::Typed, got {:?}", def.affected);
    };

    assert!(
        tf.type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Artifact)),
        "expected Artifact; got {:?}",
        tf.type_filters
    );
    assert!(
        tf.type_filters.iter().any(|f| matches!(
            f,
            TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Creature)
        )),
        "expected Non(Creature); got {:?}",
        tf.type_filters
    );
    assert_eq!(
        tf.controller,
        Some(ControllerRef::You),
        "Karn restricts to You-controlled"
    );
}

/// Sibling subject "each artifact" (no "noncreature ") is out of scope for
/// this arm — the parser must NOT capture it.
#[test]
fn rejects_each_artifact_without_noncreature_prefix() {
    let text = "Each artifact you control is a creature with power and toughness each \
                    equal to its mana value.";
    let lower = text.to_ascii_lowercase();
    let tp = TextPair::new(text, &lower);
    assert!(
        parse_each_noncreature_subject_is_creature_with_pt_mv(&tp, text).is_none(),
        "the each-noncreature arm must not capture 'each artifact' subjects"
    );
}

/// Bludgeon Brawl shape: the comma after "noncreature" defeats the
/// "each noncreature " prefix strip — the subject is "noncreature, non-Equipment
/// artifact", not "noncreature artifact". This arm must NOT capture it.
#[test]
fn rejects_bludgeon_brawl_shape() {
    let text = "Each noncreature, non-Equipment artifact is an Equipment with equip {X} \
                    and \"Equipped creature gets +X/+0,\" where X is that artifact's mana value.";
    let lower = text.to_ascii_lowercase();
    let tp = TextPair::new(text, &lower);
    assert!(
        parse_each_noncreature_subject_is_creature_with_pt_mv(&tp, text).is_none(),
        "the each-noncreature arm must not capture the Bludgeon Brawl shape \
             (comma after 'noncreature')"
    );
}

/// "Each noncreature land" — `Land` is not in the `Artifact | Enchantment`
/// whitelist at STEP C.2; this arm must NOT capture it.
#[test]
fn rejects_each_noncreature_land() {
    let text = "Each noncreature land is a creature with power and toughness each equal to its \
             mana value.";
    let lower = text.to_ascii_lowercase();
    let tp = TextPair::new(text, &lower);
    assert!(
        parse_each_noncreature_subject_is_creature_with_pt_mv(&tp, text).is_none(),
        "the each-noncreature arm must reject 'land' as affirmative type"
    );
}

/// "Each noncreature spell" — `parse_type_filter_word` maps "spell" to
/// `TypeFilter::Card` (CR 112.1), which is not in the `Artifact | Enchantment`
/// whitelist; this arm must NOT capture it.
#[test]
fn rejects_each_noncreature_spell() {
    let text = "Each noncreature spell costs {2} more to cast.";
    let lower = text.to_ascii_lowercase();
    let tp = TextPair::new(text, &lower);
    assert!(
        parse_each_noncreature_subject_is_creature_with_pt_mv(&tp, text).is_none(),
        "the each-noncreature arm must reject 'spell' as affirmative type"
    );
}

/// Synthetic Enchantment-class sibling of March of the Machines (no real
/// printed card uses this exact shape, but the parser must compose for it
/// because Enchantment is in the C.2 whitelist alongside Artifact). Asserts
/// affirmative type, Non(Creature), You-controller, and the dynamic-P/T mods.
#[test]
fn accepts_each_noncreature_enchantment_synthetic() {
    let text = "Each noncreature enchantment you control is an enchantment creature with \
                    power and toughness each equal to its mana value.";
    let def = parse_static_line(text).expect("synthetic enchantment shape must parse");

    let TargetFilter::Typed(ref tf) = def.affected.as_ref().expect("affected must be set") else {
        panic!("expected TargetFilter::Typed, got {:?}", def.affected);
    };

    assert!(
        tf.type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Enchantment)),
        "expected Enchantment in type_filters; got {:?}",
        tf.type_filters
    );
    assert!(
        tf.type_filters.iter().any(|f| matches!(
            f,
            TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Creature)
        )),
        "expected Non(Creature) in type_filters; got {:?}",
        tf.type_filters
    );
    assert_eq!(
        tf.controller,
        Some(ControllerRef::You),
        "synthetic Enchantment shape uses 'you control'"
    );

    let mods = &def.modifications;
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddType {
                core_type: CoreType::Creature
            }
        )),
        "expected AddType(Creature); got {:?}",
        mods
    );
    let expected_mv = QuantityExpr::Ref {
        qty: QuantityRef::ObjectManaValue {
            scope: ObjectScope::Recipient,
        },
    };
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::SetPowerDynamic { value } if value == &expected_mv
        )),
        "expected SetPowerDynamic(ObjectManaValue Recipient); got {:?}",
        mods
    );
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::SetToughnessDynamic { value } if value == &expected_mv
        )),
        "expected SetToughnessDynamic(ObjectManaValue Recipient); got {:?}",
        mods
    );
}

/// S1 regression: CR 611.3a — a trailing " as long as <condition>" clause
/// must be peeled before the subject/effect parse and re-attached to the
/// resulting `StaticDefinition`. Without STEP A, the condition would leak
/// into the dynamic-P/T tail and `def.condition` would be `None`.
#[test]
fn condition_clause_preserved_in_each_noncreature_static() {
    let text = "Each noncreature artifact is an artifact creature with power and \
                    toughness each equal to its mana value as long as you control a creature.";
    let def = parse_static_line(text).expect("conditional March-shape must parse");
    assert!(
        def.condition.is_some(),
        "expected condition to be attached; got None on def {:?}",
        def
    );
}

/// Animate Artifact: the full inverted-form line must parse to a single
/// animation static (AddType + dynamic P/T) with a non-null condition —
/// NOT a `RemoveType { Creature }` driven by the condition body.
#[test]
fn animate_artifact_inverted_form_animates_not_removes_type() {
    let def = parse_static_line(
        "As long as enchanted artifact isn't a creature, it's an artifact creature \
             with power and toughness each equal to its mana value.",
    )
    .expect("expected a static for Animate Artifact");
    let mods = &def.modifications;
    assert!(
        mods.iter()
            .all(|m| !matches!(m, ContinuousModification::RemoveType { .. })),
        "Animate Artifact must not remove a type, got {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddType {
            core_type: CoreType::Creature
        }),
        "expected AddType(Creature) in {mods:?}"
    );
    assert!(
        mods.iter()
            .any(|m| matches!(m, ContinuousModification::SetPowerDynamic { .. })),
        "expected dynamic P/T in {mods:?}"
    );
    assert!(
        def.condition.is_some(),
        "expected a non-null condition (clears Condition_AsLongAs warning)"
    );
}

/// Regression: the layer-4 `isn't a` type-removal path must still fire
/// when `isn't a creature` IS the effect (the 26-God class, e.g. Erebos),
/// producing `RemoveType { Creature }` plus the devotion condition.
#[test]
fn isnt_a_creature_as_effect_still_removes_type() {
    let def = parse_static_line(
        "As long as your devotion to black is less than five, \
             Erebos, God of the Dead isn't a creature.",
    )
    .expect("expected a static for the Erebos-class line");
    assert!(
        def.modifications
            .contains(&ContinuousModification::RemoveType {
                core_type: CoreType::Creature
            }),
        "expected RemoveType(Creature) in {:?}",
        def.modifications
    );
    assert!(
        def.condition.is_some(),
        "expected the devotion condition attached"
    );
}

/// CR 107.4f (Phyrexian shape) + K'rrik 2024-06-07 ruling: K'rrik's
/// granted permission "For each {B} in a cost, you may pay 2 life
/// rather than pay that mana" must lower to `PayLifeAsColoredMana`
/// targeting the correct color. Guards the parser regression that the
/// runtime tests in `casting.rs` cannot catch (they synthesize the
/// `StaticDefinition` directly, bypassing this combinator).
#[test]
fn parse_pay_life_as_colored_mana_for_krrik() {
    let def =
        parse_static_line("For each {B} in a cost, you may pay 2 life rather than pay that mana.")
            .expect("K'rrik line must parse to a StaticDefinition");
    assert_eq!(
        def.mode,
        StaticMode::PayLifeAsColoredMana {
            color: crate::types::mana::ManaColor::Black,
        },
    );
    assert!(matches!(def.affected, Some(TargetFilter::Controller)));
}

/// The combinator must reject other colors only by routing the wrong
/// `ManaColor`, not by silently dropping. Verifies the {R} variant
/// lowers symmetrically — guards against the `alt(...)` branch order
/// regressing color identification.
#[test]
fn parse_pay_life_as_colored_mana_red_variant() {
    let def =
        parse_static_line("For each {R} in a cost, you may pay 2 life rather than pay that mana.")
            .expect("Red-variant line must parse to a StaticDefinition");
    assert_eq!(
        def.mode,
        StaticMode::PayLifeAsColoredMana {
            color: crate::types::mana::ManaColor::Red,
        },
    );
}

/// CR 107.4f: only the 2-life Phyrexian shape exists in print today.
/// Other life values must fall through to `Unimplemented` (return
/// `None`) so coverage surfaces the gap rather than silently casting
/// the substitution at a wrong rate.
#[test]
fn parse_pay_life_as_colored_mana_rejects_non_two_life() {
    assert!(
        parse_static_line("For each {B} in a cost, you may pay 3 life rather than pay that mana.")
            .is_none(),
        "non-2-life variants must not bind to PayLifeAsColoredMana"
    );
}

// === CR 117.1a + CR 102.1 + CR 109.5: "only during X turn(s)" parser tests ===

/// CR 109.5: Fires of Invention emits the source-relative binding
/// (`NotDuringYourTurn`) and does NOT emit a CantActivateDuring static.
/// Regression guard — parser rewrite must preserve bit-for-bit behavior.
#[test]
fn parses_fires_of_invention_cast_only_during_your_turn() {
    let defs = parse_static_line_multi("You can cast spells only during your turn.");
    let cast = defs
        .iter()
        .find(|d| matches!(&d.mode, StaticMode::CantCastDuring { .. }))
        .expect("expected CantCastDuring");
    match &cast.mode {
        StaticMode::CantCastDuring { who, when } => {
            assert_eq!(*who, ProhibitionScope::Controller);
            assert_eq!(*when, CastingProhibitionCondition::NotDuringYourTurn);
        }
        _ => unreachable!(),
    }
    assert!(
        !defs
            .iter()
            .any(|d| matches!(&d.mode, StaticMode::CantActivateDuring { .. })),
        "Fires of Invention does NOT emit an activate-during static"
    );
}

/// CR 102.1: Dosan emits `CantCastDuring(AllPlayers, NotDuringAffectedPlayersTurn)`
/// and per its 2004-12-01 ruling does NOT emit a CantActivateDuring static.
#[test]
fn parses_dosan_cast_only_during_their_own_turns() {
    let defs = parse_static_line_multi("Players can cast spells only during their own turns.");
    assert_eq!(defs.len(), 1, "expected exactly one static, got {defs:?}");
    let cast = &defs[0];
    match &cast.mode {
        StaticMode::CantCastDuring { who, when } => {
            assert_eq!(*who, ProhibitionScope::AllPlayers);
            assert_eq!(
                *when,
                CastingProhibitionCondition::NotDuringAffectedPlayersTurn
            );
        }
        other => panic!(
            "expected CantCastDuring(AllPlayers, NotDuringAffectedPlayersTurn), got {other:?}"
        ),
    }
    // Per Dosan's 2004-12-01 ruling: "doesn't stop activated or triggered abilities".
    assert!(
        !defs
            .iter()
            .any(|d| matches!(&d.mode, StaticMode::CantActivateDuring { .. })),
        "Dosan must NOT emit an activate-during static"
    );
}

/// CR 601.2 + CR 602.5: City of Solitude emits BOTH halves (cast + activate)
/// with `NotDuringAffectedPlayersTurn`, and the activate-half has
/// `ActivationExemption::None` per its 2009-10-01 ruling.
#[test]
fn parses_city_of_solitude_cast_and_activate_only_during_their_own_turns() {
    let oracle = "Players can cast spells and activate abilities only during their own turns.";
    let defs = parse_static_line_multi(oracle);
    assert_eq!(
        defs.len(),
        2,
        "City of Solitude must emit cast-half + activate-half, got {defs:?}"
    );
    let cast = defs
        .iter()
        .find(|d| matches!(&d.mode, StaticMode::CantCastDuring { .. }))
        .expect("cast-half");
    let activate = defs
        .iter()
        .find(|d| matches!(&d.mode, StaticMode::CantActivateDuring { .. }))
        .expect("activate-half");
    match &cast.mode {
        StaticMode::CantCastDuring { who, when } => {
            assert_eq!(*who, ProhibitionScope::AllPlayers);
            assert_eq!(
                *when,
                CastingProhibitionCondition::NotDuringAffectedPlayersTurn
            );
        }
        _ => unreachable!(),
    }
    match &activate.mode {
        StaticMode::CantActivateDuring {
            who,
            when,
            exemption,
        } => {
            assert_eq!(*who, ProhibitionScope::AllPlayers);
            assert_eq!(
                *when,
                CastingProhibitionCondition::NotDuringAffectedPlayersTurn
            );
            // CR 605.1a: City of Solitude does NOT exempt mana abilities (2009-10-01 ruling).
            assert_eq!(*exemption, ActivationExemption::None);
        }
        _ => unreachable!(),
    }
    // Both emitted statics carry the full Oracle text on `description`.
    assert_eq!(cast.description.as_deref(), Some(oracle));
    assert_eq!(activate.description.as_deref(), Some(oracle));
}

/// CR 117.1: Teferi-class regression — "only any time they could cast a sorcery"
/// remains a `NotSorcerySpeed` condition; the parser rewrite must not regress it.
#[test]
fn parses_teferi_cast_only_at_sorcery_speed_regression() {
    let defs = parse_static_line_multi(
        "Each opponent can cast spells only any time they could cast a sorcery.",
    );
    let s = defs
        .iter()
        .find(|d| matches!(&d.mode, StaticMode::CantCastDuring { .. }))
        .expect("expected CantCastDuring for Teferi");
    match &s.mode {
        StaticMode::CantCastDuring { who, when } => {
            assert_eq!(*who, ProhibitionScope::Opponents);
            assert_eq!(*when, CastingProhibitionCondition::NotSorcerySpeed);
        }
        _ => unreachable!(),
    }
}

/// CR 603.2d: Damage-caused trigger doubler (Wayta, Trainer Prodigy).
#[test]
fn parses_wayta_damage_caused_doubler() {
    let def = parse_static_line(
        "If a creature you control being dealt damage causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.",
    )
    .expect("expected DoubleTriggers static for Wayta");
    assert_eq!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::ControlledCreatureDealtDamage
        }
    );
    assert!(
        def.affected.is_none(),
        "bare 'a permanent you control' must not add a redundant affected filter"
    );
}

/// CR 603.2d: Source-restricted trigger doubler (Splinter, Radical Rat).
/// "If a triggered ability of a Ninja creature you control triggers, that
/// ability triggers an additional time." The cause is unrestricted (`Any`),
/// but the doubler's `affected` filter MUST narrow to Ninja creatures the
/// controller controls — otherwise every controlled permanent's triggers
/// double, not just Ninjas'.
#[test]
fn parses_splinter_source_restricted_doubler() {
    let def = parse_static_line(
            "If a triggered ability of a Ninja creature you control triggers, that ability triggers an additional time.",
        )
        .expect("expected DoubleTriggers static for Splinter");
    assert_eq!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::Any
        }
    );
    let affected = def
        .affected
        .as_ref()
        .expect("source-restricted doubler must carry an `affected` filter");
    match affected {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype("Ninja".to_string())),
                "expected Ninja subtype constraint, got {:?}",
                tf.type_filters
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

/// CR 603.2d: A disjunctive source ("a Shaman or another Wizard
/// you control", Harmonic Prodigy) is a top-level type union sharing one
/// trailing controller scope. The doubler `affected` filter MUST be the
/// controller-scoped `Or` of both disjuncts — doubling triggers from a Shaman
/// *or* a Wizard you control, and nothing else.
///
/// Discriminating: the prior single-clause model dropped "or Wizard" AND the
/// "you control" scope, so a naive parse yielded a controller-less
/// `Subtype(Shaman)`; the conservative guard then suppressed the filter
/// entirely (`affected == None`), which over-doubles *every* controlled
/// permanent's triggers. Both wrong outcomes fail the assertions below.
#[test]
fn harmonic_prodigy_disjunctive_source_doubles_shaman_or_wizard() {
    let def = parse_static_line(
            "If a triggered ability of a Shaman or another Wizard you control triggers, that ability triggers an additional time.",
        )
        .expect("expected DoubleTriggers static for Harmonic Prodigy");
    assert_eq!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::Any
        }
    );
    let Some(TargetFilter::Or { filters }) = def.affected.as_ref() else {
        panic!(
            "disjunctive source must produce a controller-scoped `Or` of both \
             disjuncts, got {:?}",
            def.affected
        );
    };
    assert_eq!(filters.len(), 2, "expected two disjuncts, got {filters:?}");

    // Every branch must be a `Typed` clause scoped to the doubler's controller —
    // the shared "you control" suffix is distributed to both, so the doubler
    // never doubles an opponent's Shaman or Wizard.
    let mut subtypes = Vec::new();
    let mut wizard_is_another = false;
    let mut shaman_is_plain = false;
    for branch in filters {
        let TargetFilter::Typed(tf) = branch else {
            panic!("expected Typed disjunct, got {branch:?}");
        };
        assert_eq!(
            tf.controller,
            Some(ControllerRef::You),
            "each disjunct must keep the shared `you control` scope, got {tf:?}"
        );
        for type_filter in &tf.type_filters {
            if let TypeFilter::Subtype(name) = type_filter {
                subtypes.push(name.clone());
                let is_another = tf.properties.contains(&FilterProp::Another);
                // The "another Wizard" designation carries FilterProp::Another,
                // excluding the doubler itself from its own Wizard clause...
                if name == "Wizard" && is_another {
                    wizard_is_another = true;
                }
                // ...but "a Shaman" has no such designation. Pin the asymmetry:
                // disjuncts are parsed independently and only the controller is
                // distributed across the union, so a leg-local "another" must NOT
                // leak onto the Shaman branch. Guards a future refactor that might
                // route the union through property distribution.
                if name == "Shaman" && !is_another {
                    shaman_is_plain = true;
                }
            }
        }
    }
    subtypes.sort();
    assert_eq!(
        subtypes,
        vec!["Shaman".to_string(), "Wizard".to_string()],
        "expected a Shaman branch and a Wizard branch"
    );
    assert!(
        wizard_is_another,
        "the Wizard branch must carry the `another` designation"
    );
    assert!(
        shaman_is_plain,
        "the Shaman branch must NOT carry the `another` designation"
    );
}

/// CR 603.6a: Panharmonicon's source is the unrestricted "a permanent you
/// control" — controller match alone suffices, so `affected` stays `None`.
/// Regression guard: the source-filter extraction must NOT populate
/// `affected` for a bare controlled-permanent source.
#[test]
fn panharmonicon_doubler_has_no_source_filter() {
    let def = parse_static_line(
            "If an artifact or creature entering causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.",
        )
        .expect("expected DoubleTriggers static for Panharmonicon");
    assert!(
        matches!(
            def.mode,
            StaticMode::DoubleTriggers {
                cause: TriggerCause::EntersBattlefield { .. }
            }
        ),
        "expected EntersBattlefield cause, got {:?}",
        def.mode
    );
    assert!(
        def.affected.is_none(),
        "bare 'permanent you control' source must leave affected None, got {:?}",
        def.affected
    );
}

/// CR 603.2d: Echoes of Eternity — a second real disjunctive doubler beyond
/// Harmonic Prodigy. "a colorless spell you control or another colorless
/// permanent you control" must produce a controller-scoped two-branch `Or`, with
/// the trailing "another ... permanent" disjunct carrying FilterProp::Another.
/// Discriminating: the prior single-clause-or-bail behavior left `affected` None
/// here too, over-doubling every controlled trigger.
#[test]
fn echoes_of_eternity_colorless_disjunctive_doubler() {
    let def = parse_static_line(
            "If a triggered ability of a colorless spell you control or another colorless permanent you control triggers, that ability triggers an additional time.",
        )
        .expect("expected DoubleTriggers static for Echoes of Eternity");
    assert_eq!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::Any
        }
    );
    let Some(TargetFilter::Or { filters }) = def.affected.as_ref() else {
        panic!(
            "disjunctive source must produce a controller-scoped `Or`, got {:?}",
            def.affected
        );
    };
    assert_eq!(filters.len(), 2, "expected two disjuncts, got {filters:?}");
    let mut any_branch_is_another = false;
    for branch in filters {
        let TargetFilter::Typed(tf) = branch else {
            panic!("expected Typed disjunct, got {branch:?}");
        };
        assert_eq!(
            tf.controller,
            Some(ControllerRef::You),
            "each disjunct must keep the shared `you control` scope, got {tf:?}"
        );
        if tf.properties.contains(&FilterProp::Another) {
            any_branch_is_another = true;
        }
    }
    assert!(
        any_branch_is_another,
        "the `another colorless permanent` branch must carry FilterProp::Another"
    );
}

/// CR 603.2d: Delney, Streetwise Lookout — the doubler source "a creature you
/// control with power 2 or less" embeds "or" inside a power suffix, NOT a type
/// disjunction. The fix must keep it a single restrictive clause (Creature +
/// you control + the power restriction), never mistaking the suffix "or" for a
/// disjunct connector. Discriminating: the prior `scan_contains(.., "or ")`
/// guard matched the suffix "or" and bailed to `None`, over-doubling every
/// controlled trigger; a buggy connector split would instead yield an `Or`.
#[test]
fn delney_power_suffix_or_is_not_a_disjunction() {
    let def = parse_static_line(
            "If a triggered ability of a creature you control with power 2 or less triggers, that ability triggers an additional time.",
        )
        .expect("expected DoubleTriggers static for Delney");
    assert_eq!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::Any
        }
    );
    let Some(TargetFilter::Typed(tf)) = def.affected.as_ref() else {
        panic!(
            "`power N or less` must stay one typed clause, not None or Or; got {:?}",
            def.affected
        );
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "expected a Creature restriction, got {:?}",
        tf.type_filters
    );
    assert!(
        !tf.properties.is_empty(),
        "expected the `power 2 or less` restriction to be parsed, got no properties"
    );
}

/// CR 603.2d: A three-way Oxford-comma type union exercises the `, or ` and bare
/// `, ` connector arms of `doubler_disjunct_connector`. No live card uses them
/// yet, but they share the union path with the two-way "or" form — this locks
/// them so a future simplification dropping those arms is caught. Each of the
/// three disjuncts must become its own controller-scoped branch.
#[test]
fn three_way_oxford_disjunctive_doubler_source() {
    let def = parse_static_line(
            "If a triggered ability of a Shaman, a Wizard, or a Cleric you control triggers, that ability triggers an additional time.",
        )
        .expect("expected DoubleTriggers static");
    let Some(TargetFilter::Or { filters }) = def.affected.as_ref() else {
        panic!("expected a three-branch `Or`, got {:?}", def.affected);
    };
    assert_eq!(
        filters.len(),
        3,
        "expected three disjuncts, got {filters:?}"
    );
    let mut subtypes = Vec::new();
    for branch in filters {
        let TargetFilter::Typed(tf) = branch else {
            panic!("expected Typed disjunct, got {branch:?}");
        };
        assert_eq!(
            tf.controller,
            Some(ControllerRef::You),
            "every disjunct must keep the shared `you control` scope, got {tf:?}"
        );
        for type_filter in &tf.type_filters {
            if let TypeFilter::Subtype(name) = type_filter {
                subtypes.push(name.clone());
            }
        }
    }
    subtypes.sort();
    assert_eq!(
        subtypes,
        vec![
            "Cleric".to_string(),
            "Shaman".to_string(),
            "Wizard".to_string()
        ]
    );
}
