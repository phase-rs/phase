/// Snapshot tests locking current static parser output before/after the IR split.
/// These verify behavioral parity: identical snapshots before and after the
/// `parse_static_line_ir` / `lower_static_ir` refactor.
use super::prelude::*;
use super::support::*;
use super::type_change::*;
use super::*;

#[test]
fn contextual_hand_static_routing_has_complete_supported_positive_reaches() {
    const HAND: &str = "Target opponent reveals their hand. You choose a noncreature, nonland card from it. That player discards that card.";
    let expected_static = StaticDefinition::continuous()
        .affected(TargetFilter::Typed(TypedFilter::creature()))
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }])
        .description("Creatures have flying.".into());
    assert_eq!(
        parse_static_line("Creatures have flying."),
        Some(expected_static.clone())
    );
    let filter = TargetFilter::Typed(
        TypedFilter::card()
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature)))
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
    );
    let mut discard = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DiscardCard {
            count: 1,
            target: TargetFilter::ParentTarget,
        },
    );
    discard.sub_link = crate::types::ability::SubAbilityLink::SequentialSibling;
    let mut reveal = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RevealHand {
            target: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            card_filter: filter.clone(),
            count: None,
            selection: crate::types::ability::CardSelectionMode::Chosen,
            choice_optional: false,
            reveal: true,
        },
    )
    .sub_ability(discard)
    .description(HAND.into());
    reveal.declares_chosen_group = Some(crate::types::ability::ChosenGroupId(0x8000_0000));
    let expected = crate::parser::oracle::ParsedAbilities {
        abilities: vec![reveal],
        triggers: vec![],
        statics: vec![],
        replacements: vec![],
        extracted_keywords: vec![],
        modal: None,
        additional_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        solve_condition: None,
        strive_cost: None,
        parse_warnings: vec![],
    };
    let mut doc =
        crate::parser::oracle::parse_oracle_ir(HAND, "Duress", &[], &["Sorcery".into()], &[]);
    let actual = crate::parser::oracle::lower_oracle_ir(&mut doc);
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(doc.items.len(), 1);
    let crate::parser::oracle_ir::doc::OracleNodeIr::Spell(spell) = &doc.items[0].node else {
        panic!("supported hand reaches spell IR");
    };
    assert_eq!(spell.body.clauses.len(), 3);
    assert!(
        matches!(spell.body.clauses[1].disposition.followup(), Some(crate::parser::oracle_ir::ast::ContinuationAst::RevealHandFilter { card_filter: Some(actual), choice_optional: false, binding: crate::parser::oracle_ir::ast::RevealChoiceBinding::FromIt }) if actual == &filter)
    );
    assert_eq!(actual.abilities.len(), 1);
    assert!(
        actual.statics.is_empty()
            && actual.triggers.is_empty()
            && actual.replacements.is_empty()
            && actual.extracted_keywords.is_empty()
    );
    let head = &actual.abilities[0];
    let expected_filter = TargetFilter::Typed(
        TypedFilter::card()
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature)))
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
    );
    assert!(
        matches!(head.effect.as_ref(), Effect::RevealHand { card_filter, .. } if card_filter == &expected_filter)
    );
    assert!(matches!(
        head.sub_ability.as_ref().unwrap().effect.as_ref(),
        Effect::DiscardCard {
            count: 1,
            target: TargetFilter::ParentTarget
        }
    ));
    assert!(head.sub_ability.as_ref().unwrap().sub_ability.is_none());
    assert!(!crate::parser::oracle::has_unimplemented(head));
    assert!(parse_static_line(HAND).is_none());
    insta::assert_json_snapshot!("contextual_hand_static_positive", expected_static);
    insta::assert_json_snapshot!("contextual_hand_static_routing_ir", doc);
    insta::assert_json_snapshot!("contextual_hand_static_routing_lowered", actual);
}

#[test]
fn nested_comparator_static_semantics_before_snapshot() {
    // SHAPE: synthetic grammar controls, CR 109.2 + CR 109.2a + CR 202.3.
    for (source, quantity) in [
        ("Creatures with mana value less than or equal to the number of lands you control have flying.",
         QuantityRef::ObjectCount { filter: TargetFilter::Typed(TypedFilter {
             type_filters: vec![TypeFilter::Land], controller: Some(ControllerRef::You), properties: vec![],
         }) }),
        ("Creatures with mana value less than or equal to the number of cards in your graveyard have flying.",
         QuantityRef::ZoneCardCount { zone: crate::types::ability::ZoneRef::Graveyard,
             scope: crate::types::ability::CountScope::Controller, card_types: vec![], filter: None }),
    ] {
        let expected = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::LE, value: QuantityExpr::Ref { qty: quantity },
            }])))
            .modifications(vec![ContinuousModification::AddKeyword { keyword: Keyword::Flying }])
            .description(source.into());
        let actual = parse_static_line_multi(source);
        assert_eq!(actual, vec![expected.clone()]);
        assert_eq!(parse_static_line(source), Some(expected));
        assert!(parse_static_line(&source.replace(" have flying", " inexplicably have flying")).is_none());
        insta::assert_json_snapshot!(format!("nested_comparator_static_{}", if source == "Creatures with mana value less than or equal to the number of lands you control have flying." { "battlefield" } else { "graveyard" }), actual);
    }
}

#[test]
fn relative_complete_cda_attached_and_cost_statics() {
    let sources = [
        "~'s power and toughness are each equal to the number of permanents you control that are Spirits and/or enchantments.",
        "~'s power and toughness are each equal to the number of creatures you control that are Fungi and/or Saprolings.",
        "Enchanted creature has flying, lifelink, and protection from Vampires, and it gets +X/+X, where X is the number of permanents you control that are Spirits and/or enchantments.",
        "This spell costs {X} less to cast, where X is the number of cards in your graveyard that are instant cards, sorcery cards, and/or have an Adventure.",
    ];
    for (index, source) in sources.into_iter().enumerate() {
        let definitions = parse_static_line_multi(source);
        assert_eq!(definitions.len(), 1);
        let definition = &definitions[0];
        assert_eq!(definition.description.as_deref(), Some(source));
        assert_eq!(definition.condition, None);
        if index < 3 {
            let (head, legs) = if index == 1 {
                (
                    TypeFilter::Creature,
                    vec![
                        TypeFilter::Subtype("Fungus".into()),
                        TypeFilter::Subtype("Saproling".into()),
                    ],
                )
            } else {
                (
                    TypeFilter::Permanent,
                    vec![
                        TypeFilter::Subtype("Spirit".into()),
                        TypeFilter::Enchantment,
                    ],
                )
            };
            let value = QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::AnyOf(legs), head],
                        controller: Some(ControllerRef::You),
                        properties: vec![],
                    }),
                },
            };
            let modifiers = if index == 2 {
                vec![
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Flying,
                    },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Lifelink,
                    },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Protection(
                            crate::types::keywords::ProtectionTarget::CardType("vampires".into()),
                        ),
                    },
                    ContinuousModification::AddDynamicPower {
                        value: value.clone(),
                    },
                    ContinuousModification::AddDynamicToughness { value },
                ]
            } else {
                vec![
                    ContinuousModification::SetDynamicPower {
                        value: value.clone(),
                    },
                    ContinuousModification::SetDynamicToughness { value },
                ]
            };
            let expected = StaticDefinition::continuous()
                .affected(if index == 2 {
                    TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                    )
                } else {
                    TargetFilter::SelfRef
                })
                .modifications(modifiers)
                .description(source.into());
            let expected = if index == 2 { expected } else { expected.cda() };
            assert_eq!(definition, &expected);
        } else {
            let count = QuantityRef::ZoneCardCount {
                zone: crate::types::ability::ZoneRef::Graveyard,
                scope: CountScope::Controller,
                card_types: vec![],
                filter: Some(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                        TargetFilter::Typed(
                            TypedFilter::card().properties(vec![FilterProp::HasAdventure]),
                        ),
                    ],
                }),
            };
            let expected = StaticDefinition::new(StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::generic(1),
                spell_filter: None,
                dynamic_count: Some(count),
                reach: CostReductionReach::SpillsToGeneric,
            })
            .affected(TargetFilter::SelfRef)
            .active_zones(crate::types::zones::self_spell_cost_mod_active_zones())
            .description(source.into());
            assert_eq!(definition, &expected);
        }
        insta::assert_json_snapshot!(format!("relative_complete_static_{index}"), definitions);
    }
}

#[test]
fn retained_noncreature_prohibition_cost_and_nth_static() {
    let sources = [
        "Noncreature spells you cast cost {1} less to cast.",
        "Noncreature spells with mana value 4 or greater can't be cast.",
        "Noncreature spells with {X} in their mana costs can't be cast.",
        "The first noncreature spell each opponent casts each turn costs {1} more to cast.",
    ];
    let definitions: Vec<_> = sources
        .into_iter()
        .map(|source| parse_static_line(source).unwrap())
        .collect();
    let expected = vec![
        TypeFilter::Card,
        TypeFilter::Non(Box::new(TypeFilter::Creature)),
    ];
    for def in &definitions {
        let filter = match &def.mode {
            StaticMode::ModifyCost {
                spell_filter: Some(filter),
                ..
            } => filter,
            StaticMode::CantBeCast { .. } => def.affected.as_ref().unwrap(),
            other => panic!("unexpected static: {other:?}"),
        };
        assert!(matches!(filter, TargetFilter::Typed(typed) if typed.type_filters == expected));
    }
    insta::assert_json_snapshot!(
        "retained_noncreature_prohibition_cost_and_nth_static",
        definitions
    );
}

#[test]
fn jev_chosen_quality_haste() {
    let def = parse_static_line("Each creature with mana value of the chosen quality has haste.")
        .unwrap();
    insta::assert_json_snapshot!(def);
}

#[test]
fn repeated_population_dismissal() {
    let def = parse_static_line("This spell costs {1} less to cast for each artifact you control and each artifact card in your hand.").unwrap();
    insta::assert_json_snapshot!(def);
}

#[test]
fn repeated_population_leech() {
    let def = parse_static_line("This spell costs {1} less to cast for each Cave you control and each Cave card in your graveyard.").unwrap();
    insta::assert_json_snapshot!(def);
}

#[test]
fn coordinated_population_sailors() {
    let def = parse_static_line("This spell costs {1} less to cast for each card you own in exile and in your graveyard that's an instant card, a sorcery card, or a card that has an Adventure.").unwrap();
    insta::assert_json_snapshot!(def);
}

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
fn elenda_life_threshold_static() {
    let def = parse_static_line(
        "Elenda gets an additional +5/+5 as long as your life total is at least 10 greater than your starting life total.",
    )
    .expect("Elenda's life-threshold static must parse");
    insta::assert_json_snapshot!("elenda_life_threshold_static", &def);
}

#[test]
fn static_granted_keyword() {
    let def = parse_static_line("Creatures you control have flying.").unwrap();
    insta::assert_json_snapshot!(def);
}

#[test]
fn static_extra_blockers_group_grant() {
    let def = parse_static_line(
        "Each creature you control can block an additional creature each combat.",
    )
    .unwrap();
    insta::assert_json_snapshot!("static_extra_blockers_group_grant", &def);
}

#[test]
fn static_tiered_enters_with_additional_counters() {
    let defs = parse_static_line_multi(
        "Each other Vehicle and creature you control enters with an additional +1/+1 counter on it if its mana value is 4 or less. Otherwise, it enters with three additional +1/+1 counters on it.",
    );
    insta::assert_json_snapshot!("static_tiered_enters_with_additional_counters", &defs);
}

#[test]
fn havi_historic_graveyard_gate() {
    let def = parse_static_line(
        "Havi has indestructible as long as there are four or more historic cards in your graveyard. (Artifacts, legendaries, and Sagas are historic.)",
    )
    .expect("Havi's historic threshold static must parse");
    insta::assert_json_snapshot!("havi_historic_graveyard_gate", &def);
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

/// CR 603.2d + CR 604.1: "triggers while <condition>, that ability triggers an
/// additional time" attaches the parsed condition to the doubler; the same text
/// without the "while" clause stays unconditional.
#[test]
fn double_triggers_while_condition_is_attached() {
    for text in [
        "If a triggered ability of another Shrine you control triggers while you control six or more Shrines, that ability triggers an additional time.",
        "If a triggered ability of a creature you control triggers while you control three or more creatures, that ability triggers an additional time.",
    ] {
        let def = parse_static_line(text).expect("expected DoubleTriggers static");
        assert!(matches!(def.mode, StaticMode::DoubleTriggers { .. }));
        assert!(
            matches!(
                def.condition,
                Some(StaticCondition::QuantityComparison { .. })
            ),
            "condition must be a typed quantity gate for {text:?}, got {:?}",
            def.condition
        );
    }
    let unconditional = parse_static_line(
        "If a triggered ability of a creature you control triggers, that ability triggers an additional time.",
    )
    .expect("expected DoubleTriggers static");
    assert_eq!(unconditional.condition, None);
    // An unparseable gate declines instead of doubling unconditionally.
    assert!(parse_static_line(
        "If a triggered ability of a creature you control triggers while the moon is full, that ability triggers an additional time.",
    )
    .is_none());
    // A gate whose continuation is not ", that ability trigger…" (Roaming
    // Throne's "it triggers an additional time") cannot be anchored; it declines
    // instead of dropping the printed "while" clause into an unconditional
    // doubler. The ungated "it triggers" spelling stays a paired positive.
    assert!(parse_static_line(
        "If a triggered ability of a creature you control triggers while you control three or more creatures, it triggers an additional time.",
    )
    .is_none());
    assert!(parse_static_line(
        "If a triggered ability of a creature you control triggers, it triggers an additional time.",
    )
    .is_some());
    // The gate runs up to the doubler's own continuation, so a condition that
    // contains a comma is not truncated at its first ", " into a weaker gate:
    // the whole text is judged, and an unparseable whole declines.
    assert!(parse_static_line(
        "If a triggered ability of a creature you control triggers while you control three or more creatures, and the moon is full, that ability triggers an additional time.",
    )
    .is_none());
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
    let Some(TargetFilter::Typed(filter)) = def.affected.as_ref() else {
        panic!(
            "bare 'a permanent you control' must preserve its permanent source scope, got {:?}",
            def.affected
        );
    };
    assert_eq!(filter.type_filters, [TypeFilter::Permanent]);
    assert_eq!(filter.controller, Some(ControllerRef::You));
}

/// CR 603.2d + CR 601.2 + CR 707.10: Cast-or-copy-caused trigger doubler
/// (Veyran, Voice of Duality). "If you casting or copying an instant or
/// sorcery spell causes ..." must produce a `ControllerCastOrCopiedSpell`
/// cause narrowed to instants and sorceries — never the unrestricted `Any`
/// fallback, which wrongly doubled attack/ETB triggers (issue #5291).
#[test]
fn parses_veyran_cast_or_copy_caused_doubler() {
    let def = parse_static_line(
        "If you casting or copying an instant or sorcery spell causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.",
    )
    .expect("expected DoubleTriggers static for Veyran");
    assert_eq!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::ControllerCastOrCopiedSpell {
                core_types: vec![CoreType::Instant, CoreType::Sorcery]
            }
        }
    );
    let Some(TargetFilter::Typed(filter)) = def.affected.as_ref() else {
        panic!(
            "Veyran must preserve its permanent source scope, got {:?}",
            def.affected
        );
    };
    assert_eq!(filter.type_filters, [TypeFilter::Permanent]);
    assert_eq!(filter.controller, Some(ControllerRef::You));
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

/// CR 603.6a: Panharmonicon's source is "a permanent you control". Preserve
/// that permanent-domain restriction so its controller check cannot admit a
/// spell-source trigger.
#[test]
fn panharmonicon_doubler_preserves_permanent_source_filter() {
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
    let Some(TargetFilter::Typed(filter)) = def.affected.as_ref() else {
        panic!(
            "bare 'permanent you control' source must preserve its scope, got {:?}",
            def.affected
        );
    };
    assert_eq!(filter.type_filters, [TypeFilter::Permanent]);
    assert_eq!(filter.controller, Some(ControllerRef::You));
}

/// CR 603.2d + CR 603.6a + CR 603.6c: Gandalf the White — legendary OR
/// artifact entering/leaving doubles controlled triggers (issue #5332).
#[test]
fn gandalf_the_white_doubler_static() {
    let def = parse_static_line(
        "If a legendary permanent or an artifact entering or leaving the battlefield causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.",
    )
    .expect("expected DoubleTriggers static for Gandalf the White");
    assert_eq!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::BattlefieldTransition {
                enter: true,
                leave: true,
                qualifiers: vec![
                    ZoneChangeQualifier::Supertype(Supertype::Legendary),
                    ZoneChangeQualifier::CoreType(CoreType::Artifact),
                ],
            }
        },
        "Gandalf must parse as legendary-or-artifact battlefield transition doubling"
    );
    let Some(TargetFilter::Typed(filter)) = def.affected.as_ref() else {
        panic!(
            "Gandalf's permanent source must preserve its scope, got {:?}",
            def.affected
        );
    };
    assert_eq!(filter.type_filters, [TypeFilter::Permanent]);
    assert_eq!(filter.controller, Some(ControllerRef::You));
}

#[test]
fn hama_pashar_room_ability_doubler_static() {
    let def = parse_static_line("Room abilities of dungeons you own trigger an additional time.")
        .expect("expected DoubleTriggers static for Hama Pashar");
    assert!(matches!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::RoomEntered
        }
    ));
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

/// CR 603.2d + CR 301.5a: Cloud, Midgar Mercenary — an inverted "As long as ~
/// is equipped, if a triggered ability of ~ or an Equipment attached to it
/// triggers, that ability triggers an additional time." BOTH the affected SCOPE
/// (self + attached Equipment) and the equipped CONDITION must survive the
/// inverted-as-long-as split.
///
/// Discriminating: before the fix the DoubleTriggers branch dropped `affected`
/// (SelfRef was rejected as non-restrictive, and "an Equipment attached to it"
/// had no dedicated arm) AND the split condition was never re-attached
/// (`condition: None`) — so the doubler over-fired on every trigger and never
/// gated on being equipped. Both assertions below flip to failure on revert.
#[test]
fn cloud_midgar_mercenary_self_and_equipment_doubler_gated_on_equipped() {
    let def = parse_static_line(
        "As long as ~ is equipped, if a triggered ability of ~ or an Equipment attached to it triggers, that ability triggers an additional time.",
    )
    .expect("expected DoubleTriggers static for Cloud");
    assert_eq!(
        def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::Any
        }
    );
    // Gate: the "as long as ~ is equipped" clause must re-attach as the condition.
    assert_eq!(
        def.condition,
        Some(StaticCondition::SourceIsEquipped),
        "equipped gate must survive the inverted-as-long-as split"
    );
    // Scope: Or[SelfRef, Typed(Equipment, AttachedToSource)].
    let Some(TargetFilter::Or { filters }) = def.affected.as_ref() else {
        panic!(
            "affected must be an Or of self + attached Equipment, got {:?}",
            def.affected
        );
    };
    assert_eq!(filters.len(), 2, "expected two disjuncts, got {filters:?}");
    assert!(
        filters.contains(&TargetFilter::SelfRef),
        "self-reference disjunct (`~`) missing: {filters:?}"
    );
    let equip = filters
        .iter()
        .find_map(|f| match f {
            TargetFilter::Typed(tf) => Some(tf),
            _ => None,
        })
        .expect("attached-Equipment disjunct missing");
    assert!(
        equip
            .type_filters
            .contains(&TypeFilter::Subtype("Equipment".to_string())),
        "expected Equipment subtype, got {:?}",
        equip.type_filters
    );
    assert!(
        equip.properties.contains(&FilterProp::AttachedTo {
            to: crate::types::ability::AttachmentReferent::Source
        }),
        "expected AttachedToSource property, got {:?}",
        equip.properties
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

/// CR 613.1d + CR 613.4b + CR 613.1f (issue #2363): Grand Master of Flowers —
/// "As long as ~ has seven or more loyalty counters on him, he's a 7/7 Dragon
/// God creature with flying and indestructible."
/// The parser must emit SetPower(7), SetToughness(7), SetCardTypes([Creature]),
/// AddSubtype(Dragon), AddSubtype(God), AddKeyword(Flying),
/// AddKeyword(Indestructible), plus a non-null HasCounters condition.
#[test]
fn grand_master_of_flowers_becomes_777_dragon_god_creature() {
    let text = "As long as ~ has seven or more loyalty counters on him, \
                he's a 7/7 Dragon God creature with flying and indestructible.";
    let def = parse_static_line(text)
        .unwrap_or_else(|| panic!("Grand Master animation static must parse; text = {text:?}"));
    let mods = &def.modifications;

    assert!(
        mods.contains(&ContinuousModification::SetPower { value: 7 }),
        "expected SetPower(7) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::SetToughness { value: 7 }),
        "expected SetToughness(7) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::SetCardTypes {
            core_types: vec![CoreType::Creature]
        }),
        "expected SetCardTypes([Creature]) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddSubtype {
            subtype: "Dragon".to_string()
        }),
        "expected AddSubtype(Dragon) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddSubtype {
            subtype: "God".to_string()
        }),
        "expected AddSubtype(God) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Flying
        }),
        "expected AddKeyword(Flying) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Indestructible
        }),
        "expected AddKeyword(Indestructible) in {mods:?}"
    );
    assert!(
        def.condition.is_some(),
        "expected a HasCounters condition (loyalty counter threshold); got None"
    );
}

#[test]
fn goddric_celebration_grants_complete_dragon_characteristics() {
    let text = "Celebration — As long as two or more nonland permanents entered the battlefield under your control this turn, ~ is a Dragon with base power and toughness 4/4, flying, and \"{R}: Dragons you control get +1/+0 until end of turn.\" (It loses all other creature types.)";
    let def = parse_static_line(text).expect("Goddric Celebration static must parse");
    let mods = &def.modifications;
    assert!(mods.contains(&ContinuousModification::RemoveAllSubtypes {
        set: SubtypeSet::Creature
    }));
    assert!(mods.contains(&ContinuousModification::AddSubtype {
        subtype: "Dragon".to_string()
    }));
    assert!(mods.contains(&ContinuousModification::SetPower { value: 4 }));
    assert!(mods.contains(&ContinuousModification::SetToughness { value: 4 }));
    assert!(mods.contains(&ContinuousModification::AddKeyword {
        keyword: Keyword::Flying
    }));
    assert!(mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::GrantAbility { definition }
            if definition.kind == AbilityKind::Activated
    )));
    assert!(!mods.contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def.condition.is_some());
}

/// CR 613.1d + CR 613.4b + CR 613.1f (issue #2363): "she's a" gendered pronoun
/// variant — confirms the parser accepts feminine pronouns on cards like future
/// Planeswalkers that become creatures.
#[test]
fn gendered_pronoun_she_becomes_creature_static() {
    let text = "As long as ~ has three or more loyalty counters on her, \
                she's a 3/3 Warrior creature with first strike.";
    let def = parse_static_line(text)
        .unwrap_or_else(|| panic!("gendered she's-a animation static must parse; text = {text:?}"));
    let mods = &def.modifications;

    assert!(
        mods.contains(&ContinuousModification::SetPower { value: 3 }),
        "expected SetPower(3) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::SetToughness { value: 3 }),
        "expected SetToughness(3) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::SetCardTypes {
            core_types: vec![CoreType::Creature]
        }),
        "expected SetCardTypes([Creature]) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddSubtype {
            subtype: "Warrior".to_string()
        }),
        "expected AddSubtype(Warrior) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::FirstStrike
        }),
        "expected AddKeyword(FirstStrike) in {mods:?}"
    );
    assert!(
        def.condition.is_some(),
        "expected a HasCounters condition; got None"
    );
}

/// CR 613.1d + CR 613.4b + CR 613.1f: neutral-plural "they're a" pronoun
/// variant stays on the same composable animation path as he/she/it forms.
#[test]
fn neutral_plural_pronoun_they_becomes_creature_static() {
    let text = "they're a 4/4 Angel creature with vigilance.";
    let def = parse_static_line(text).unwrap_or_else(|| {
        panic!("neutral-plural they're-a animation static must parse; text = {text:?}")
    });
    let mods = &def.modifications;

    assert!(
        mods.contains(&ContinuousModification::SetPower { value: 4 }),
        "expected SetPower(4) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::SetToughness { value: 4 }),
        "expected SetToughness(4) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::SetCardTypes {
            core_types: vec![CoreType::Creature]
        }),
        "expected SetCardTypes([Creature]) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddSubtype {
            subtype: "Angel".to_string()
        }),
        "expected AddSubtype(Angel) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Vigilance
        }),
        "expected AddKeyword(Vigilance) in {mods:?}"
    );
}

#[test]
fn locket_name_population() {
    let def = parse_static_line("Spells you cast cost {1} less to cast for each card with the same name as that spell in your graveyard.").unwrap();
    insta::assert_json_snapshot!("locket_name_population", &def);
}
