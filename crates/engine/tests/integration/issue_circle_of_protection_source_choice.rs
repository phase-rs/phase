//! Circle of Protection / Rune of Protection cycles — "a <color/type> source of
//! your choice ... prevent that damage" must parse into a `PreventDamage` effect
//! whose `damage_source_filter` is a QUALIFIED `ChosenDamageSource` (retaining
//! the color/type qualifier), not a bare/unconstrained shield. Verifies the real
//! production parser path against verbatim Scryfall Oracle text.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityKind, Effect, FilterProp, TargetFilter, TypeFilter};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;

// Verbatim Scryfall Oracle text (fetched 2026-07).
const COP_RED: &str =
    "{1}: The next time a red source of your choice would deal damage to you this turn, prevent that damage.";
const RUNE_LANDS: &str = "{W}: The next time a land source of your choice would deal damage to you this turn, prevent that damage.\nCycling {2} ({2}, Discard this card: Draw a card.)";

/// Extract the single `PreventDamage` effect from a parsed card.
fn prevent_damage_source_filter(parsed: &engine::parser::oracle::ParsedAbilities) -> TargetFilter {
    parsed
        .abilities
        .iter()
        .find_map(|a| match a.effect.as_ref() {
            Effect::PreventDamage {
                damage_source_filter: Some(f),
                ..
            } => Some(f.clone()),
            _ => None,
        })
        .expect("card must have a PreventDamage ability with a source filter")
}

#[test]
fn circle_of_protection_red_parses_qualified_chosen_source() {
    let parsed = parse_oracle_text(
        COP_RED,
        "Circle of Protection: Red",
        &[],
        &["Enchantment".to_string()],
        &[],
    );

    // The prevention lives on an activated ability.
    assert!(
        parsed
            .abilities
            .iter()
            .any(|a| matches!(a.kind, AbilityKind::Activated)),
        "Circle of Protection: Red's prevention must be an activated ability"
    );

    match prevent_damage_source_filter(&parsed) {
        TargetFilter::ChosenDamageSource {
            chooser: Some(engine::types::ability::Chooser::Controller),
            filter: Some(inner),
        } => match *inner {
            TargetFilter::Typed(tf) => assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::HasColor {
                        color: ManaColor::Red
                    }
                )),
                "qualifier must constrain to red sources, got {tf:?}"
            ),
            other => panic!("expected Typed red qualifier, got {other:?}"),
        },
        other => panic!("expected qualified ChosenDamageSource, got {other:?}"),
    }
}

#[test]
fn rune_of_protection_lands_parses_type_qualifier_and_keeps_cycling_separate() {
    let parsed = parse_oracle_text(
        RUNE_LANDS,
        "Rune of Protection: Lands",
        &["Cycling".to_string()],
        &["Enchantment".to_string()],
        &[],
    );

    match prevent_damage_source_filter(&parsed) {
        TargetFilter::ChosenDamageSource {
            chooser: Some(engine::types::ability::Chooser::Controller),
            filter: Some(inner),
        } => match *inner {
            TargetFilter::Typed(tf) => assert!(
                tf.type_filters.contains(&TypeFilter::Land),
                "qualifier must constrain to Land sources, got {tf:?}"
            ),
            other => panic!("expected Typed Land qualifier, got {other:?}"),
        },
        other => panic!("expected qualified ChosenDamageSource, got {other:?}"),
    }

    // Cycling is a keyword-only line: the parser decomposes it into
    // `extracted_keywords` (as `Keyword::Cycling`), NOT a second entry in
    // `abilities`. Assert it is present there, independent of the prevention
    // clause — the prevention line is not swallowed and Cycling is not lost.
    assert!(
        parsed
            .extracted_keywords
            .iter()
            .any(|kw| matches!(kw, Keyword::Cycling(_))),
        "Rune of Protection: Lands must extract Cycling as a keyword alongside its prevention ability, got keywords: {:?}",
        parsed.extracted_keywords,
    );
}

#[test]
fn paid_pilgrim_protects_the_entire_wumpus_opportunity_then_expires_on_use() {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    let mut scenario = GameScenario::new();
    let pilgrim = scenario.add_creature_from_oracle(P0, "Pilgrim of Virtue", 1, 3,
        "Protection from black\n{W}, Sacrifice this creature: The next time a black source of your choice would deal damage this turn, prevent that damage.").id();
    let wumpus = scenario
        .add_creature_from_oracle(
            P0,
            "Thrashing Wumpus",
            3,
            3,
            "{B}: This creature deals 1 damage to each creature and each player.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 3,
        })
        .id();
    let first = scenario
        .add_creature(P0, "First protected recipient", 2, 2)
        .id();
    let last = scenario
        .add_creature(P1, "Last protected recipient", 2, 2)
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::White, ManaType::Black, ManaType::Black]
            .into_iter()
            .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.activate(pilgrim, 0).resolve();
    let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
        panic!("the paid sacrifice must reach the actual source choice");
    };
    let reference = options
        .iter()
        .find(|option| option.identity.reference.object_id == wumpus)
        .unwrap()
        .identity
        .reference;
    assert!(
        !runner.state().battlefield.contains(&pilgrim),
        "the printed sacrifice was actually paid"
    );
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved).unwrap();
    runner
        .act(GameAction::ChooseDamageSource { source: reference })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    assert_eq!(
        runner.state().pending_damage_replacements[0].shield_kind,
        engine::types::ability::ShieldKind::PreventionOneShot
    );
    runner.activate(wumpus, 0).resolve();
    assert_eq!(
        (
            runner.state().players[0].life,
            runner.state().players[1].life
        ),
        (20, 20)
    );
    for recipient in [wumpus, first, last] {
        assert_eq!(
            runner.state().objects[&recipient].damage_marked,
            0,
            "one source's whole simultaneous opportunity is protected"
        );
    }
    runner.activate(wumpus, 0).resolve();
    assert_eq!(
        (
            runner.state().players[0].life,
            runner.state().players[1].life
        ),
        (19, 19)
    );
    for recipient in [wumpus, first, last] {
        assert_eq!(
            runner.state().objects[&recipient].damage_marked,
            1,
            "the next independent operation receives no consumed-map inheritance"
        );
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().deferred_triggers.is_empty());
    assert!(runner.state().owed_spell_final_part().is_none());
}

#[test]
fn paid_circle_and_rune_you_only_next_time_and_authentic_generic_continuous_programs() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::RedirectionLifetime;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    for (name, text, payment) in [
        ("Circle of Protection: Black", "{1}: The next time a black source of your choice would deal damage to you this turn, prevent that damage.", ManaType::Colorless),
        ("Rune of Protection: Black", "{W}: The next time a black source of your choice would deal damage to you this turn, prevent that damage.\nCycling {2} ({2}, Discard this card: Draw a card.)", ManaType::White),
    ] {
        for old_generic in [false, true] {
            let mut scenario = GameScenario::new();
            let shield = scenario.add_enchantment_from_oracle(P0, name, text).id();
            let wumpus = scenario.add_creature_from_oracle(P0, "Thrashing Wumpus", 3, 3, "{B}: This creature deals 1 damage to each creature and each player.")
                .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Black, ManaCostShard::Black], generic: 3 }).id();
            scenario.with_mana_pool(P0, [payment, ManaType::Black, ManaType::Black].into_iter().map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![])).collect());
            let mut runner = scenario.build();
            if old_generic {
                // The actual historical typed generic default was continuous.
                // Removing only that omitted new field preserves stored semantics;
                // the printed description is deliberately retained unchanged.
                let mut value = serde_json::to_value(runner.state()).unwrap();
                let id = shield.0.to_string();
                for field in ["abilities", "base_abilities"] {
                    if let Some(abilities) = value["objects"][&id][field].as_array_mut() {
                        abilities[0]["effect"].as_object_mut().unwrap().remove("lifetime");
                    }
                }
                *runner.state_mut() = serde_json::from_value(value).unwrap();
                let Effect::PreventDamage { lifetime, .. } = runner.state().objects[&shield].abilities[0].effect.as_ref() else { panic!("complete prevention program"); };
                assert_eq!(*lifetime, RedirectionLifetime::Continuous);
            }
            runner.activate(shield, 0).resolve();
            let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else { panic!("paid activation must prompt unconditionally"); };
            let reference = options.iter().find(|option| option.identity.reference.object_id == wumpus).unwrap().identity.reference;
            runner.act(GameAction::ChooseDamageSource { source: reference }).unwrap();
            runner.advance_until_stack_empty();
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).unwrap();
            runner.activate(wumpus, 0).resolve();
            assert_eq!(runner.state().players[0].life, 20);
            assert_eq!(runner.state().players[1].life, 19, "Circle/Rune independently restrict recipients to You");
            assert_eq!(runner.state().objects[&wumpus].damage_marked, 1);
            runner.activate(wumpus, 0).resolve();
            assert_eq!(runner.state().players[0].life, if old_generic { 20 } else { 19 }, "typed lifetime determines the next independent event without Oracle reparsing");
            assert_eq!(runner.state().players[1].life, 18);
            assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { .. }));
            assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty() && runner.state().deferred_triggers.is_empty());
            assert!(runner.state().owed_spell_final_part().is_none());
        }
    }
}

#[test]
fn final_wumpus_recipient_pause_retains_consumed_definition_without_a_dummy_remainder() {
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{TrustedGameStateEnvelope, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::replacements::ReplacementEvent;
    let mut scenario = GameScenario::new();
    let pilgrim = scenario.add_creature_from_oracle(P0, "Pilgrim of Virtue", 1, 3,
        "Protection from black\n{W}, Sacrifice this creature: The next time a black source of your choice would deal damage this turn, prevent that damage.").id();
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Thrashing Wumpus",
            3,
            3,
            "{B}: This creature deals 1 damage to each creature and each player.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::White, ManaType::Black, ManaType::Black]
            .into_iter()
            .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.activate(pilgrim, 0).resolve();
    let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
        panic!("paid source choice");
    };
    let reference = options
        .iter()
        .find(|option| option.identity.reference.object_id == source)
        .unwrap()
        .identity
        .reference;
    runner
        .act(GameAction::ChooseDamageSource { source: reference })
        .unwrap();
    runner.advance_until_stack_empty();
    let optional_index = runner.state().pending_damage_replacements.len();
    let mut optional = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .damage_modification(DamageModification::Double)
        .damage_target_filter(DamageTargetFilter::Player {
            player: DamageTargetPlayerScope::Specific(P1),
        })
        .mode(ReplacementMode::Optional { decline: None });
    optional.source_controller = Some(P1);
    optional.description = Some("Last-recipient optional double".into());
    runner
        .state_mut()
        .pending_damage_replacements
        .push(optional);
    runner.activate(source, 0).resolve();
    let WaitingFor::ReplacementChoice { .. } = &runner.state().waiting_for else {
        panic!("last recipient really pauses after earlier recipients consumed the shield");
    };
    let pending = runner.state().pending_replacement.as_ref().unwrap();
    assert!(
        matches!(pending.proposed, engine::types::proposed_event::ProposedEvent::Damage { target: TargetRef::Player(player), .. } if player == P1)
    );
    assert_eq!(pending.damage_opportunity.len(), 1);
    assert!(runner.state().pending_damage_replacements[0].is_consumed);
    assert!(
        runner.state().active_ability_continuation().is_none(),
        "the last recipient has no fabricated raw remainder"
    );
    assert_eq!(runner.state().players[0].life, 20);
    let original = runner.state().clone();
    for wire in [false, true] {
        let raw = if wire {
            serde_json::to_value(
                engine::types::resolution::ResolutionStateWire::from_game_state(original.clone()),
            )
            .unwrap()
        } else {
            serde_json::to_value(&original).unwrap()
        };
        for hostile in 0..3 {
            let mut invalid = raw.clone();
            let map = invalid["pending_replacement"]["damage_opportunity"]["definitions"]
                .as_array_mut()
                .unwrap();
            assert_eq!(
                map.len(),
                1,
                "the reached last-recipient pause has real consumed-definition authority"
            );
            match hostile {
                0 => map.push(map[0].clone()),
                1 => map[0][1]["is_consumed"] = serde_json::json!(true),
                2 => {
                    map[0][1]["shield_kind"] = serde_json::to_value(ShieldKind::Prevention {
                        amount: PreventionAmount::All,
                    })
                    .unwrap()
                }
                _ => unreachable!(),
            }
            let bytes = serde_json::to_vec(&invalid).unwrap();
            let error = if wire {
                serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                    invalid.clone(),
                )
                .unwrap_err()
                .to_string()
            } else {
                serde_json::from_value::<engine::types::game_state::GameState>(invalid.clone())
                    .unwrap_err()
                    .to_string()
            };
            assert!(
                error.contains("pending_replacement/damage_opportunity")
                    && error.contains("consumed-definition authority"),
                "precise local authority error: {error}"
            );
            assert_eq!(serde_json::to_vec(&invalid).unwrap(), bytes);
            assert_eq!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .damage_opportunity
                    .len(),
                1
            );
        }
    }
    for viewer in [P0, P1] {
        use engine::game::derived_views::ClientGameStateRef;
        let filtered = engine::game::visibility::filter_state_for_viewer(&original, viewer);
        for public in [
            serde_json::to_value(ClientGameStateRef::wrap(&original, Some(viewer))).unwrap(),
            serde_json::to_value(ClientGameStateRef::wrap_filtered(
                &original,
                &filtered,
                Some(viewer),
            ))
            .unwrap(),
        ] {
            let text = serde_json::to_string(&public).unwrap();
            assert!(
                !text.contains("damage_opportunity") && !text.contains("selected_damage_source")
            );
            assert_eq!(
                public["state"]["waiting_for"]["type"], "ReplacementChoice",
                "private maps are hidden while the genuine public pause stays visible"
            );
        }
    }
    for trusted in [false, true] {
        let state = if trusted {
            let value =
                serde_json::to_value(TrustedGameStateEnvelope::capture(original.clone())).unwrap();
            serde_json::from_value::<TrustedGameStateEnvelope>(value)
                .unwrap()
                .into_game_state()
        } else {
            serde_json::from_value(serde_json::to_value(&original).unwrap()).unwrap()
        };
        let mut resumed = GameRunner::from_state(state);
        let pending = resumed.state().pending_replacement.as_ref().unwrap();
        let chosen = pending
            .candidates
            .iter()
            .position(|id| id.source == ObjectId(0) && id.index == optional_index)
            .unwrap();
        assert!(
            chosen > 0,
            "the production answer deliberately chooses the nondefault competing modifier"
        );
        resumed
            .act(GameAction::ChooseReplacement { index: chosen })
            .unwrap();
        assert!(
            matches!(
                resumed.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "ordering an optional modifier still requires its own public accept decision"
        );
        assert!(
            resumed
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .is_optional
        );
        assert_eq!(
            resumed
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .damage_opportunity
                .len(),
            1
        );
        let optional_pause = serde_json::to_value(resumed.state()).unwrap();
        *resumed.state_mut() = serde_json::from_value(optional_pause).unwrap();
        resumed
            .act(GameAction::ChooseReplacement { index: 0 })
            .unwrap();

        resumed.advance_until_stack_empty();
        assert_eq!(
            (
                resumed.state().players[0].life,
                resumed.state().players[1].life
            ),
            (20, 20),
            "the current paused event retains the spent shield's whole-operation authority"
        );
        assert!(matches!(
            resumed.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            resumed.state().stack.is_empty()
                && resumed.state().resolution_stack.is_empty()
                && resumed.state().deferred_triggers.is_empty()
        );
        assert!(resumed.state().owed_spell_final_part().is_none());
        assert!(resumed.state().pending_replacement.is_none());
        resumed.activate(source, 0).resolve();
        assert!(matches!(
            resumed.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(resumed
            .state()
            .pending_replacement
            .as_ref()
            .unwrap()
            .damage_opportunity
            .is_empty());
        resumed
            .act(GameAction::ChooseReplacement { index: 1 })
            .unwrap();
        resumed.advance_until_stack_empty();
        assert_eq!(
            (
                resumed.state().players[0].life,
                resumed.state().players[1].life
            ),
            (19, 19),
            "an independent operation cannot inherit the prior map"
        );
        assert!(resumed.state().owed_spell_final_part().is_none());
    }
}

#[test]
fn wumpus_raw_remainders_preserve_first_later_optional_pauses_and_exclude_nested_independent_damage(
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{TrustedGameStateEnvelope, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::replacements::ReplacementEvent;
    for nested in [false, true] {
        let mut scenario = GameScenario::new();
        let pilgrim = scenario.add_creature_from_oracle(P0, "Pilgrim of Virtue", 1, 3,
            "Protection from black\n{W}, Sacrifice this creature: The next time a black source of your choice would deal damage this turn, prevent that damage.").id();
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Thrashing Wumpus",
                3,
                3,
                "{B}: This creature deals 1 damage to each creature and each player.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                generic: 3,
            })
            .id();
        let bear = scenario
            .add_creature(P1, "Later protected recipient", 2, 5)
            .id();
        scenario.with_mana_pool(
            P0,
            [ManaType::White, ManaType::Black]
                .into_iter()
                .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        runner.activate(pilgrim, 0).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("paid source choice");
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        runner.advance_until_stack_empty();
        // Typed hostile replacement witness. The printed damage/prevention
        // bodies and payments above still enter the ordinary production path.
        let mut optional = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_target_filter(DamageTargetFilter::CreatureOnly)
            .damage_modification(DamageModification::Double)
            .mode(ReplacementMode::Optional { decline: None });
        if nested {
            optional.valid_card = Some(TargetFilter::SpecificObject { id: bear });
            optional.execute = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SpecificPlayer { id: P0 },
                    damage_source: None,
                    excess: None,
                },
            )));
        }
        let optional_index = runner.state().objects[&source]
            .replacement_definitions
            .len();
        runner
            .state_mut()
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(optional);
        runner.activate(source, 0).resolve();
        let mut decisions = 0;
        let mut first_before_use = false;
        let mut later_after_use = false;
        let mut nondefault = false;
        loop {
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ),
                "every required order/accept pause is asserted unconditionally"
            );
            let pending = runner.state().pending_replacement.as_ref().unwrap();
            first_before_use |= pending.damage_opportunity.is_empty();
            later_after_use |= pending.damage_opportunity.len() == 1;
            let remainder = runner
                .state()
                .active_ability_continuation()
                .expect("this is an actual raw remainder pause before the later players");
            assert!(remainder.chain.context.damage_opportunity.is_some());
            let current = runner.state().clone();
            *runner.state_mut() = if decisions % 2 == 0 {
                serde_json::from_value(serde_json::to_value(&current).unwrap()).unwrap()
            } else {
                serde_json::from_value::<TrustedGameStateEnvelope>(
                    serde_json::to_value(TrustedGameStateEnvelope::capture(current)).unwrap(),
                )
                .unwrap()
                .into_game_state()
            };
            let pending = runner.state().pending_replacement.as_ref().unwrap();
            let index = if pending.is_optional {
                0
            } else {
                let index = pending
                    .candidates
                    .iter()
                    .position(|rid| rid.source == source && rid.index == optional_index)
                    .unwrap();
                nondefault |= index > 0;
                index
            };
            runner.act(GameAction::ChooseReplacement { index }).unwrap();
            decisions += 1;
            assert!(
                decisions <= 8,
                "the exact operation completes without repeated prompts"
            );
            if !matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ) {
                break;
            }
        }
        runner.advance_until_stack_empty();
        assert!(later_after_use && nondefault);
        assert_eq!(decisions, if nested { 2 } else { 4 });
        if !nested {
            assert!(
                first_before_use,
                "the first-recipient control reaches an unspent shield"
            );
        }
        assert_eq!(runner.state().objects[&source].damage_marked, 0);
        assert_eq!(runner.state().objects[&bear].damage_marked, 0);
        assert_eq!((runner.state().players[0].life, runner.state().players[1].life), (if nested { 19 } else { 20 }, 20), "same-source nested independent damage gets a fresh map while every remaining outer recipient keeps its original opportunity");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            runner.state().stack.is_empty()
                && runner.state().resolution_stack.is_empty()
                && runner.state().deferred_triggers.is_empty()
        );
        assert!(
            runner.state().owed_spell_final_part().is_none()
                && runner.state().pending_replacement.is_none()
        );
    }
}

#[test]
fn full_paid_continuous_source_prevention_witnesses_keep_both_independent_damage_events() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    for kind in 0..3 {
        let mut scenario = GameScenario::new();
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Thrashing Wumpus",
                3,
                3,
                "{B}: This creature deals 1 damage to each creature and each player.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                generic: 3,
            })
            .id();
        let protect = match kind {
            0 => scenario.add_spell_to_hand_from_oracle(P0, "Pay No Heed", true, "Prevent all damage a source of your choice would deal this turn.")
                .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id(),
            1 => scenario.add_creature_from_oracle(P0, "Burrenton Forge-Tender", 1, 1, "Protection from red\nSacrifice this creature: Prevent all damage a red source of your choice would deal this turn.").id(),
            2 => scenario.add_creature_from_oracle(P0, "Auriok Replica", 2, 2, "{W}, Sacrifice this creature: Prevent all damage a source of your choice would deal to you this turn.").as_artifact_creature().id(),
            _ => unreachable!(),
        };
        let lace = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Chaoslace",
                true,
                "Target spell or permanent becomes red. (Its mana symbols remain unchanged.)",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        scenario.with_mana_pool(
            P0,
            [
                ManaType::Red,
                ManaType::White,
                ManaType::Black,
                ManaType::Black,
            ]
            .into_iter()
            .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .collect(),
        );
        let mut runner = scenario.build();
        runner.cast(lace).target_object(source).resolve();
        if kind == 0 {
            runner.cast(protect).resolve();
        } else {
            runner.activate(protect, 0).resolve();
        }
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("complete continuous source producer prompts");
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        runner.advance_until_stack_empty();
        assert!(matches!(
            runner.state().pending_damage_replacements[0].shield_kind,
            engine::types::ability::ShieldKind::Prevention {
                amount: engine::types::ability::PreventionAmount::All
            }
        ));
        *runner.state_mut() =
            serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap();
        for _ in 0..2 {
            runner.activate(source, 0).resolve();
        }
        assert_eq!((runner.state().players[0].life, runner.state().players[1].life), (20, if kind == 2 { 18 } else { 20 }), "continuous prevention survives both events and preserves its independently printed recipient scope");
        assert!(!runner.state().pending_damage_replacements[0].is_consumed);
        assert_eq!(
            runner.state().objects[&source].damage_marked,
            if kind == 2 { 2 } else { 0 }
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            runner.state().stack.is_empty()
                && runner.state().resolution_stack.is_empty()
                && runner.state().deferred_triggers.is_empty()
        );
        assert!(runner.state().owed_spell_final_part().is_none());
    }
}
