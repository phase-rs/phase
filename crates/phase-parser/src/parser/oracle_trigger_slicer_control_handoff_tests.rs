use crate::parser::oracle::parse_oracle_text;
use crate::types::ability::{
    AbilityDefinition, ControllerRef, Effect, EffectScope, FilterProp, TapStateChange, TargetFilter,
};
use crate::types::TriggerMode;

/// Walk a chained `AbilityDefinition` collecting one effect per node (parent
/// then each nested `sub_ability`). Lets the regression assert the FULL set
/// of sub-effects in a "you may pay; if you do, A, B, and C" chain — the
/// Slicer defect dropped the trailing conjunct entirely (issue #2032).
fn flatten_effects(def: &AbilityDefinition) -> Vec<&Effect> {
    let mut out = Vec::new();
    let mut node = Some(def);
    while let Some(d) = node {
        out.push(&*d.effect);
        node = d.sub_ability.as_deref();
    }
    out
}

/// CR 613.1b + CR 110.2 (issue #2032): Slicer, Hired Muscle's attack trigger
/// is "you may pay {2}. If you do, untap it, goad it, and an opponent of your
/// choice gains control of it." Before the fix the chunk splitter failed to
/// recognize the trailing "an opponent of your choice gains control of it"
/// conjunct as a clause, so the control-handoff sub-effect was silently
/// dropped — Slicer untapped (and goaded) but never changed control. The
/// trigger must lower to ALL of Untap + Goad + GiveControl(recipient=Opponent).
#[test]
fn slicer_attack_trigger_includes_all_three_sub_effects() {
    let parsed = parse_oracle_text(
            "Whenever Slicer, Hired Muscle attacks, you may pay {2}. If you do, untap it, goad it, and an opponent of your choice gains control of it.",
            "Slicer, Hired Muscle",
            &[],
            &["Artifact".into(), "Creature".into()],
            &["Equipment".into()],
        );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::Attacks))
        .expect("attack trigger must parse");
    let execute = trigger.execute.as_ref().expect("execute must be Some");
    let effects = flatten_effects(execute);

    // PayCost → Untap → Goad → GiveControl, all present.
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SetTapState {
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
                ..
            }
        )),
        "untap sub-effect must be present, got {effects:#?}",
    );
    assert!(
        effects.iter().any(|e| matches!(e, Effect::Goad { .. })),
        "goad sub-effect must be present, got {effects:#?}",
    );
    // The trailing conjunct — the sub-effect that was being dropped.
    let give = effects
        .iter()
        .find_map(|e| match e {
            Effect::GiveControl { recipient, .. } => Some(recipient),
            _ => None,
        })
        .expect("control-handoff (GiveControl) sub-effect must not be dropped");
    assert_eq!(
        *give,
        TargetFilter::Typed(
            crate::types::ability::TypedFilter::default().controller(ControllerRef::Opponent)
        ),
        "GiveControl recipient must be an opponent (CR 110.2), got {give:?}",
    );
}

/// Building-block coverage: a bare-`and` two-clause chain "untap it and an
/// opponent gains control of it" must also split the player-subject control
/// handoff into its own clause (not an `Unimplemented { name: "an" }`
/// fallback that drops the transfer). Exercises the
/// `starts_player_gains_control_clause` recognizer on the bare-`and` boundary
/// independent of the comma form. The control transfer may lower either to a
/// direct `GiveControl` (when the recipient is fully determined, e.g. "an
/// opponent of your choice") or to a `Choose(Opponent)` + `GainControl` pair
/// (when "an opponent" needs an explicit selection step) — both are correct
/// CR 110.2 handoffs; what must never happen is the clause being dropped.
#[test]
fn player_gains_control_splits_on_bare_and() {
    let parsed = parse_oracle_text(
        "Whenever ~ attacks, untap it and an opponent gains control of it.",
        "Test Card",
        &[],
        &["Artifact".into(), "Creature".into()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::Attacks))
        .expect("attack trigger must parse");
    let effects = flatten_effects(trigger.execute.as_ref().expect("execute"));
    // The untap clause must survive…
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SetTapState {
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
                ..
            }
        )),
        "untap sub-effect must be present, got {effects:#?}",
    );
    // …and the control handoff must be present in some valid lowered form,
    // never an Unimplemented drop.
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::GiveControl { .. } | Effect::GainControl { .. })),
        "bare-and player control handoff must lower to a control transfer, got {effects:#?}",
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Unimplemented { .. })),
        "control-handoff clause must not be dropped as Unimplemented, got {effects:#?}",
    );
}

/// Regression test for issue #2346: Grenzo, Havoc Raiser - DamageDone triggers
/// with inline modal choices must scope "that player" in each mode body to the
/// damaged player (TriggeringPlayer), not ParentTargetController.
///
/// Asserts the lowered Goad and ExileTop effects directly so the test is
/// discriminating: it fails if mode bodies produce TargetOnly/Unimplemented or
/// if "that player" resolves to the wrong player.
#[test]
fn damage_done_trigger_uses_triggering_player_for_that_player() {
    let parsed = parse_oracle_text(
            "Whenever a creature you control deals combat damage to a player, choose one \u{2014} goad target creature that player controls; or exile the top card of that player's library.",
            "Grenzo, Havoc Raiser",
            &[],
            &["Artifact".into(), "Creature".into()],
            &["Equipment".into()],
        );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::DamageDone))
        .expect("DamageDone trigger must parse");

    assert_eq!(trigger.mode, TriggerMode::DamageDone);

    let execute = trigger.execute.as_ref().expect("execute must be Some");

    // The execute must have a modal with two mode abilities
    let modal = execute.modal.as_ref().expect("execute must carry a modal");
    assert_eq!(modal.mode_count, 2, "must have two modes, got {:?}", modal);

    let mode_abilities = &execute.mode_abilities;
    assert_eq!(
        mode_abilities.len(),
        2,
        "must have two mode ability entries, got {:?}",
        mode_abilities
    );

    // Mode 0: Goad — target creature that player (TriggeringPlayer) controls
    let mode0_effects = flatten_effects(&mode_abilities[0]);
    let goad_controller = mode0_effects
        .iter()
        .find_map(|e| match e {
            Effect::Goad {
                target: TargetFilter::Typed(tf),
            } => Some(tf.controller.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!("mode 0 must contain Goad with Typed filter, got {mode0_effects:?}")
        });
    assert_eq!(
        goad_controller,
        Some(ControllerRef::TriggeringPlayer),
        "Goad target controller must be TriggeringPlayer (the damaged player), got {:?}",
        goad_controller
    );

    // Mode 1: ExileTop — exile the top card of that player's (TriggeringPlayer) library
    let mode1_effects = flatten_effects(&mode_abilities[1]);
    let exile_player = mode1_effects
        .iter()
        .find_map(|e| match e {
            Effect::ExileTop { player, .. } => Some(player.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("mode 1 must contain ExileTop, got {mode1_effects:?}"));
    assert_eq!(
        exile_player,
        TargetFilter::TriggeringPlayer,
        "ExileTop player must be TriggeringPlayer (the damaged player), got {:?}",
        exile_player
    );
}

/// CR 603.10 + CR 603.4 (issue #2861): Prized Amalgam is a graveyard-zone
/// delayed-return trigger. It fires from the graveyard whenever *another*
/// creature enters, gated by the intervening-if "if it entered from your
/// graveyard or you cast it from your graveyard". The parser must:
///   1. set `trigger_zones = [Graveyard]` (the source is in the graveyard
///      when it fires — NOT the battlefield default), and
///   2. attach the graveyard-origin intervening-if condition (not `None`).
#[test]
fn prized_amalgam_graveyard_origin_intervening_if() {
    use crate::types::ability::TriggerCondition;
    use crate::types::Zone;
    let parsed = parse_oracle_text(
        "Whenever a creature enters, if it entered from your graveyard or \
             you cast it from your graveyard, return this card from your graveyard \
             to the battlefield tapped at the beginning of the next end step.",
        "Prized Amalgam",
        &[],
        &["Creature".into()],
        &["Zombie".into()],
    );
    let def = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::ChangesZone))
        .expect("ChangesZone trigger must parse");
    // (1) graveyard-zone trigger: the source lives in the graveyard.
    assert!(
        def.trigger_zones.contains(&Zone::Graveyard),
        "trigger must fire from the graveyard, got trigger_zones={:?}",
        def.trigger_zones
    );
    assert!(
        !def.trigger_zones.contains(&Zone::Battlefield),
        "trigger must NOT fire from the battlefield, got trigger_zones={:?}",
        def.trigger_zones
    );
    // (2) graveyard-origin intervening-if must be preserved (not dropped).
    let condition = def
        .condition
        .as_ref()
        .expect("graveyard-origin intervening-if must be parsed, got condition=None");
    // Must reference the graveyard origin (entered-from or cast-from).
    fn references_graveyard_origin(c: &TriggerCondition) -> bool {
        match c {
            TriggerCondition::ZoneChangeObjectMatchesFilter {
                origin: Some(Zone::Graveyard),
                ..
            } => true,
            TriggerCondition::WasCast {
                zone: Some(Zone::Graveyard),
                ..
            } => true,
            TriggerCondition::Or { conditions } | TriggerCondition::And { conditions } => {
                conditions.iter().any(references_graveyard_origin)
            }
            TriggerCondition::Not { condition } => references_graveyard_origin(condition),
            _ => false,
        }
    }
    assert!(
        references_graveyard_origin(condition),
        "condition must gate on graveyard origin, got {condition:?}"
    );
    match condition {
        TriggerCondition::Or { conditions } => {
            assert!(
                matches!(
                    &conditions[0],
                    TriggerCondition::ZoneChangeObjectMatchesFilter {
                        filter: TargetFilter::Typed(typed),
                        ..
                    } if typed.properties.contains(&FilterProp::Owned {
                        controller: ControllerRef::You
                    })
                ),
                "entered-from-your-graveyard branch must be owner-scoped to you, got {condition:?}"
            );
            assert!(
                matches!(
                    &conditions[1],
                    TriggerCondition::WasCast {
                        zone: Some(Zone::Graveyard),
                        controller: Some(ControllerRef::You),
                        owner: Some(ControllerRef::You),
                    }
                ),
                "cast-from-your-graveyard branch must scope BOTH caster (\"you cast it\") \
                     and origin-zone owner (\"your graveyard\") to you, got {condition:?}"
            );
        }
        other => panic!("expected Or condition, got {other:?}"),
    }
}
