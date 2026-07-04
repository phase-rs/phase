use super::*;
use crate::types::replacements::ReplacementEvent;
use crate::types::statics::StaticMode;

fn pipeline_parse(
    oracle_text: &str,
    card_name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> ParsedAbilities {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle_text, card_name, &[], &types, &subtypes)
}

#[test]
fn pipeline_simple_spell() {
    let result = pipeline_parse(
        "Deal 3 damage to any target.",
        "Test Card",
        &["Sorcery"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

/// CR 601.2a + CR 611.2a (issue #2851): Chandra, Hope's Beacon +1 —
/// "Exile the top five cards of your library. Until the end of your next
/// turn, you may cast an instant or sorcery spell from among those exiled
/// cards." The cast-from-exile grant must carry BOTH the instant/sorcery
/// type filter AND the single-spell-total cap, not the unrestricted
/// impulse-draw shape (which dropped the filter, the cap, and the duration).
#[test]
fn pipeline_chandra_plus_one_exile_cast_typed_single_use() {
    use crate::types::ability::{
        CastingPermission, Duration, Effect, PlayerScope, TargetFilter, TypeFilter, TypedFilter,
    };
    let result = pipeline_parse(
            "Exile the top five cards of your library. Until the end of your next turn, you may cast an instant or sorcery spell from among those exiled cards.",
            "Chandra, Hope's Beacon",
            &["Sorcery"],
            &[],
        );
    let exile_top = result
        .abilities
        .first()
        .expect("ExileTop root ability present");
    assert!(
        matches!(*exile_top.effect, Effect::ExileTop { .. }),
        "root effect must be ExileTop, got {:?}",
        exile_top.effect
    );
    let grant = exile_top
        .sub_ability
        .as_deref()
        .expect("cast-from-exile grant must chain off ExileTop, not be swallowed");
    match &*grant.effect {
            Effect::GrantCastingPermission {
                permission:
                    CastingPermission::PlayFromExile {
                        duration:
                            Duration::UntilEndOfNextTurnOf {
                                player: PlayerScope::Controller,
                            },
                        card_filter: Some(TargetFilter::Typed(TypedFilter { type_filters, .. })),
                        single_use: true,
                        cast_cost_raise: None,
                        land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        ..
                    },
                ..
            } => {
                assert_eq!(
                    type_filters.as_slice(),
                    [TypeFilter::AnyOf(vec![TypeFilter::Instant, TypeFilter::Sorcery])],
                    "card filter must restrict to instant or sorcery"
                );
            }
            other => panic!(
                "expected single-use, instant/sorcery-filtered PlayFromExile with UntilEndOfNextTurnOf, got {other:?}"
            ),
        }
}

/// CR 601.2a: The plural unbounded form ("you may cast spells from among
/// those exiled cards") must keep its unrestricted shape — no card filter,
/// not single-use — so existing impulse-cast cards (Nassari, Stolen
/// Strategy) are unaffected by the typed-grant extension.
#[test]
fn pipeline_plural_exile_cast_stays_unrestricted() {
    use crate::types::ability::{CastingPermission, Effect};
    let result = pipeline_parse(
            "Exile the top five cards of your library. Until the end of your next turn, you may cast spells from among those exiled cards.",
            "Plural Impulse",
            &["Sorcery"],
            &[],
        );
    let grant = result.abilities[0]
        .sub_ability
        .as_deref()
        .expect("grant chains off ExileTop");
    assert!(
        matches!(
            &*grant.effect,
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    card_filter: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    ..
                },
                ..
            }
        ),
        "plural form must stay unrestricted (no filter, not single-use), got {:?}",
        grant.effect
    );
}

#[test]
fn pipeline_creature_with_keywords_and_trigger() {
    let result = pipeline_parse(
        "Flying\nWhen Test Card enters, draw a card.",
        "Test Card",
        &["Creature"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn pipeline_enchantment_with_static_and_replacement() {
    let result = pipeline_parse(
        "Creatures you control get +1/+1.\nIf a creature you control would die, exile it instead.",
        "Test Card",
        &["Enchantment"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn pipeline_saga_card() {
    let result = pipeline_parse(
            "I — You draw a card and you lose 1 life.\nII — Create a 2/2 black Zombie creature token.\nIII — Target opponent discards a card.",
            "Test Card",
            &["Enchantment"],
            &["Saga"],
        );
    insta::assert_json_snapshot!(result);
}

#[test]
fn pipeline_class_card() {
    let result = pipeline_parse(
            "Creatures you control get +1/+0.\n{1}{R}: Level 2\nWhenever you attack, target creature you control gains first strike until end of turn.",
            "Test Card",
            &["Enchantment"],
            &["Class"],
        );
    insta::assert_json_snapshot!(result);
}

#[test]
fn pipeline_modal_spell() {
    let result = pipeline_parse(
        "Choose one —\n• Destroy target artifact.\n• Destroy target enchantment.",
        "Test Card",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

/// CR 614.1c + CR 502.3: Same-line compound "[~] enters tapped and doesn't
/// untap during your untap step." must emit BOTH an ETB-tapped replacement
/// (CR 614.1c) and a CantUntap static (CR 502.3). Regression guard against
/// the prior bug where the static-pattern classifier consumed the line and
/// silently dropped the replacement half. Corpus: Traxos, Scourge of Kroog;
/// Grimgrin, Corpse-Born; Leviathan.
#[test]
fn pipeline_etb_tapped_and_cant_untap_compound_emits_both() {
    let result = pipeline_parse(
            "Trample\nTraxos enters tapped and doesn't untap during your untap step.\nWhenever you cast a historic spell, untap Traxos.",
            "Traxos, Scourge of Kroog",
            &["Artifact", "Creature"],
            &["Construct"],
        );
    assert_eq!(
        result.replacements.len(),
        1,
        "expected one ETB-tapped replacement, got {:?}",
        result.replacements
    );
    assert!(
        matches!(result.replacements[0].event, ReplacementEvent::Moved),
        "replacement event must be Moved (ETB), got {:?}",
        result.replacements[0].event
    );
    assert_eq!(
        result.statics.len(),
        1,
        "expected one CantUntap static, got {:?}",
        result.statics
    );
    assert_eq!(
        result.statics[0].mode,
        StaticMode::CantUntap,
        "static mode must be CantUntap"
    );
}

// ----------------------------------------------------------------
// Rocco, Street Chef (issue #412): end-step exile-and-grant +
// disjunctive play-or-cast payoff triggers.
// ----------------------------------------------------------------

/// CR 513.1 + CR 611.2a + CR 108.3 + CR 400.7: Rocco's first trigger
/// parses to a Phase-mode end-step trigger whose chained sub-ability is
/// `GrantCastingPermission { permission: PlayFromExile { duration:
/// UntilNextStepOf { step: End, player: Controller }, ... }, target: TrackedSet(0),
/// grantee: ObjectOwner }`. CR 305.1 + CR 601.2: the second trigger is
/// disjunctive on "plays a land from exile" / "casts a spell from
/// exile" and emits two TriggerDefinitions — one `LandPlayed`, one
/// `SpellCast` — both with `valid_card.InZone(Exile)` so the
/// payoff (counter + Food token) fires only on plays-from-exile.
#[test]
fn pipeline_rocco_street_chef_emits_three_triggers() {
    use crate::types::ability::{
        CastingPermission, Duration, Effect, FilterProp, PermissionGrantee, PlayerScope,
        TargetFilter, TypedFilter,
    };
    let result = pipeline_parse(
            "At the beginning of your end step, each player exiles the top card of their library. Until your next end step, each player may play the card they exiled this way.\nWhenever a player plays a land from exile or casts a spell from exile, you put a +1/+1 counter on target creature and create a Food token.",
            "Rocco, Street Chef",
            &["Legendary", "Creature"],
            &["Elf", "Druid"],
        );

    assert_eq!(
        result.triggers.len(),
        3,
        "expected 3 triggers (1 end-step + 2 disjunctive payoff), got {:?}",
        result.triggers.iter().map(|t| &t.mode).collect::<Vec<_>>(),
    );

    // Trigger 0: end-step Phase trigger with sub_ability GrantCastingPermission.
    let t0 = &result.triggers[0];
    assert_eq!(t0.mode, TriggerMode::Phase);
    assert_eq!(t0.phase, Some(crate::types::phase::Phase::End));
    let execute = t0.execute.as_deref().expect("trigger has execute");
    let sub = execute.sub_ability.as_deref().expect("sub_ability present");
    match sub.effect.as_ref() {
        Effect::GrantCastingPermission {
            permission,
            target,
            grantee,
        } => {
            match permission {
                CastingPermission::PlayFromExile {
                    duration:
                        Duration::UntilNextStepOf {
                            step: crate::types::phase::Phase::End,
                            player: PlayerScope::Controller,
                        },
                    ..
                } => {}
                _ => panic!(
                    "expected PlayFromExile {{ UntilNextStepOf {{ End, Controller }} }}, got {:?}",
                    permission,
                ),
            }
            assert!(
                matches!(
                    target,
                    TargetFilter::TrackedSet {
                        id: crate::types::identifiers::TrackedSetId(0)
                    }
                ),
                "target must be TrackedSet(0), got {:?}",
                target,
            );
            assert_eq!(*grantee, PermissionGrantee::ObjectOwner);
        }
        other => panic!("expected GrantCastingPermission, got {:?}", other),
    }

    // Triggers 1 and 2: disjunctive payoff. Order may vary; collect modes.
    let modes: std::collections::HashSet<_> = result.triggers[1..]
        .iter()
        .map(|t| t.mode.clone())
        .collect();
    assert!(
        modes.contains(&TriggerMode::LandPlayed),
        "expected one LandPlayed trigger, got {:?}",
        modes,
    );
    assert!(
        modes.contains(&TriggerMode::SpellCast),
        "expected one SpellCast trigger, got {:?}",
        modes,
    );

    // Each payoff trigger constrains the event to "from exile" — but
    // through different typed fields per CR 601.2a vs CR 305:
    //   - LandPlayed (CR 305): `valid_card.InZone(Exile)` — the
    //     LandPlayed matcher reads the FilterProp::InZone.
    //   - SpellCast (CR 601.2a): `spell_cast_origin = Equals(Exile)` —
    //     the SpellCast matcher reads the typed origin constraint via
    //     the cast-origin gate, since at fire-time the spell object's
    //     zone is `Stack`, not its cast origin.
    use crate::types::ability::OriginConstraint;
    use crate::types::zones::Zone;
    for trigger in &result.triggers[1..] {
        match trigger.mode {
            TriggerMode::LandPlayed => {
                let valid_card = trigger
                    .valid_card
                    .as_ref()
                    .expect("LandPlayed payoff trigger has valid_card filter");
                match valid_card {
                    TargetFilter::Typed(TypedFilter { properties, .. }) => {
                        assert!(
                            properties
                                .iter()
                                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Exile })),
                            "LandPlayed valid_card must carry InZone(Exile), got {:?}",
                            properties,
                        );
                    }
                    other => panic!("expected Typed filter, got {:?}", other),
                }
            }
            TriggerMode::SpellCast => {
                assert_eq!(
                    trigger.spell_cast_origin,
                    OriginConstraint::Equals(Zone::Exile),
                    "SpellCast payoff trigger must constrain cast origin to Exile",
                );
            }
            ref other => panic!("unexpected payoff trigger mode: {:?}", other),
        }
    }
}

/// CR 608.2c: Compound "destroy X and up to one other target Y" must parse
/// both halves as Destroy effects with the verb carried forward to the
/// "up to" sub-clause. Cards: Relic Crush, Sword of Sinew and Steel.
#[test]
fn pipeline_relic_crush_compound_destroy_up_to() {
    use crate::types::ability::{FilterProp, MultiTargetSpec, QuantityExpr, TargetFilter};
    let result = pipeline_parse(
            "Destroy target artifact or enchantment and up to one other target artifact or enchantment.",
            "Relic Crush",
            &["Sorcery"],
            &[],
        );
    assert_eq!(
        result.abilities.len(),
        1,
        "expected one spell ability, got {:?}",
        result.abilities,
    );
    let ab = &result.abilities[0];
    assert!(
        matches!(*ab.effect, Effect::Destroy { .. }),
        "primary effect must be Destroy, got {:?}",
        ab.effect,
    );
    let sub = ab.sub_ability.as_deref().expect("must have sub_ability");
    assert!(
        matches!(*sub.effect, Effect::Destroy { .. }),
        "sub-effect must be Destroy, got {:?}",
        sub.effect,
    );
    // CR 115.6: "up to one" cardinality must be preserved on the sub-ability.
    assert_eq!(
        sub.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 })),
        "sub-ability must carry up-to-one multi_target",
    );
    // CR 608.2c: "other" must appear as FilterProp::Another in the sub-effect target.
    match sub.effect.as_ref() {
        Effect::Destroy { target, .. } => {
            // Target may be Typed or Or { filters: [Typed, Typed] } for
            // "artifact or enchantment".
            let typed_filters: Vec<_> = match target {
                TargetFilter::Typed(tf) => vec![tf],
                TargetFilter::Or { filters } => filters
                    .iter()
                    .map(|f| match f {
                        TargetFilter::Typed(tf) => tf,
                        other => panic!("expected Typed in Or, got {:?}", other),
                    })
                    .collect(),
                other => panic!("expected Typed or Or target, got {:?}", other),
            };
            assert!(
                typed_filters
                    .iter()
                    .all(|tf| tf.properties.contains(&FilterProp::Another)),
                "all sub-clause target filters must have Another property, got {:?}",
                typed_filters,
            );
        }
        other => panic!("expected Destroy, got {:?}", other),
    }
}

#[test]
fn pipeline_scheming_aspirant_proliferate_trigger() {
    let result = pipeline_parse(
        "Whenever you proliferate, each opponent loses 2 life and you gain 2 life.",
        "Scheming Aspirant",
        &["Creature"],
        &["Human", "Noble"],
    );
    assert_eq!(result.triggers.len(), 1);
    let trigger = &result.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::PlayerPerformedAction);
    assert_eq!(trigger.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        trigger.player_actions,
        Some(vec![crate::types::events::PlayerActionKind::Proliferate])
    );
    // Verify the execute body is LoseLife + GainLife
    let exec = trigger.execute.as_ref().expect("execute body");
    assert!(
        matches!(exec.effect.as_ref(), Effect::LoseLife { .. }),
        "expected LoseLife, got {:?}",
        exec.effect
    );
    let sub = exec.sub_ability.as_ref().expect("sub_ability");
    assert!(
        matches!(sub.effect.as_ref(), Effect::GainLife { .. }),
        "expected GainLife, got {:?}",
        sub.effect
    );
}

/// CR 608.2c + CR 701.8a: Loyal Sentry — "destroy that creature and ~"
/// compound action with self-reference carry-forward.
#[test]
fn pipeline_loyal_sentry_compound_destroy_self_ref() {
    use crate::types::ability::TargetFilter;
    use crate::types::triggers::TriggerMode;
    let result = pipeline_parse(
        "When this creature blocks a creature, destroy that creature and ~.",
        "Loyal Sentry",
        &["Creature"],
        &[],
    );
    // Should have one triggered ability.
    assert_eq!(
        result.triggers.len(),
        1,
        "expected one trigger, got {:?}",
        result.triggers,
    );
    let trig = &result.triggers[0];
    // CR 509.1g: Trigger mode must be Blocks.
    assert_eq!(
        trig.mode,
        TriggerMode::Blocks,
        "trigger mode must be Blocks",
    );
    // The execute field holds the AbilityDefinition for the triggered effect.
    let exec = trig.execute.as_deref().expect("trigger must have execute");
    // CR 608.2c: Primary effect is Destroy targeting the blocked creature.
    // The anaphoric "that creature" resolves to ParentTarget (inherits the
    // trigger's target binding via try_split_targeted_compound).
    match exec.effect.as_ref() {
        Effect::Destroy { target, .. } => {
            assert_eq!(
                target.clone(),
                TargetFilter::ParentTarget,
                "primary target must be ParentTarget (the blocked creature)",
            );
        }
        other => panic!("primary effect must be Destroy, got {:?}", other),
    }
    // CR 608.2c + CR 701.8a: Sub-clause is Destroy { SelfRef } for '~'.
    let sub = exec.sub_ability.as_deref().expect("must have sub_ability");
    match sub.effect.as_ref() {
        Effect::Destroy { target, .. } => {
            assert_eq!(
                target.clone(),
                TargetFilter::SelfRef,
                "sub-clause target must be SelfRef for '~'",
            );
        }
        other => panic!("sub-clause must be Destroy, got {:?}", other),
    }
}

// ── Well of Lost Dreams: pay {X} ≤ life gained, draw X cards ─────────────

#[test]
fn well_of_lost_dreams_draw_count_is_variable_x() {
    // CR 107.3i: "where X is less than or equal to <bound>" is a player-
    // chosen constraint, not a definition of X. The draw count must resolve
    // to Variable("X") so the PayAmountChoice → chosen_x → draw path
    // produces X drawn cards (not 0 from a stale QuantityRef string).
    let r = pipeline_parse(
            "Whenever you gain life, you may pay {X}, where X is less than or equal to the amount of life you gained. If you do, draw X cards.",
            "Well of Lost Dreams",
            &["Artifact"],
            &[],
        );
    assert_eq!(r.triggers.len(), 1, "should have one trigger");
    let exec = r.triggers[0]
        .execute
        .as_ref()
        .expect("trigger must have execute");
    assert!(
        matches!(*exec.effect, Effect::PayCost { .. }),
        "first effect should be PayCost, got {:?}",
        exec.effect,
    );
    let sub = exec
        .sub_ability
        .as_deref()
        .expect("PayCost must have sub_ability");
    match sub.effect.as_ref() {
        Effect::Draw { count, .. } => {
            assert_eq!(
                    count.clone(),
                    crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    "draw count must be Variable(\"X\") so chosen_x resolves it, not a stale bound string"
                );
        }
        other => panic!("sub-ability must be Draw, got {:?}", other),
    }
}

#[test]
fn zack_fair_activated_parses_counter_move_and_attach_sub_chain() {
    use crate::types::ability::TargetFilter;

    let effect = "Target creature you control gains indestructible until end of turn. Put Zack Fair's counters on that creature and attach an Equipment that was attached to Zack Fair to that creature.";
    let mut ctx = ParseContext::default();
    let def = parse_activated_with_self_ref_fallback(effect, "Zack Fair", &mut ctx);

    fn has_effect(def: &AbilityDefinition, pred: &dyn Fn(&Effect) -> bool) -> bool {
        if pred(&def.effect) {
            return true;
        }
        def.sub_ability
            .as_ref()
            .is_some_and(|sub| has_effect(sub, pred))
    }

    assert!(has_effect(&def, &|e| matches!(
        e,
        Effect::MoveCounters {
            source: TargetFilter::SelfRef,
            ..
        }
    )));
    assert!(
        has_effect(&def, &|e| matches!(e, Effect::Attach { .. })),
        "expected Attach in sub chain, got {:?}",
        def.sub_ability
    );
    assert!(!has_unimplemented(&def));
}

/// CR 120.1 + CR 208.3 + CR 115.4: Iron Fist, Living Weapon — the cast
/// trigger grants "{T}: ~ deals damage equal to his power to any other
/// target". The granted ability's inner effect must parse to a concrete
/// `DealDamage { Power{Source}, Another }`, not `Effect::Unimplemented`.
#[test]
fn iron_fist_living_weapon_grants_damage_equal_to_power_any_other_target() {
    use crate::types::ability::{
        ContinuousModification, Effect, FilterProp, ObjectScope, QuantityExpr, QuantityRef,
        TargetFilter, TypedFilter,
    };

    let p = parse_oracle_text(
            "Whenever you cast a spell that targets a creature you control, Iron Fist gains \
             \"{T}: Iron Fist deals damage equal to his power to any other target\" until end of turn.",
            "Iron Fist, Living Weapon",
            &[],
            &["Creature".into()],
            &[],
        );

    let execute = p.triggers[0]
        .execute
        .as_ref()
        .expect("cast trigger has an execute ability");
    assert!(!has_unimplemented(execute), "no Unimplemented in Iron Fist");

    let Effect::GenericEffect {
        static_abilities, ..
    } = &*execute.effect
    else {
        panic!("expected GenericEffect, got {:?}", execute.effect);
    };
    let granted = static_abilities
        .iter()
        .flat_map(|s| s.modifications.iter())
        .find_map(|m| match m {
            ContinuousModification::GrantAbility { definition } => Some(definition),
            _ => None,
        })
        .expect("a GrantAbility modification");

    assert!(
        matches!(
            &*granted.effect,
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source,
                    },
                },
                target: TargetFilter::Typed(TypedFilter { properties, .. }),
                ..
            } if properties.iter().any(|prop| matches!(prop, FilterProp::Another))
        ),
        "granted ability must deal damage equal to source power to any other target, got {:?}",
        granted.effect
    );
}

/// CR 707.2 + CR 608.2c + CR 109.4: Fractured Identity — exile target
/// nonland permanent, then each player OTHER THAN ITS CONTROLLER creates a
/// token that's a copy of it. The second sentence must lower to a
/// `player_scope`-iterated `CopyTokenOf` whose scope is
/// `AllExcept { ParentObjectTargetController }`, with the copy source being
/// the exiled permanent (`ParentTarget`). Zero `Effect::Unimplemented`.
#[test]
fn fractured_identity_each_player_other_than_controller_copies_exiled_permanent() {
    use crate::types::ability::{Effect, PlayerFilter, TargetFilter};

    let p = parse_oracle_text(
        "Exile target nonland permanent. Each player other than its controller \
             creates a token that's a copy of it.",
        "Fractured Identity",
        &[],
        &["Sorcery".into()],
        &[],
    );

    let spell = &p.abilities[0];
    assert!(
        !has_unimplemented(spell),
        "no Unimplemented in Fractured Identity, got {:?}",
        spell
    );

    // Head: exile the targeted nonland permanent.
    assert!(
        matches!(
            &*spell.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ),
        "head clause must exile, got {:?}",
        spell.effect
    );

    // Tail: per-player copy-token of the exiled permanent, scoped to every
    // player except the exiled permanent's controller.
    let sub = spell
        .sub_ability
        .as_ref()
        .expect("Fractured Identity has a second-sentence sub-ability");
    assert_eq!(
        sub.player_scope,
        Some(PlayerFilter::AllExcept {
            exclude: Box::new(PlayerFilter::ParentObjectTargetController),
        }),
        "tail player_scope must exclude the exiled permanent's controller",
    );
    // The "it" anaphor links to the exiled permanent published by the head
    // clause's `ChangeZone` as a tracked set (the standard cross-sentence
    // exiled-object reference); `token_copy::resolve` resolves a
    // `TrackedSet` copy source. The iterated player is the token owner.
    assert!(
        matches!(
            &*sub.effect,
            Effect::CopyTokenOf {
                target: TargetFilter::TrackedSet { .. },
                owner: TargetFilter::Controller,
                ..
            }
        ),
        "tail must copy the exiled permanent (tracked-set anaphor) with the iterated \
             player as owner, got {:?}",
        sub.effect
    );
}

/// CR 120.1 + CR 122.1 + CR 115.4: Red Hulk — the Enrage trigger puts a
/// +1/+1 counter on the source, then a reflexive "when you do" deals damage
/// equal to the number of +1/+1 counters on the source to any other target.
/// The reflexive damage must parse concretely, not to `Effect::Unimplemented`.
#[test]
fn red_hulk_enrage_reflex_damage_equal_to_counters_any_other_target() {
    use crate::types::ability::{
        Effect, FilterProp, ObjectScope, QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
    };
    use crate::types::counter::CounterType;

    let p = parse_oracle_text(
        "Reach, trample\nEnrage — Whenever Red Hulk is dealt damage, put a +1/+1 \
             counter on him. When you do, he deals damage equal to the number of +1/+1 \
             counters on him to any other target.",
        "Red Hulk",
        &["Reach".into(), "Trample".into()],
        &["Creature".into()],
        &[],
    );

    let execute = p.triggers[0]
        .execute
        .as_ref()
        .expect("enrage trigger has an execute ability");
    assert!(!has_unimplemented(execute), "no Unimplemented in Red Hulk");

    // Head: put a +1/+1 counter on the source.
    assert!(
        matches!(
            &*execute.effect,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "enrage head must put a +1/+1 counter on the source, got {:?}",
        execute.effect
    );

    // Reflexive sub: deal damage equal to +1/+1 counters on source to any other target.
    let reflex = execute
        .sub_ability
        .as_deref()
        .expect("reflexive when-you-do sub-ability");
    assert!(
        matches!(
            &*reflex.effect,
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(CounterType::Plus1Plus1),
                    },
                },
                target: TargetFilter::Typed(TypedFilter { properties, .. }),
                ..
            } if properties.iter().any(|prop| matches!(prop, FilterProp::Another))
        ),
        "reflex must deal damage equal to source's +1/+1 counters to any other target, got {:?}",
        reflex.effect
    );
}
