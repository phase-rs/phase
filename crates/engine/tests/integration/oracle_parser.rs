use engine::parser::oracle::{keyword_display_name, parse_oracle_text};
use engine::types::ability::{
    ChosenSubtypeKind, ContinuousModification, ControllerRef, DamageModification,
    DamageTargetFilter, DamageTargetPlayerScope, Effect, FilterProp, TargetFilter, TypeFilter,
};
use engine::types::keywords::Keyword;
use engine::types::statics::StaticMode;

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

#[test]
fn arcane_adaptation_full_oracle_splits_battlefield_static_and_unimplemented_tail() {
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
    assert_eq!(
        unimplemented,
        vec![
            "The same is true for creature spells you control and creature cards you own that aren't on the battlefield."
        ]
    );
}

// CR 613.1d + CR 205.3m: Maskwood Nexus's full Oracle text shares Arcane
// Adaptation's two-sentence shape — a battlefield static plus the
// "the same is true for creature spells / creature cards you own that aren't
// on the battlefield" tail. The dispatcher in `oracle.rs` must split the
// battlefield static (Layer 4 `AddAllCreatureTypes` on creatures you
// control) from the non-battlefield tail, which is parked as
// `Unimplemented` because layer-applied type changes outside the
// battlefield aren't modeled.
#[test]
fn maskwood_nexus_full_oracle_splits_battlefield_static_and_unimplemented_tail() {
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
    assert_eq!(
        unimplemented,
        vec![
            "The same is true for creature spells you control and creature cards you own that aren't on the battlefield."
        ]
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
                assert!(*enter_tapped, "found lands should enter tapped");
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
