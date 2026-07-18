use engine::parser::oracle::{keyword_display_name, parse_oracle_text};
use engine::types::ability::{
    ChosenSubtypeKind, ContinuousModification, ControllerRef, DamageModification,
    DamageTargetFilter, DamageTargetPlayerScope, Effect, FilterProp, StaticCondition, TargetFilter,
    TypeFilter,
};
use engine::types::keywords::Keyword;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// CR 701.57c + CR 608.2c: Hit the Mother Lode — Discover 10 followed by a
/// conditional "create a number of tapped Treasure tokens equal to the
/// difference". The follow-up clause's bare "the difference" anaphor must bind
/// to the `Difference` of the leading `QuantityCheck` condition's operands
/// (`ObjectManaValue { CostPaidObject }` vs `Fixed(10)`), the token must be
/// `tapped: true`, and NOTHING may remain `Unimplemented`. Reverting the token
/// anaphor recognition, the shared difference binder, or the spell-seam
/// invocation flips the token count back to a dead `Variable("difference")`
/// placeholder (or drops the whole token clause to `Unimplemented`).
#[test]
fn hit_the_mother_lode_binds_difference_token_count() {
    use engine::types::ability::{
        AbilityCondition, Comparator, Effect, ObjectScope, QuantityExpr, QuantityRef,
    };

    fn any_unimplemented(def: &engine::types::ability::AbilityDefinition) -> bool {
        matches!(&*def.effect, Effect::Unimplemented { .. })
            || def.sub_ability.as_deref().is_some_and(any_unimplemented)
            || def.else_ability.as_deref().is_some_and(any_unimplemented)
    }

    let result = parse(
        "Discover 10. If the discovered card's mana value is less than 10, create a number of tapped Treasure tokens equal to the difference.",
        "Hit the Mother Lode",
        &[],
        &["Sorcery"],
        &[],
    );

    let top = result
        .abilities
        .iter()
        .find(|a| matches!(&*a.effect, Effect::Discover { .. }))
        .unwrap_or_else(|| panic!("no Discover ability parsed: {result:#?}"));
    assert!(
        !any_unimplemented(top),
        "Hit the Mother Lode must have no Unimplemented residual: {top:#?}"
    );

    let token_def = top
        .sub_ability
        .as_deref()
        .unwrap_or_else(|| panic!("Discover has no follow-up token sub: {top:#?}"));

    let expected_mv = QuantityExpr::Ref {
        qty: QuantityRef::ObjectManaValue {
            scope: ObjectScope::CostPaidObject,
        },
    };
    let expected_difference = QuantityExpr::Difference {
        left: Box::new(expected_mv.clone()),
        right: Box::new(QuantityExpr::Fixed { value: 10 }),
    };

    match &*token_def.effect {
        Effect::Token {
            name,
            tapped,
            count,
            ..
        } => {
            assert_eq!(name, "Treasure", "token is a Treasure: {token_def:#?}");
            assert!(*tapped, "Treasure tokens enter tapped: {token_def:#?}");
            assert_eq!(
                count, &expected_difference,
                "token count binds to Difference{{ObjectManaValue(CostPaidObject), Fixed(10)}}: {token_def:#?}"
            );
        }
        other => panic!("expected a Token effect sub, got {other:#?}"),
    }

    match token_def.condition.as_ref() {
        Some(AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        }) => {
            assert_eq!(
                lhs, &expected_mv,
                "condition lhs is the discovered card's mana value"
            );
            assert_eq!(*comparator, Comparator::LT, "condition uses less-than");
            assert_eq!(
                rhs,
                &QuantityExpr::Fixed { value: 10 },
                "condition rhs is 10"
            );
        }
        other => panic!("expected a QuantityCheck condition on the token sub, got {other:#?}"),
    }
}

fn parse(
    oracle_text: &str,
    card_name: &str,
    keywords: &[Keyword],
    types: &[&str],
    subtypes: &[&str],
) -> engine::parser::oracle::ParsedAbilities {
    let keyword_names: Vec<String> = keywords.iter().map(keyword_display_name).collect();
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle_text, card_name, &keyword_names, &types, &subtypes)
}

#[test]
fn snapshot_lightning_bolt() {
    let result = parse(
        "Lightning Bolt deals 3 damage to any target.",
        "Lightning Bolt",
        &[],
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_murder() {
    let result = parse("Destroy target creature.", "Murder", &[], &["Instant"], &[]);
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_counterspell() {
    let result = parse(
        "Counter target spell.",
        "Counterspell",
        &[],
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_bonesplitter() {
    let result = parse(
        "Equipped creature gets +2/+0.\nEquip {1}",
        "Bonesplitter",
        &[],
        &["Artifact"],
        &["Equipment"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_questing_beast() {
    let result = parse(
        "Vigilance, deathtouch, haste\nQuesting Beast can't be blocked by creatures with power 2 or less.\nCombat damage that would be dealt by creatures you control can't be prevented.\nWhenever Questing Beast deals combat damage to a planeswalker, it deals that much damage to target planeswalker that player controls.",
        "Questing Beast",
        &[Keyword::Vigilance, Keyword::Deathtouch, Keyword::Haste],
        &["Creature"],
        &["Beast"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_baneslayer_angel() {
    let result = parse(
        "Flying, first strike, lifelink, protection from Demons and from Dragons",
        "Baneslayer Angel",
        &[Keyword::Flying, Keyword::FirstStrike, Keyword::Lifelink],
        &["Creature"],
        &["Angel"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_jace_the_mind_sculptor() {
    let result = parse(
        "+2: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n0: Draw three cards, then put two cards from your hand on top of your library in any order.\n\u{2212}1: Return target creature to its owner's hand.\n\u{2212}12: Exile all cards from target player's library, then that player shuffles their hand into their library.",
        "Jace, the Mind Sculptor",
        &[],
        &["Planeswalker"],
        &["Jace"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_forest() {
    let result = parse("({T}: Add {G}.)", "Forest", &[], &["Land"], &["Forest"]);
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_mox_pearl() {
    let result = parse("{T}: Add {W}.", "Mox Pearl", &[], &["Artifact"], &[]);
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_llanowar_elves() {
    let result = parse(
        "{T}: Add {G}.",
        "Llanowar Elves",
        &[],
        &["Creature"],
        &["Elf", "Druid"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_rancor() {
    let result = parse(
        "Enchant creature\nEnchanted creature gets +2/+0 and has trample.\nWhen Rancor is put into a graveyard from the battlefield, return Rancor to its owner's hand.",
        "Rancor",
        &[],
        &["Enchantment"],
        &["Aura"],
    );
    insta::assert_json_snapshot!(result);
}

fn assert_same_is_true_type_recipients(affected: &Option<TargetFilter>) {
    let Some(TargetFilter::Or { filters }) = affected else {
        panic!("expected battlefield, stack, and owned-card recipient arms, got {affected:?}");
    };
    assert_eq!(filters.len(), 3);

    let TargetFilter::Typed(battlefield) = &filters[0] else {
        panic!("expected a typed battlefield recipient arm")
    };
    assert_eq!(battlefield.controller, Some(ControllerRef::You));
    assert!(battlefield.type_filters.contains(&TypeFilter::Creature));
    assert!(battlefield.properties.contains(&FilterProp::InZone {
        zone: Zone::Battlefield,
    }));

    let TargetFilter::Typed(stack) = &filters[1] else {
        panic!("expected a typed stack recipient arm")
    };
    assert_eq!(stack.controller, Some(ControllerRef::You));
    assert!(stack.type_filters.contains(&TypeFilter::Creature));
    assert!(stack
        .properties
        .contains(&FilterProp::InZone { zone: Zone::Stack }));

    let TargetFilter::Typed(cards) = &filters[2] else {
        panic!("expected a typed owned-card recipient arm")
    };
    assert_eq!(cards.controller, None);
    assert!(cards.type_filters.contains(&TypeFilter::Creature));
    assert!(cards.properties.contains(&FilterProp::Owned {
        controller: ControllerRef::You,
    }));
    assert!(cards.properties.contains(&FilterProp::RepresentedByCard));
    assert!(cards.properties.contains(&FilterProp::InAnyZone {
        zones: vec![
            Zone::Library,
            Zone::Hand,
            Zone::Graveyard,
            Zone::Stack,
            Zone::Exile,
            Zone::Command,
        ],
    }));
}

#[test]
fn arcane_adaptation_full_oracle_models_all_same_is_true_recipients() {
    let result = parse(
        "As Arcane Adaptation enters, choose a creature type.\nCreatures you control are the chosen type in addition to their other types. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.",
        "Arcane Adaptation",
        &[],
        &["Enchantment"],
        &[],
    );

    assert_eq!(result.statics.len(), 1);
    let static_def = &result.statics[0];
    assert_eq!(static_def.mode, StaticMode::Continuous);
    assert!(static_def.active_zones.is_empty());
    assert!(static_def.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType
        }
    )));
    assert_same_is_true_type_recipients(&static_def.affected);

    let unimplemented: Vec<_> = result
        .abilities
        .iter()
        .filter_map(|ability| match ability.effect.as_ref() {
            Effect::Unimplemented {
                description: Some(description),
                ..
            } => Some(description.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        unimplemented.is_empty(),
        "Arcane Adaptation's continuation is fully modeled: {unimplemented:?}"
    );
}

// CR 611.3a + CR 613.1d + CR 205.3m: Maskwood Nexus's complete two-sentence
// static reaches controlled permanents and spells plus owned cards outside the
// battlefield through one Layer-4 continuous effect.
#[test]
fn maskwood_nexus_full_oracle_models_all_same_is_true_recipients() {
    let result = parse(
        "Creatures you control are every creature type. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.\n{3}, {T}: Create a 2/2 blue Shapeshifter creature token with changeling.",
        "Maskwood Nexus",
        &[],
        &["Artifact"],
        &[],
    );

    assert_eq!(result.statics.len(), 1);
    let static_def = &result.statics[0];
    assert_eq!(static_def.mode, StaticMode::Continuous);
    assert!(static_def
        .modifications
        .iter()
        .any(|modification| matches!(modification, ContinuousModification::AddAllCreatureTypes)));
    assert_same_is_true_type_recipients(&static_def.affected);

    let unimplemented: Vec<_> = result
        .abilities
        .iter()
        .filter_map(|ability| match ability.effect.as_ref() {
            Effect::Unimplemented {
                description: Some(description),
                ..
            } => Some(description.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        unimplemented.is_empty(),
        "Maskwood Nexus's continuation is fully modeled: {unimplemented:?}"
    );
}

#[test]
fn xenograft_full_oracle_applies_chosen_type_to_creatures_you_control() {
    let result = parse(
        "As Xenograft enters, choose a creature type.\nEach creature you control is the chosen type in addition to its other types.",
        "Xenograft",
        &[],
        &["Enchantment"],
        &[],
    );

    assert_eq!(result.statics.len(), 1);
    let static_def = &result.statics[0];
    assert_eq!(static_def.mode, StaticMode::Continuous);
    assert!(static_def.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType
        }
    )));
    match &static_def.affected {
        Some(TargetFilter::Typed(filter)) => {
            assert_eq!(filter.controller, Some(ControllerRef::You));
            assert!(filter.type_filters.contains(&TypeFilter::Creature));
        }
        other => panic!("expected battlefield creature filter, got {other:?}"),
    }

    let unimplemented: Vec<_> = result
        .abilities
        .iter()
        .filter_map(|ability| match ability.effect.as_ref() {
            Effect::Unimplemented {
                description: Some(description),
                ..
            } => Some(description.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        unimplemented.is_empty(),
        "Xenograft wording should not fall through to an unimplemented ability: {unimplemented:?}"
    );
}

#[test]
fn roaming_throne_self_static_uses_creature_chosen_type() {
    let result = parse(
        "As Roaming Throne enters, choose a creature type.\nRoaming Throne is the chosen type in addition to its other types.\nIf a triggered ability of another creature you control of the chosen type triggers, it triggers an additional time.",
        "Roaming Throne",
        &[],
        &["Artifact", "Creature"],
        &["Golem"],
    );

    assert!(result.statics.iter().any(|static_def| {
        static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddChosenSubtype {
                    kind: ChosenSubtypeKind::CreatureType
                }
            )
        })
    }));
}

#[test]
fn thran_portal_self_static_uses_basic_land_chosen_type() {
    let result = parse(
        "This land enters tapped unless you control two or fewer other lands.\nAs this land enters, choose a basic land type.\nThis land is the chosen type in addition to its other types.\nMana abilities of this land cost an additional 1 life to activate.",
        "Thran Portal",
        &[],
        &["Land"],
        &[],
    );

    assert!(result.statics.iter().any(|static_def| {
        static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddChosenSubtype {
                    kind: ChosenSubtypeKind::BasicLandType
                }
            )
        })
    }));
}

#[test]
fn steely_resolve_grants_shroud_keyword_to_chosen_type_creatures() {
    let result = parse(
        "As Steely Resolve enters, choose a creature type.\nCreatures of the chosen type have shroud.",
        "Steely Resolve",
        &[],
        &["Enchantment"],
        &[],
    );

    let static_def = result
        .statics
        .iter()
        .find(|static_def| {
            static_def.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Shroud
                    }
                )
            })
        })
        .expect("expected chosen-type creatures to gain shroud as a keyword");

    assert_eq!(static_def.mode, StaticMode::Continuous);
    match &static_def.affected {
        Some(TargetFilter::Typed(filter)) => {
            assert!(filter.type_filters.contains(&TypeFilter::Creature));
            assert!(filter
                .properties
                .contains(&FilterProp::IsChosenCreatureType));
        }
        other => panic!("expected chosen-type creature filter, got {other:?}"),
    }
}

#[test]
fn stuffy_doll_damage_trigger_uses_source_chosen_player() {
    let result = parse(
        "As Stuffy Doll enters, choose a player.\nIndestructible\nWhenever Stuffy Doll is dealt damage, it deals that much damage to the chosen player.",
        "Stuffy Doll",
        &[Keyword::Indestructible],
        &["Artifact", "Creature"],
        &["Toy"],
    );

    let trigger = result
        .triggers
        .iter()
        .find_map(|trigger| trigger.execute.as_deref())
        .expect("expected damage trigger");

    match trigger.effect.as_ref() {
        Effect::DealDamage {
            target: TargetFilter::SourceChosenPlayer,
            ..
        } => {}
        other => panic!("expected damage to source chosen player, got {other:?}"),
    }
}

#[test]
fn sawhorn_nemesis_damage_replacement_scopes_to_source_chosen_player() {
    let result = parse(
        "As Sawhorn Nemesis enters, choose a player.\nIf a source would deal damage to the chosen player or a permanent they control, it deals double that damage instead.",
        "Sawhorn Nemesis",
        &[],
        &["Creature"],
        &["Dinosaur"],
    );

    let replacement = result
        .replacements
        .iter()
        .find(|replacement| replacement.damage_modification == Some(DamageModification::Double))
        .expect("expected double-damage replacement");

    assert_eq!(
        replacement.damage_target_filter,
        Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
            player: DamageTargetPlayerScope::SourceChosenPlayer,
        })
    );
}

#[test]
fn snapshot_goblin_chainwhirler() {
    let result = parse(
        "First strike\nWhen Goblin Chainwhirler enters the battlefield, it deals 1 damage to each opponent and each creature and planeswalker they control.",
        "Goblin Chainwhirler",
        &[Keyword::FirstStrike],
        &["Creature"],
        &["Goblin", "Warrior"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_wizard_class() {
    // CR 716: Class enchantment with all three level patterns:
    // Level 1 static, "When this Class becomes level 2" trigger, Level 3 continuous trigger
    let result = parse(
        "(Gain the next level as a sorcery to add its ability.)\nYou have no maximum hand size.\n{2}{U}: Level 2\nWhen this Class becomes level 2, draw two cards.\n{4}{U}: Level 3\nWhenever you draw a card, put a +1/+1 counter on target creature you control.",
        "Wizard Class",
        &[],
        &["Enchantment"],
        &["Class"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn class_structural_correctness() {
    // CR 716: Verify structural correctness of Class parsing
    let result = parse(
        "(Gain the next level as a sorcery to add its ability.)\nIf you would roll one or more dice, instead roll that many dice plus one and ignore the lowest roll.\n{1}{R}: Level 2\nWhenever you roll one or more dice, target creature you control gets +2/+0 and gains menace until end of turn.\n{2}{R}: Level 3\nCreatures you control have haste.",
        "Barbarian Class",
        &[],
        &["Enchantment"],
        &["Class"],
    );

    // 2 SetClassLevel activated abilities (Level 2 and Level 3)
    let set_class_levels: Vec<_> = result
        .abilities
        .iter()
        .filter(|a| {
            matches!(
                *a.effect,
                engine::types::ability::Effect::SetClassLevel { .. }
            )
        })
        .collect();
    assert_eq!(
        set_class_levels.len(),
        2,
        "expected 2 SetClassLevel abilities"
    );

    // Level 2 ability has ClassLevelIs { level: 1 } restriction
    let level2 = &set_class_levels[0];
    assert!(
        level2.activation_restrictions.iter().any(|r| matches!(
            r,
            engine::types::ability::ActivationRestriction::ClassLevelIs { level: 1 }
        )),
        "Level 2 ability should require ClassLevelIs {{ level: 1 }}"
    );

    // Level 3 ability has ClassLevelIs { level: 2 } restriction
    let level3 = &set_class_levels[1];
    assert!(
        level3.activation_restrictions.iter().any(|r| matches!(
            r,
            engine::types::ability::ActivationRestriction::ClassLevelIs { level: 2 }
        )),
        "Level 3 ability should require ClassLevelIs {{ level: 2 }}"
    );
}

/// CR 701.23a + CR 701.23h: Dual-filter library search lowers into one
/// `SearchLibrary` choice constrained to match each printed filter, then a
/// single destination move for the found set. Krosan Verge is the canonical
/// case: the prompt asks for two cards assignable to Forest and Plains, then
/// puts both onto the battlefield tapped.
#[test]
fn krosan_verge_lowers_to_dual_search_choice() {
    use engine::types::ability::{Effect, QuantityExpr, SearchSelectionConstraint, TargetFilter};

    let result = parse(
        "Krosan Verge enters tapped.\n{2}, {T}, Sacrifice Krosan Verge: Search your library for a Forest card and a Plains card, put them onto the battlefield tapped, then shuffle.",
        "Krosan Verge",
        &[],
        &["Land"],
        &[],
    );

    let activated = result
        .abilities
        .iter()
        .find(|a| matches!(&*a.effect, Effect::SearchLibrary { .. }))
        .expect("expected activated search ability");

    let mut effects: Vec<&'static str> = Vec::new();
    let mut cursor: Option<&engine::types::ability::AbilityDefinition> = Some(activated);
    while let Some(def) = cursor {
        let label = match &*def.effect {
            Effect::SearchLibrary { .. } => "SearchLibrary",
            Effect::ChangeZone {
                destination,
                enter_tapped,
                ..
            } => {
                assert_eq!(
                    *destination,
                    engine::types::zones::Zone::Battlefield,
                    "ChangeZone destination should be Battlefield",
                );
                assert!(enter_tapped.is_tapped(), "found lands should enter tapped");
                "ChangeZone"
            }
            Effect::Shuffle { .. } => "Shuffle",
            other => panic!("unexpected effect in chain: {other:?}"),
        };
        effects.push(label);
        cursor = def.sub_ability.as_deref();
    }

    assert_eq!(
        effects,
        vec!["SearchLibrary", "ChangeZone", "Shuffle"],
        "expected one constrained search, one move, then shuffle"
    );
    let Effect::SearchLibrary {
        filter,
        count,
        selection_constraint,
        ..
    } = &*activated.effect
    else {
        panic!("expected SearchLibrary");
    };
    assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
    let TargetFilter::Or { filters } = filter else {
        panic!("expected Or filter, got {filter:?}");
    };
    assert_eq!(
        filters
            .iter()
            .filter_map(|filter| match filter {
                TargetFilter::Typed(tf) => tf.get_subtype().map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["Forest".to_string(), "Plains".to_string()],
        "expected Forest and Plains subtype filters"
    );
    assert!(matches!(
        selection_constraint,
        SearchSelectionConstraint::MatchEachFilter { filters: constrained }
            if constrained == filters
    ));
}

/// CR 701.23a + CR 107.1: Corpse Harvester exercises the Hand-destination
/// variant of the dual-search primitive: "a Zombie card and a Swamp card,
/// reveal them, put them into your hand, then shuffle." Proves that the
/// building block is not Krosan-Verge-specific.
#[test]
fn corpse_harvester_lowers_to_dual_search_into_hand() {
    use engine::types::ability::Effect;

    let result = parse(
        "{1}{B}, {T}, Sacrifice a creature: Search your library for a Zombie card and a Swamp card, reveal them, put them into your hand, then shuffle.",
        "Corpse Harvester",
        &[],
        &["Creature"],
        &["Zombie"],
    );

    let activated = result
        .abilities
        .iter()
        .find(|a| matches!(&*a.effect, Effect::SearchLibrary { .. }))
        .expect("expected activated search ability");

    let mut cursor: Option<&engine::types::ability::AbilityDefinition> = Some(activated);
    let mut change_zone_count = 0;
    while let Some(def) = cursor {
        match &*def.effect {
            Effect::SearchLibrary { .. } => {}
            Effect::ChangeZone { destination, .. } => {
                assert_eq!(
                    *destination,
                    engine::types::zones::Zone::Hand,
                    "Corpse Harvester destination should be Hand",
                );
                change_zone_count += 1;
            }
            Effect::Shuffle { .. } => {}
            other => panic!("unexpected effect in chain: {other:?}"),
        }
        cursor = def.sub_ability.as_deref();
    }

    assert_eq!(
        change_zone_count, 1,
        "expected one ChangeZone for found set"
    );
}

#[test]
fn snapshot_force_of_despair() {
    // CR 118.9 + CR 102.1: leading-if conditional alternative cost. The
    // "If it's not your turn" gate must bind to the casting option's
    // `condition` slot as `Not(IsYourTurn)` — never `null`.
    let result = parse(
        "If it's not your turn, you may exile a black card from your hand rather than pay this spell's mana cost.\nDestroy all creatures that entered this turn.",
        "Force of Despair",
        &[],
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

// CR 614.1 + CR 614.12 + CR 303.4 + CR 613.1d + CR 613.1f + CR 113.10:
// Return-as-Aura dies trigger. The dies-trigger sub-effect chain MUST emit
// `Effect::ReturnAsAura` (not `Effect::Unimplemented { name: "it's" }`).
// Snapshot tests lock in the parsed shape for the three known class members
// so a future parser refactor cannot silently regress the outer effect.
#[test]
fn snapshot_old_growth_troll() {
    let result = parse(
        "Trample\nWhen Old-Growth Troll dies, if it was a creature, return it to the battlefield. It's an Aura enchantment with enchant Forest you control and \"Enchanted Forest has '{T}: Add {G}{G}' and '{1}, {T}, Sacrifice this land: Create a tapped 4/4 green Troll Warrior creature token with trample.'\"",
        "Old-Growth Troll",
        &[Keyword::Trample],
        &["Creature"],
        &["Troll"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_bronzehide_lion() {
    // CR 614.1 + CR 113.10: Bronzehide's "and it loses all other abilities"
    // is pre-split by the chunk-splitter into a sibling `GenericEffect`,
    // which `try_fold_loses_other_sibling` folds back into the
    // `Effect::ReturnAsAura.grants` list. The final IR must have
    // `RemoveAllAbilities` at `grants[0]`.
    let result = parse(
        "{G}{W}: This creature gains indestructible until end of turn.\nWhen this creature dies, return it to the battlefield. It's an Aura enchantment with enchant creature you control and \"{G}{W}: Enchanted creature gains indestructible until end of turn,\" and it loses all other abilities.",
        "Bronzehide Lion",
        &[],
        &["Creature"],
        &["Cat"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_harold_and_bob_first_numens() {
    // CR 614.1 + CR 113.10: Harold's "<card name> loses all other abilities"
    // appears in the SAME chunk as the `It's an Aura ...` sentence (no
    // intervening ` and ` connector). Detected in-combinator by
    // `parse_loses_clause` and inserted at `grants[0]` directly.
    let result = parse(
        "Vigilance, reach\nWhen Harold and Bob dies, if it was a creature, return it to the battlefield. It's an Aura enchantment with enchant Forest you control and \"Enchanted Forest has '{T}: Add three mana of any one color. You get two rad counters.'\" Harold and Bob loses all other abilities.",
        "Harold and Bob, First Numens",
        &[Keyword::Vigilance, Keyword::Reach],
        &["Legendary", "Creature"],
        &["Troll", "Warrior"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn inverted_as_long_as_flash_grant_attaches_condition() {
    // CR 601.3b + CR 702.8a + CR 611.3a: The inverted conditional flash-grant
    // "As long as <cond>, you may cast [type] spells as though they had flash"
    // must lower to a `CastWithKeyword { Flash }` static carrying the condition
    // — not silently collapse into a bare conditionless Continuous fallback.
    let result = parse(
        "As long as it's your turn, you may cast creature spells as though they had flash.",
        "Test Inverted Flash Grant",
        &[],
        &["Enchantment"],
        &[],
    );

    let static_def = result
        .statics
        .iter()
        .find(|static_def| {
            matches!(
                static_def.mode,
                StaticMode::CastWithKeyword {
                    keyword: Keyword::Flash
                }
            )
        })
        .expect("expected inverted flash grant to lower to CastWithKeyword { Flash }");

    // "it's your turn" is recognized by parse_static_condition → DuringYourTurn,
    // not just a generic Unrecognized fallback.
    assert!(
        matches!(static_def.condition, Some(StaticCondition::DuringYourTurn)),
        "expected DuringYourTurn condition on the CastWithKeyword static, \
         got condition: {:#?}",
        static_def.condition
    );
}

/// CR 111.3 + CR 111.4: a created token's quoted inline ability text is the
/// token's own "text", not a static clause of the host spell. Before the fix,
/// the Priority-7 static gate matched a static-shaped marker (e.g. "can't
/// block", "can't be blocked") INSIDE the quoted token ability and routed the
/// whole spell to the static parser, producing `abilities: []` and a bogus
/// static. Masking double-quoted spans for spell-line static classification
/// restores the spell's own effect chain (>=1 ability, no bogus static).
#[test]
fn token_grant_spells_no_longer_misroute_to_static() {
    // (oracle, name, types, subtypes)
    let cases: &[(&str, &str, &[&str], &[&str])] = &[
        (
            "You gain X life. Create X 1/1 colorless Phyrexian Mite artifact creature \
             tokens with toxic 1 and \"This token can't block.\" If X is 5 or more, \
             destroy all other creatures.",
            "White Sun's Twilight",
            &["Sorcery"],
            &[],
        ),
        (
            "Create two 1/1 black Rat creature tokens with \"This token can't block.\"",
            "Pest Problem",
            &["Instant"],
            &["Adventure"],
        ),
        (
            "Create two 1/1 blue Fish creature tokens with \"This token can't be \
             blocked.\" Then for each kind of counter among creatures you control, \
             put a counter of that kind on either of those tokens.",
            "Exotic Pets",
            &["Instant"],
            &[],
        ),
        (
            "Return up to one target creature to its owner's hand. Create a 1/1 \
             colorless Spirit creature token with \"This token can't block or be \
             blocked by non-Spirit creatures.\"",
            "Lost in the Spirit World",
            &["Sorcery"],
            &[],
        ),
    ];

    for (oracle, name, types, subtypes) in cases {
        let result = parse(oracle, name, &[], types, subtypes);
        assert!(
            !result.abilities.is_empty(),
            "{name}: expected >=1 spell ability after masking the quoted token text, \
             got abilities={:#?}",
            result.abilities
        );
        // The bogus host-line static (a CantBlock the gate manufactured from the
        // token's quoted text) must be gone. The token's own can't-block lives
        // on the created token inside the Effect::Token, not in `statics`.
        assert!(
            !result
                .statics
                .iter()
                .any(|s| matches!(s.mode, StaticMode::CantBlock)),
            "{name}: a bogus host-line CantBlock static must not be produced, \
             got statics={:#?}",
            result.statics
        );
    }
}

/// Negative control: Brood Birthing's "have \"…\"" grant marker is OUTSIDE the
/// quoted span, so masking must NOT change its parse — it still lowers to its
/// functional `GrantAbility` static (the sacrifice-for-mana grant). This is the
/// invariant the masking fix must preserve.
#[test]
fn brood_birthing_grant_static_unchanged_by_masking() {
    use engine::types::ability::ContinuousModification;

    let result = parse(
        "If you control an Eldrazi Spawn, create three 0/1 colorless Eldrazi Spawn \
         creature tokens. They have \"Sacrifice this token: Add {C}.\" Otherwise, \
         create one of those tokens.",
        "Brood Birthing",
        &[],
        &["Sorcery"],
        &[],
    );

    assert_eq!(
        result.statics.len(),
        1,
        "Brood Birthing keeps exactly one static (the GrantAbility grant): {:#?}",
        result.statics
    );
    let grants_ability = matches!(result.statics[0].mode, StaticMode::Continuous)
        && result.statics[0]
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::GrantAbility { .. }));
    assert!(
        grants_ability,
        "Brood Birthing's static must remain the functional GrantAbility grant, \
         got: {:#?}",
        result.statics[0]
    );
}
