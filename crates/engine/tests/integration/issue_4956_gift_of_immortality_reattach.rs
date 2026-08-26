//! Issue #4956: Gift of Immortality delayed Aura reattach must specify the
//! host ("that creature" / "you") instead of opening CR 303.4f Aura choice.
//!
//! Peers: Next of Kin (attach to the put creature), Lynde (attach to you).

use engine::game::effects::attach::{attach_to, attach_to_player};
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, DelayedTriggerCondition, Effect, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GIFT_ORACLE: &str = "Enchant creature\n\
When enchanted creature dies, return that card to the battlefield under its \
owner's control. Return this card to the battlefield attached to that creature \
at the beginning of the next end step.";

const NEXT_OF_KIN_ORACLE: &str = "Enchant creature\n\
When enchanted creature dies, you may put a creature card you own with lesser \
mana value from your hand or from the command zone onto the battlefield. If \
you do, return this card to the battlefield attached to that creature at the \
beginning of the next end step.";

const LYNDE_ORACLE: &str = "Deathtouch\n\
Whenever a Curse is put into your graveyard from the battlefield, return it to \
the battlefield attached to you at the beginning of the next end step.\n\
At the beginning of your upkeep, you may attach a Curse attached to you to one \
of your opponents. If you do, draw two cards.";

const SMOKE_SHROUD_ORACLE: &str = "Enchant creature\n\
Enchanted creature gets +1/+1 and has flying.\n\
When a Ninja you control enters, you may return this card from your graveyard \
to the battlefield attached to that creature.";

const DRAGON_BREATH_ORACLE: &str = "Enchant creature\n\
Enchanted creature has haste.\n\
{R}: Enchanted creature gets +1/+0 until end of turn.\n\
When a creature with mana value 6 or greater enters, you may return this card \
from your graveyard to the battlefield attached to that creature.";

const CASS_ORACLE: &str = "Vigilance\n\
Whenever Cass or another creature you control dies, if it was enchanted or \
equipped, return any number of Aura cards that were attached to it from your \
graveyard to the battlefield attached to target creature, then attach any \
number of Equipment that were attached to it to that creature.";

const STORM_HERALD_ORACLE: &str = "Haste\n\
When this creature enters, return any number of Aura cards from your graveyard \
to the battlefield attached to creatures you control. Exile those Auras at the \
beginning of your next end step. If those Auras would leave the battlefield, \
exile them instead of putting them anywhere else.";

const NECROTIC_PLAGUE_ORACLE: &str = "Enchant creature\n\
Enchanted creature has \"At the beginning of your upkeep, sacrifice this creature.\"\n\
When enchanted creature dies, its controller chooses target creature one of \
their opponents controls. Return this card from its owner's graveyard to the \
battlefield attached to that creature.";

/// Event-subject GY return with nested Attach→ParentTarget (Smoke Shroud / Dragon Breath).
fn event_subject_return_attach_host(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> &TargetFilter {
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| {
            matches!(
                t.execute.as_ref().map(|e| e.effect.as_ref()),
                Some(Effect::ChangeZone {
                    destination: Zone::Battlefield,
                    ..
                })
            )
        })
        .expect("GY return trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(
        execute.forward_result,
        "event-subject return must stamp forward_result for Attach nest"
    );
    let attach = execute.sub_ability.as_ref().expect("Attach nest");
    match attach.effect.as_ref() {
        Effect::Attach {
            attachment: TargetFilter::SelfRef,
            target,
        } => target,
        other => panic!("expected Attach SelfRef→host, got {other:?}"),
    }
}

fn ability_chain_contains_equipment_attach(
    def: &engine::types::ability::AbilityDefinition,
) -> bool {
    let is_equipment_attach = matches!(
        def.effect.as_ref(),
        Effect::Attach {
            attachment: TargetFilter::Typed(tf),
            ..
        } if tf.type_filters.iter().any(|f| {
            matches!(f, engine::types::ability::TypeFilter::Subtype(s) if s == "Equipment")
        })
    );
    is_equipment_attach
        || def
            .sub_ability
            .as_deref()
            .is_some_and(ability_chain_contains_equipment_attach)
        || def
            .else_ability
            .as_deref()
            .is_some_and(ability_chain_contains_equipment_attach)
}

fn ability_chain_contains_delayed_exile(def: &engine::types::ability::AbilityDefinition) -> bool {
    let is_exile = match def.effect.as_ref() {
        Effect::CreateDelayedTrigger { effect, .. } => {
            matches!(
                effect.effect.as_ref(),
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            ) || ability_chain_contains_delayed_exile(effect)
        }
        Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        } => true,
        _ => false,
    };
    is_exile
        || def
            .sub_ability
            .as_deref()
            .is_some_and(ability_chain_contains_delayed_exile)
        || def
            .else_ability
            .as_deref()
            .is_some_and(ability_chain_contains_delayed_exile)
}

fn effect_is_unimplemented(effect: &Effect) -> bool {
    matches!(effect, Effect::Unimplemented { .. })
}

fn count_cdts(def: &engine::types::ability::AbilityDefinition) -> usize {
    let head = matches!(def.effect.as_ref(), Effect::CreateDelayedTrigger { .. }) as usize;
    head + def.sub_ability.as_deref().map(count_cdts).unwrap_or(0)
        + def.else_ability.as_deref().map(count_cdts).unwrap_or(0)
}

fn gift_delayed_attach_host(parsed: &engine::parser::oracle::ParsedAbilities) -> &TargetFilter {
    let trigger = parsed.triggers.first().expect("dies trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    let cdt = execute
        .sub_ability
        .as_ref()
        .expect("CreateDelayedTrigger sibling");
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = cdt.effect.as_ref()
    else {
        panic!("expected CreateDelayedTrigger, got {:?}", cdt.effect);
    };
    assert_eq!(
        condition,
        &DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    let inner = effect.as_ref();
    assert!(
        inner.forward_result,
        "delayed ChangeZone must forward_result into Attach"
    );
    let attach = inner.sub_ability.as_ref().expect("Attach nest");
    match (inner.effect.as_ref(), attach.effect.as_ref()) {
        (
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                ..
            },
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target,
            },
        ) => target,
        other => panic!("unexpected Gift delayed body shape: {other:?}"),
    }
}

fn drain_priority(runner: &mut GameRunner) -> bool {
    drain_priority_preferring(runner, &[])
}

/// Drain priority/resolution prompts, preferring `preferred` object ids when
/// choosing from EffectZoneChoice / target slots (Cass host, Storm Aura, etc.).
fn drain_priority_preferring(
    runner: &mut GameRunner,
    preferred: &[engine::types::identifiers::ObjectId],
) -> bool {
    let mut consumed_effect_zone_choice = false;
    for _ in 0..256 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return consumed_effect_zone_choice;
            }
            WaitingFor::ReturnAsAuraTarget {
                legal_targets,
                returned_id,
                ..
            } => {
                if preferred.is_empty() {
                    panic!(
                        "CR 303.4f Aura host choice must not open when attach_to is specified; \
                         waiting_for = {:?}",
                        runner.state().waiting_for
                    );
                }
                // Storm Herald "attached to creatures you control" is a Typed
                // multi-host filter — CR 303.4f choice is rules-correct when
                // more than one creature is legal. Prefer an explicit host.
                let pick = preferred
                    .iter()
                    .find_map(|id| {
                        legal_targets
                            .iter()
                            .find(|t| matches!(t, TargetRef::Object(oid) if oid == id))
                            .cloned()
                    })
                    .or_else(|| legal_targets.first().cloned())
                    .unwrap_or(TargetRef::Object(*returned_id));
                runner
                    .act(GameAction::ChooseTarget { target: Some(pick) })
                    .expect("choose Aura host");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                consumed_effect_zone_choice = true;
                let pick = preferred
                    .iter()
                    .copied()
                    .find(|id| cards.contains(id))
                    .or_else(|| cards.first().copied())
                    .expect("zone choice candidate");
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("choose zone cards");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                if let Some(pref) = preferred.first() {
                    // Prefer an explicit host when present among legal targets.
                    let legal = match &runner.state().waiting_for {
                        WaitingFor::TriggerTargetSelection {
                            target_slots,
                            selection,
                            ..
                        }
                        | WaitingFor::TargetSelection {
                            target_slots,
                            selection,
                            ..
                        } => target_slots
                            .get(selection.current_slot)
                            .map(|s| s.legal_targets.clone())
                            .unwrap_or_default(),
                        _ => vec![],
                    };
                    let target = legal
                        .iter()
                        .find(|t| matches!(t, TargetRef::Object(id) if id == pref))
                        .cloned()
                        .or_else(|| legal.first().cloned());
                    runner
                        .act(GameAction::ChooseTarget { target })
                        .expect("choose target");
                } else {
                    runner
                        .choose_first_legal_target()
                        .expect("choose first legal target");
                }
            }
            WaitingFor::MultiTargetSelection {
                legal_targets,
                min_targets,
                max_targets,
                ..
            } => {
                let mut chosen: Vec<_> = preferred
                    .iter()
                    .copied()
                    .filter(|id| legal_targets.contains(id))
                    .take(*max_targets)
                    .collect();
                if chosen.len() < *min_targets {
                    for id in legal_targets {
                        if chosen.len() >= *min_targets {
                            break;
                        }
                        if !chosen.contains(id) {
                            chosen.push(*id);
                        }
                    }
                }
                runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("multi-target select");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return consumed_effect_zone_choice;
                }
            }
        }
    }
    panic!(
        "drain_priority exceeded bound; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

fn advance_through_delayed_end(runner: &mut GameRunner) {
    for _ in 0..256 {
        // Stop once the delayed trigger has fired and End/Cleanup priority is
        // idle — do not keep walking into later turns.
        if runner.state().delayed_triggers.is_empty()
            && runner.state().stack.is_empty()
            && matches!(runner.state().phase, Phase::End | Phase::Cleanup)
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::ReturnAsAuraTarget { .. } => {
                panic!(
                    "delayed reattach must not prompt for Aura host; waiting_for = {:?}",
                    runner.state().waiting_for
                );
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                // Lynde (and similar) can be a legal attacker; empty declaration
                // lets auto-advance reach the End step where the delayed fires.
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    panic!(
                        "priority pass stalled; phase={:?} dt={} stack={} wf={:?}",
                        runner.state().phase,
                        runner.state().delayed_triggers.len(),
                        runner.state().stack.len(),
                        runner.state().waiting_for,
                    );
                }
            }
        }
    }
    panic!(
        "advance_through_delayed_end exceeded bound; phase = {:?}, dt = {}, stack = {}, wf = {:?}",
        runner.state().phase,
        runner.state().delayed_triggers.len(),
        runner.state().stack.len(),
        runner.state().waiting_for
    );
}

#[test]
fn gift_of_immortality_delayed_reattach_shape() {
    let parsed = parse_oracle_text(
        GIFT_ORACLE,
        "Gift of Immortality",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1);
    assert!(
        !effect_is_unimplemented(&parsed.triggers[0].execute.as_ref().unwrap().effect),
        "Gift dies trigger must be supported"
    );
    assert_eq!(
        gift_delayed_attach_host(&parsed),
        &TargetFilter::ParentTarget,
        "Gift delayed Attach host must be ParentTarget (that creature)"
    );
}

#[test]
fn next_of_kin_delayed_reattach_shape() {
    let parsed = parse_oracle_text(
        NEXT_OF_KIN_ORACLE,
        "Next of Kin",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let trigger = parsed.triggers.first().expect("dies trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(
        !effect_is_unimplemented(&execute.effect),
        "Next of Kin must parse supported: {:?}",
        execute.effect
    );
    let delayed_link = execute
        .sub_ability
        .as_ref()
        .expect("delayed / if-you-do sibling");
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = delayed_link.effect.as_ref()
    else {
        panic!(
            "Next of Kin 'next end step' must wrap CreateDelayedTrigger (short-name \
             'next' must not rewrite temporal text); got {:?}",
            delayed_link.effect
        );
    };
    assert_eq!(
        condition,
        &DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    let inner = effect.as_ref();
    assert!(inner.forward_result);
    // "If you do" gates CreateDelayedTrigger installation, not the delayed body
    // at end-step fire time (OptionalEffectPerformed is a creation-time signal).
    assert_eq!(
        delayed_link.condition.as_ref(),
        Some(&AbilityCondition::effect_performed()),
        "OptionalEffectPerformed must lift onto the CreateDelayedTrigger wrapper"
    );
    assert!(
        inner.condition.is_none(),
        "delayed payload must not retain OptionalEffectPerformed; got {:?}",
        inner.condition
    );
    let attach = inner.sub_ability.as_ref().expect("Attach nest");
    match attach.effect.as_ref() {
        Effect::Attach {
            attachment,
            target: TargetFilter::ParentTarget,
        } => {
            assert_eq!(
                attachment,
                &TargetFilter::SelfRef,
                "Next of Kin Attach attachment must be SelfRef, got {attachment:?}"
            );
        }
        other => panic!("Next of Kin Attach host must be ParentTarget, got {other:?}"),
    }
    assert_eq!(
        count_cdts(execute),
        1,
        "nest-before-wrap must yield a single CreateDelayedTrigger, not two"
    );
}

#[test]
fn lynde_delayed_reattach_shape() {
    let parsed = parse_oracle_text(
        LYNDE_ORACLE,
        "Lynde, Cheerful Tormentor",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Warlock".to_string()],
    );
    let curse_ltb = parsed
        .triggers
        .iter()
        .find(|t| {
            matches!(
                t.execute.as_ref().map(|e| e.effect.as_ref()),
                Some(Effect::CreateDelayedTrigger { .. })
            )
        })
        .expect("Curse LTB delayed trigger");
    let execute = curse_ltb.execute.as_ref().unwrap();
    let Effect::CreateDelayedTrigger {
        effect, condition, ..
    } = execute.effect.as_ref()
    else {
        unreachable!()
    };
    assert_eq!(
        condition,
        &DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    let inner = effect.as_ref();
    assert!(inner.forward_result);
    let attach = inner.sub_ability.as_ref().expect("Attach nest");
    match (inner.effect.as_ref(), attach.effect.as_ref()) {
        (
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target: TargetFilter::TriggeringSource,
                ..
            },
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::Controller,
            },
        ) => {}
        other => panic!("Lynde delayed body shape wrong: {other:?}"),
    }
}

#[test]
fn gift_of_immortality_reattaches_without_aura_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let gift = scenario
        .add_creature(P0, "Gift of Immortality", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(GIFT_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    // `attach_to` returns the prior host (`None` on first attach), not success.
    attach_to(runner.state_mut(), gift, host);
    assert_eq!(
        runner.state().objects[&gift].attached_to,
        Some(AttachTarget::Object(host)),
        "Gift must start attached to the host"
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "dies trigger returns the enchanted creature"
    );
    assert_eq!(
        runner.state().objects[&gift].zone,
        Zone::Graveyard,
        "Gift is in the graveyard awaiting end-step return"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "exactly one AtNextEnd delayed reattach must be installed"
    );
    assert_eq!(
        runner.state().delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(host)],
        "delayed trigger must snapshot the returned creature for ParentTarget Attach; \
         targets={:?} source={:?} effect={:?}",
        runner.state().delayed_triggers[0].ability.targets,
        runner.state().delayed_triggers[0].ability.source_id,
        runner.state().delayed_triggers[0].ability.effect,
    );
    assert_eq!(
        runner.state().delayed_triggers[0].ability.source_id,
        gift,
        "delayed SelfRef source must remain Gift, not the returned creature"
    );
    assert!(
        matches!(
            &runner.state().delayed_triggers[0].ability.effect,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                origin: None,
                ..
            }
        ),
        "delayed ChangeZone must be SelfRef→BF with no origin guard; got {:?}",
        runner.state().delayed_triggers[0].ability.effect
    );
    assert!(
        matches!(
            runner.state().delayed_triggers[0]
                .ability
                .sub_ability
                .as_ref()
                .map(|s| &s.effect),
            Some(Effect::Attach {
                target: TargetFilter::ParentTarget,
                ..
            })
        ),
        "delayed body must nest Attach→ParentTarget; sub={:?}",
        runner.state().delayed_triggers[0].ability.sub_ability
    );
    runner.advance_to_end_step();
    advance_through_delayed_end(&mut runner);

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "delayed reattach must have fired; phase={:?} dt={:?} stack={} wf={:?}",
        runner.state().phase,
        runner.state().delayed_triggers.len(),
        runner.state().stack.len(),
        runner.state().waiting_for,
    );
    let gift_obj = &runner.state().objects[&gift];
    assert_eq!(
        gift_obj.zone,
        Zone::Battlefield,
        "Gift returns at the next end step; attached={:?} core={:?} subtypes={:?} kw={:?} host_zone={:?} wf={:?}",
        gift_obj.attached_to,
        gift_obj.card_types.core_types,
        gift_obj.card_types.subtypes,
        gift_obj.keywords,
        runner.state().objects[&host].zone,
        runner.state().waiting_for,
    );
    assert_eq!(
        runner.state().objects[&gift].attached_to,
        Some(AttachTarget::Object(host)),
        "Gift auto-attaches to that creature — no CR 303.4f prompt"
    );
}

#[test]
fn gift_of_immortality_stays_in_graveyard_when_host_gone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let gift = scenario
        .add_creature(P0, "Gift of Immortality", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(GIFT_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), gift, host);
    assert_eq!(
        runner.state().objects[&gift].attached_to,
        Some(AttachTarget::Object(host))
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "hostile path must install the delayed reattach before the host is exiled; \
         otherwise the negative assertion can pass without exercising CR 303.4i"
    );
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "dies trigger must return the host before the hostile exile"
    );

    // CR 303.4i + Gatherer: exile the returned host before end step → Gift remains in GY.
    let mut exile_events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Exile, &mut exile_events);

    runner.advance_to_end_step();
    advance_through_delayed_end(&mut runner);

    assert_eq!(
        runner.state().objects[&gift].zone,
        Zone::Graveyard,
        "Gift remains in the graveyard when the specified host is undefined/illegal"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "must not open Aura host choice when the specified host is gone"
    );
}

#[test]
fn next_of_kin_attaches_to_put_creature_not_dying_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Dying host A (MV 3); put creature B from hand (MV 2, lesser).
    let host_a = scenario
        .add_creature(P0, "Hill Giant", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 3,
        })
        .id();
    let put_b = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();
    let aura = scenario
        .add_creature(P0, "Next of Kin", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(NEXT_OF_KIN_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, host_a);
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host_a))
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host_a, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&put_b].zone,
        Zone::Battlefield,
        "Next of Kin puts the lesser-MV creature"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "if-you-do delayed reattach installed"
    );
    assert!(
        runner.state().delayed_triggers[0]
            .ability
            .condition
            .is_none(),
        "installed delayed body must not carry OptionalEffectPerformed; got {:?}",
        runner.state().delayed_triggers[0].ability.condition
    );

    runner.advance_to_end_step();
    advance_through_delayed_end(&mut runner);

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Battlefield,
        "Next of Kin returns at end step"
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(put_b)),
        "hostile: attach to put creature B, not dying host A"
    );
}

#[test]
fn lynde_returns_curse_attached_to_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _lynde = scenario
        .add_creature(P0, "Lynde, Cheerful Tormentor", 2, 4)
        .from_oracle_text(LYNDE_ORACLE)
        .id();
    let curse = scenario
        .add_creature(P0, "Curse of Thirst", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura", "Curse"])
        .with_keyword(Keyword::Enchant(TargetFilter::Player))
        .id();

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P0);
    assert_eq!(
        runner.state().objects[&curse].attached_to,
        Some(AttachTarget::Player(P0))
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), curse, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(runner.state().delayed_triggers.len(), 1);

    runner.advance_to_end_step();
    advance_through_delayed_end(&mut runner);

    assert_eq!(runner.state().objects[&curse].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&curse].attached_to,
        Some(AttachTarget::Player(P0)),
        "Lynde returns the Curse attached to you"
    );
}

#[test]
fn smoke_shroud_and_dragon_breath_attach_host_is_parent_target() {
    for (name, oracle) in [
        ("Smoke Shroud", SMOKE_SHROUD_ORACLE),
        ("Dragon Breath", DRAGON_BREATH_ORACLE),
    ] {
        let parsed = parse_oracle_text(
            oracle,
            name,
            &[],
            &["Enchantment".to_string()],
            &["Aura".to_string()],
        );
        assert_eq!(
            event_subject_return_attach_host(&parsed),
            &TargetFilter::ParentTarget,
            "{name}: GY return Attach host must be ParentTarget (that creature)"
        );
    }
}

#[test]
fn cass_preserves_equipment_reattach_continuation() {
    let parsed = parse_oracle_text(
        CASS_ORACLE,
        "Cass, Hand of Vengeance",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Assassin".to_string()],
    );
    let execute = parsed
        .triggers
        .first()
        .and_then(|t| t.execute.as_ref())
        .expect("Cass dies trigger");
    assert!(
        ability_chain_contains_equipment_attach(execute),
        "Cass must preserve Equipment reattach continuation; execute={:?}",
        execute.effect
    );
    fn find_equipment_attach(
        def: &engine::types::ability::AbilityDefinition,
    ) -> Option<(TargetFilter, TargetFilter)> {
        match def.effect.as_ref() {
            Effect::Attach {
                attachment: TargetFilter::Typed(tf),
                target,
            } if tf.type_filters.iter().any(
                |f| matches!(f, engine::types::ability::TypeFilter::Subtype(s) if s == "Equipment"),
            ) =>
            {
                Some((TargetFilter::Typed(tf.clone()), target.clone()))
            }
            _ => def
                .sub_ability
                .as_deref()
                .and_then(find_equipment_attach)
                .or_else(|| def.else_ability.as_deref().and_then(find_equipment_attach)),
        }
    }
    let (attachment, target) =
        find_equipment_attach(execute).expect("Equipment Attach in Cass chain");
    match &attachment {
        TargetFilter::Typed(tf) => {
            assert!(
                tf.properties
                    .contains(&engine::types::ability::FilterProp::AttachedToSource),
                "Equipment must look back via AttachedToSource LKI, got {tf:?}"
            );
        }
        other => panic!("expected Typed Equipment, got {other:?}"),
    }
    assert!(
        matches!(target, TargetFilter::ParentTarget),
        "Cass 'to that creature' must be ParentTarget (chosen Aura host), got {target:?}"
    );
}

#[test]
fn storm_herald_preserves_delayed_exile_continuation() {
    let parsed = parse_oracle_text(
        STORM_HERALD_ORACLE,
        "Storm Herald",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Shaman".to_string()],
    );
    let execute = parsed
        .triggers
        .first()
        .and_then(|t| t.execute.as_ref())
        .expect("Storm Herald ETB");
    assert!(
        ability_chain_contains_delayed_exile(execute),
        "Storm Herald must preserve delayed exile continuation; execute={:?}",
        execute.effect
    );
    fn find_cdt(
        def: &engine::types::ability::AbilityDefinition,
    ) -> Option<&engine::types::ability::AbilityDefinition> {
        if matches!(def.effect.as_ref(), Effect::CreateDelayedTrigger { .. }) {
            return Some(def);
        }
        def.sub_ability
            .as_deref()
            .and_then(find_cdt)
            .or_else(|| def.else_ability.as_deref().and_then(find_cdt))
    }
    let cdt = find_cdt(execute).expect("CreateDelayedTrigger");
    let Effect::CreateDelayedTrigger {
        uses_tracked_set,
        effect,
        ..
    } = cdt.effect.as_ref()
    else {
        unreachable!()
    };
    assert!(
        *uses_tracked_set,
        "Storm Herald 'those Auras' delayed exile must set uses_tracked_set"
    );
    assert!(
        matches!(
            effect.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        ),
        "delayed body must exile TrackedSet, got {:?}",
        effect.effect
    );
    // Leave-battlefield rider must be AddTargetReplacement (TrackedSet), not a
    // fake immediate ChangeZone Exile ParentTarget claiming support.
    fn find_leave_rider(def: &engine::types::ability::AbilityDefinition) -> Option<&Effect> {
        match def.effect.as_ref() {
            e @ Effect::AddTargetReplacement { .. } => Some(e),
            e @ Effect::Unimplemented { .. } => Some(e),
            e @ Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::ParentTarget,
                ..
            } => Some(e),
            _ => def
                .sub_ability
                .as_deref()
                .and_then(find_leave_rider)
                .or_else(|| def.else_ability.as_deref().and_then(find_leave_rider)),
        }
    }
    match find_leave_rider(execute) {
        Some(Effect::AddTargetReplacement {
            target: TargetFilter::TrackedSet { .. },
            ..
        }) => {}
        Some(Effect::Unimplemented { .. }) => {}
        Some(other) => panic!(
            "leave-battlefield rider must be AddTargetReplacement{{TrackedSet}} or \
             Unimplemented, got {other:?}"
        ),
        None => panic!("expected leave-battlefield rider in Storm Herald chain"),
    }
}

#[test]
fn cass_reattaches_equipment_to_chosen_host_via_pipeline() {
    // CR 400.7j + CR 608.2c + CR 701.3a: drive the printed Cass dies trigger
    // through process_triggers → TriggerTargetSelection → resolution. ParentTarget
    // for the Equipment attach must bind the chosen host without a test-side
    // stamp_host helper.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bearer = scenario.add_creature(P0, "Bearer", 2, 2).id();
    let cass = scenario
        .add_creature(P0, "Cass, Hand of Vengeance", 2, 2)
        .from_oracle_text(CASS_ORACLE)
        .id();
    let equipment = scenario
        .add_creature(P0, "Bonesplitter", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), equipment, cass);

    let mut death_events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), cass, Zone::Graveyard, &mut death_events);
    process_triggers(runner.state_mut(), &death_events);
    assert!(
        !runner.state().stack.is_empty()
            || matches!(
                runner.state().waiting_for,
                WaitingFor::TriggerTargetSelection { .. } | WaitingFor::OrderTriggers { .. }
            ),
        "Cass dies trigger must be pending after equipped Cass dies; \
         stack={} waiting={:?}",
        runner.state().stack.len(),
        runner.state().waiting_for
    );
    drain_priority_preferring(&mut runner, &[bearer, equipment]);

    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(bearer)),
        "Equipment that was attached to dying Cass must reattach to chosen bearer; \
         attached_to={:?}",
        runner.state().objects[&equipment].attached_to
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "must not open CR 303.4f Aura host choice for Equipment reattach"
    );
}

#[test]
fn storm_herald_exiles_returned_auras_at_end_step_via_pipeline() {
    // CR 603.7 + CR 303.4f: return Aura attached to a creature you control, then
    // delayed TrackedSet exile at next end step.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = scenario
        .add_creature_to_graveyard(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();
    let herald = scenario
        .add_creature_to_hand(P0, "Storm Herald", 3, 2)
        .from_oracle_text(STORM_HERALD_ORACLE)
        .id();

    let mut runner = scenario.build();
    let mut etb_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        herald,
        Zone::Battlefield,
        &mut etb_events,
    );
    process_triggers(runner.state_mut(), &etb_events);
    drain_priority_preferring(&mut runner, &[aura, host]);

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Battlefield,
        "Storm Herald must return the Aura"
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "returned Aura must attach to a creature you control"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "delayed exile of those Auras must be installed; delayed={:?}",
        runner.state().delayed_triggers
    );
    match &runner.state().delayed_triggers[0].ability.effect {
        Effect::ChangeZone {
            destination: Zone::Exile,
            target: TargetFilter::TrackedSet { id },
            ..
        }
        | Effect::ChangeZoneAll {
            destination: Zone::Exile,
            target: TargetFilter::TrackedSet { id },
            ..
        } => {
            assert_ne!(
                id.0, 0,
                "TrackedSet sentinel must be rebound at CDT creation; id={id:?}"
            );
            assert!(
                runner
                    .state()
                    .tracked_object_sets
                    .get(id)
                    .is_some_and(|set| set.contains(&aura)),
                "rebound TrackedSet must contain the returned Aura; set={:?}",
                runner.state().tracked_object_sets.get(id)
            );
        }
        other => panic!("delayed body must be exile TrackedSet, got {other:?}"),
    }

    // CR 603.7: Resolve the installed delayed body through the production
    // effect pipeline (same path end-step firing uses). Turn-gate timing for
    // AtNextPhaseForPlayer is covered by the delayed-trigger suite; here we
    // prove the TrackedSet bind + exile semantics for Storm Herald's rem.
    let delayed = runner.state().delayed_triggers[0].ability.clone();
    runner.state_mut().delayed_triggers.clear();
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &delayed, &mut events, 0)
        .expect("delayed exile resolves");
    drain_priority_preferring(&mut runner, &[host]);

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Exile,
        "those Auras must be exiled via the delayed TrackedSet body"
    );
}

#[test]
fn smoke_shroud_attaches_to_entering_ninja_among_multiple_hosts() {
    // CR 303.4f + CR 608.2c: with another legal Aura host on the battlefield,
    // the event-subject return must bind ParentTarget to the entering Ninja —
    // no CR 303.4f prompt, and not the distractor creature.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let distractor = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ninja = scenario
        .add_creature_to_hand(P0, "Ninja of the Deep Hours", 2, 2)
        .with_subtypes(vec!["Human", "Ninja"])
        .id();
    let aura = scenario
        .add_creature(P0, "Smoke Shroud", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(SMOKE_SHROUD_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    // Prior host so AttachedTo fallback would prefer the distractor if the
    // event referent is not hydrated onto the nested Attach.
    attach_to(runner.state_mut(), aura, distractor);
    let mut gy_events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Graveyard, &mut gy_events);
    process_triggers(runner.state_mut(), &gy_events);
    drain_priority(&mut runner);
    assert_eq!(runner.state().objects[&aura].zone, Zone::Graveyard);

    let mut etb_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        ninja,
        Zone::Battlefield,
        &mut etb_events,
    );
    process_triggers(runner.state_mut(), &etb_events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Battlefield,
        "Smoke Shroud returns from GY on Ninja ETB"
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(ninja)),
        "must attach to the entering Ninja, not the distractor ({distractor:?}); \
         attached_to={:?}",
        runner.state().objects[&aura].attached_to
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "must not open CR 303.4f Aura host choice among multiple legal hosts"
    );
}

#[test]
fn necrotic_plague_attaches_to_chosen_creature_not_dying_host() {
    // CR 608.2c + CR 303.4f: Necrotic Plague nests Attach→ParentTarget under a
    // TargetOnly→ChangeZone chain. Drive the printed dies trigger through
    // process_triggers → TriggerTargetSelection → resolution with distinct dying
    // and chosen creatures — no stamp_host / hand-built ResolvedAbility.
    let parsed = parse_oracle_text(
        NECROTIC_PLAGUE_ORACLE,
        "Necrotic Plague",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let dies = parsed
        .triggers
        .iter()
        .find(|t| {
            matches!(
                t.execute.as_ref().map(|e| e.effect.as_ref()),
                Some(Effect::TargetOnly { .. })
            )
        })
        .expect("dies trigger");
    let execute = dies.execute.as_ref().expect("execute");
    assert!(
        execute.forward_result
            || execute
                .sub_ability
                .as_ref()
                .is_some_and(|s| s.forward_result),
        "Necrotic Plague return must forward_result into Attach; execute={:?}",
        execute.effect
    );
    fn change_zone_has_forward_result(def: &engine::types::ability::AbilityDefinition) -> bool {
        let here = matches!(
            def.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        ) && def.forward_result;
        here || def
            .sub_ability
            .as_deref()
            .is_some_and(change_zone_has_forward_result)
            || def
                .else_ability
                .as_deref()
                .is_some_and(change_zone_has_forward_result)
    }
    assert!(
        change_zone_has_forward_result(execute),
        "TargetOnly→ChangeZone[+Attach] must stamp forward_result on ChangeZone; execute={:?}",
        execute.effect
    );
    fn find_attach_parent(def: &engine::types::ability::AbilityDefinition) -> bool {
        matches!(
            def.effect.as_ref(),
            Effect::Attach {
                target: TargetFilter::ParentTarget,
                ..
            }
        ) || def.sub_ability.as_deref().is_some_and(find_attach_parent)
            || def.else_ability.as_deref().is_some_and(find_attach_parent)
    }
    assert!(
        find_attach_parent(execute),
        "Necrotic Plague must nest Attach→ParentTarget; execute={:?}",
        execute.effect
    );
    assert!(
        dies.trigger_zones.contains(&Zone::Battlefield),
        "AttachedTo dies trigger must fire from the battlefield (Gift-shaped), not only GY; \
         trigger_zones={:?}",
        dies.trigger_zones
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let dying = scenario.add_creature(P0, "Dying Host", 2, 2).id();
    let chosen = scenario.add_creature(P1, "Chosen Host", 2, 2).id();
    let other_opp = scenario.add_creature(P1, "Other Opp Creature", 2, 2).id();
    let plague = scenario
        .add_creature(P0, "Necrotic Plague", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(NECROTIC_PLAGUE_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), plague, dying);

    // Mirror Gift: collect dies triggers while the Aura is still on the battlefield
    // (CR 603.6d LKI); SBAs during drain move it to the GY before resolution.
    let mut death_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        dying,
        Zone::Graveyard,
        &mut death_events,
    );
    process_triggers(runner.state_mut(), &death_events);
    assert!(
        !runner.state().stack.is_empty()
            || matches!(
                runner.state().waiting_for,
                WaitingFor::TriggerTargetSelection { .. } | WaitingFor::OrderTriggers { .. }
            ),
        "Necrotic dies trigger must be pending after enchanted creature dies; \
         stack={} waiting={:?}",
        runner.state().stack.len(),
        runner.state().waiting_for
    );
    // CR 704.5m + CR 704.3: Aura with illegal/dead host goes to GY before the
    // pending trigger resolves. The printed return is "from its owner's
    // graveyard", so ChangeZone's origin guard needs the Aura in GY first.
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut death_events);
    assert_eq!(
        runner.state().objects[&plague].zone,
        Zone::Graveyard,
        "SBA must put Necrotic Plague into GY before its return resolves"
    );
    drain_priority_preferring(&mut runner, &[chosen]);

    assert_eq!(
        runner.state().objects[&plague].zone,
        Zone::Battlefield,
        "Necrotic Plague returns from GY"
    );
    assert_eq!(
        runner.state().objects[&plague].attached_to,
        Some(AttachTarget::Object(chosen)),
        "must attach to the chosen opponent creature ({chosen:?}), not the dying host \
         ({dying:?}) or distractor ({other_opp:?}); attached_to={:?}",
        runner.state().objects[&plague].attached_to
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "must not open CR 303.4f Aura host choice when ParentTarget is the chosen creature"
    );
}

/// Oracle text verbatim from `client/public/card-data.json`.
const SWORD_OF_THE_MEEK_ORACLE: &str = "Equipped creature gets +1/+2.\n\
Equip {2}\n\
Whenever a 1/1 creature you control enters, you may return this card from your \
graveyard to the battlefield, then attach it to that creature.";

const AURIOK_SURVIVORS_ORACLE: &str = "When this creature enters, you may return \
target Equipment card from your graveyard to the battlefield. If you do, you may \
attach it to this creature.";

/// CR 400.7j + CR 301.5: "return this card from your graveyard to the
/// battlefield, then attach it to that creature" must equip the entering 1/1.
/// The bare-"it" attachment names the card the same effect just returned
/// (CR 400.7j), not the trigger-event referent; before the parser rebind both
/// operands collapsed onto the entering creature and CR 301.5c's self-attach
/// guard silently swallowed the whole attach.
///
/// Hostile fixture: the Sword has a *prior host* and there is a *second 1/1*
/// already on the battlefield, so an `AttachedTo` LKI fallback or a battlefield
/// scan binds the wrong permanent and fails.
#[test]
fn sword_of_the_meek_attaches_to_entering_one_one_not_the_prior_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let prior_host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let distractor = scenario.add_creature(P0, "Memnite", 1, 1).id();
    let one_one = scenario.add_creature_to_hand(P0, "Ornithopter", 1, 1).id();
    let sword = scenario
        .add_creature(P0, "Sword of the Meek", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(SWORD_OF_THE_MEEK_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), sword, prior_host);

    let mut gy_events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), sword, Zone::Graveyard, &mut gy_events);
    process_triggers(runner.state_mut(), &gy_events);
    drain_priority(&mut runner);
    assert_eq!(runner.state().objects[&sword].zone, Zone::Graveyard);

    let mut etb_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        one_one,
        Zone::Battlefield,
        &mut etb_events,
    );
    process_triggers(runner.state_mut(), &etb_events);
    let consumed_effect_zone_choice = drain_priority(&mut runner);

    // Positive reach-guard: the return ran, so execution reached the Attach.
    assert_eq!(
        runner.state().objects[&sword].zone,
        Zone::Battlefield,
        "Sword of the Meek returns from the graveyard on the 1/1's ETB"
    );
    assert_eq!(
        runner.state().objects[&sword].attached_to,
        Some(AttachTarget::Object(one_one)),
        "must equip the entering 1/1 ({one_one:?}), not the prior host \
         ({prior_host:?}) or the distractor 1/1 ({distractor:?}); attached_to={:?}",
        runner.state().objects[&sword].attached_to
    );
    assert!(
        runner.state().objects[&one_one]
            .attachments
            .contains(&sword),
        "the entering 1/1 must list the Sword as attached"
    );
    assert!(
        !consumed_effect_zone_choice,
        "a SelfRef attachment must not consume a resolution-time attachment choice"
    );
}

/// CR 301.5e + CR 608.2c: "return …, **then** attach it" performs the return
/// unconditionally — only the attach can fail. This is the observable that
/// distinguishes it from the Aura "return … **attached to** that creature"
/// family, whose CR 303.4i/CR 704.5m denial keeps the card in the graveyard
/// (`gift_of_immortality_stays_in_graveyard_when_host_gone`, above). The rebind
/// routes the Sword through the same `enter_attached_to` delivery slot those
/// Auras use, so this guard pins that the slot stays a delivery mechanism for an
/// Equipment rather than becoming an entry gate.
///
/// Not revert-failing (reverted, the Sword also enters unattached) — Claim 1a
/// above carries that burden. The prior host is retained so the `AttachedTo`
/// fallback would bind it if the primary referent ever stopped resolving.
#[test]
fn sword_of_the_meek_returns_unattached_when_the_entering_creature_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let prior_host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let one_one = scenario.add_creature_to_hand(P0, "Ornithopter", 1, 1).id();
    let sword = scenario
        .add_creature(P0, "Sword of the Meek", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(SWORD_OF_THE_MEEK_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), sword, prior_host);

    let mut gy_events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), sword, Zone::Graveyard, &mut gy_events);
    process_triggers(runner.state_mut(), &gy_events);
    drain_priority(&mut runner);

    let mut etb_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        one_one,
        Zone::Battlefield,
        &mut etb_events,
    );
    process_triggers(runner.state_mut(), &etb_events);
    // Reach-guard: the trigger is actually pending before the host is removed,
    // otherwise "the Sword entered" below could pass without the trigger firing.
    assert!(
        !runner.state().stack.is_empty()
            || matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. } | WaitingFor::OrderTriggers { .. }
            ),
        "the return trigger must be pending; stack={} waiting={:?}",
        runner.state().stack.len(),
        runner.state().waiting_for
    );

    let mut death_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        one_one,
        Zone::Graveyard,
        &mut death_events,
    );
    process_triggers(runner.state_mut(), &death_events);
    assert_eq!(runner.state().objects[&one_one].zone, Zone::Graveyard);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&sword].zone,
        Zone::Battlefield,
        "CR 608.2c: the return is not gated on the attach — unlike an Aura's \
         CR 303.4i return, the Equipment enters even with no legal host"
    );
    assert_eq!(
        runner.state().objects[&sword].attached_to,
        None,
        "CR 301.5e: with an undefined host the Equipment enters unattached — \
         not re-bound to the stale prior host ({prior_host:?})"
    );
}

/// Auriok Survivors shares Sword of the Meek's collapsed-anaphor shape, so the
/// rebind changes its AST too. Its *recipient* operand ("attach it to this
/// creature") is a separate, unfixed misparse, so the card still asks to attach
/// the returned Equipment to itself — a CR 301.5c no-op. This guard pins that
/// the newly-stamped self-host is neutralized rather than producing a corrupt
/// self-attached state.
#[test]
fn auriok_survivors_returned_equipment_enters_unattached() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let equipment = scenario
        .add_creature_to_graveyard(P0, "Bonesplitter", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let survivors = scenario
        .add_creature_to_hand(P0, "Auriok Survivors", 3, 5)
        .from_oracle_text(AURIOK_SURVIVORS_ORACLE)
        .id();

    let mut runner = scenario.build();
    let mut etb_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        survivors,
        Zone::Battlefield,
        &mut etb_events,
    );
    process_triggers(runner.state_mut(), &etb_events);
    drain_priority_preferring(&mut runner, &[equipment]);

    assert_eq!(
        runner.state().objects[&equipment].zone,
        Zone::Battlefield,
        "the targeted Equipment returns from the graveyard"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        None,
        "CR 301.5c + CR 301.5e: an Equipment can't equip itself, so the \
         unfixed recipient anaphor leaves it entering unattached rather than \
         producing a self-attached state"
    );
}
