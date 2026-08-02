//! Kang Dynasty — "until your next turn" delayed combat-damage rider (Gap C).
//!
//! Verified Oracle text (`client/public/card-data.json`,
//! `jq '.["kang dynasty"].oracle_text'`), chapters I/II:
//!   "For each opponent, tap up to one target creature that player controls. Goad
//!    those creatures. Until your next turn, whenever any of those creatures deals
//!    combat damage to a player, draw a card."
//!
//! Pins:
//!   - Gap A: "any of those creatures" → `ParentTarget`, `DamageDone`/`CombatOnly`,
//!     recipient a player; inner `Draw`.
//!   - Gap C: the rider's stated "until your next turn" duration lands on the
//!     `WheneverEvent`'s `expiry` (`UntilControllersNextTurn`), NOT the enclosing
//!     ability's `duration`. This is load-bearing: goaded creatures attack on
//!     opponents' turns AFTER the creating turn's cleanup (CR 701.15a), so a
//!     default `EndOfTurn` `WheneverEvent` would be purged before it could fire.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DamageKindFilter, DelayedTriggerCondition, Effect,
    QuantityExpr, TargetFilter, TriggerDefinition, TurnGate, WheneverEventExpiry,
};
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

/// Chapters I/II effect chain (the tap/goad/rider clause).
const KANG_CHAPTER: &str = "For each opponent, tap up to one target creature that \
    player controls. Goad those creatures. Until your next turn, whenever any of \
    those creatures deals combat damage to a player, draw a card.";

fn find_delayed(ability: &AbilityDefinition) -> &Effect {
    let mut cur = ability;
    loop {
        if matches!(&*cur.effect, Effect::CreateDelayedTrigger { .. }) {
            return &cur.effect;
        }
        cur = cur
            .sub_ability
            .as_deref()
            .expect("CreateDelayedTrigger must appear in the chapter chain");
    }
}

#[test]
fn chapter_rider_parses_until_next_turn_expiry_and_parent_target() {
    let def = parse_effect_chain(KANG_CHAPTER, AbilityKind::Spell);
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = find_delayed(&def)
    else {
        unreachable!("find_delayed returns a CreateDelayedTrigger");
    };
    let DelayedTriggerCondition::WheneverEvent { trigger, expiry } = condition else {
        panic!("expected WheneverEvent, got {condition:?}");
    };

    // Gap A.
    assert_eq!(trigger.mode, TriggerMode::DamageDone, "not Unknown");
    assert_eq!(trigger.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(
        trigger.valid_source,
        Some(TargetFilter::ParentTarget),
        "'any of those creatures' → ParentTarget"
    );
    assert_eq!(trigger.valid_target, Some(TargetFilter::Player));

    // Gap C: the duration is carried on the WheneverEvent expiry (symbolic at
    // parse time), NOT on the ability's `duration`.
    assert_eq!(
        *expiry,
        WheneverEventExpiry::UntilControllersNextTurn {
            after: TurnGate::AfterCreationTurn,
        },
        "'until your next turn' → WheneverEventExpiry::UntilControllersNextTurn"
    );

    // Inner effect: draw a card.
    assert!(
        matches!(&*effect.effect, Effect::Draw { .. }),
        "inner effect draws"
    );
}

/// Scope-evidence class guard (PR #6884): "until your next turn, whenever …"
/// delayed triggers on cards OTHER than Kang Dynasty. The parse-diff showed 12
/// such cards whose CreateDelayedTrigger `duration` field changed — this pins the
/// two signatures so the class behavior can't silently regress:
///   - Sig 1 (Don't Move / A Display / Davriel class): a plain inner effect →
///     the "until your next turn" moves ENTIRELY to the WheneverEvent expiry and
///     the creator ability keeps no `duration`.
///   - Sig 2 (Jace, Architect of Thought / Tamiyo class): an inner "… until end
///     of turn" buff → the "until your next turn" still moves to the expiry, and
///     the residual `UntilEndOfTurn` surfaces on the creator ability.
///
/// In BOTH cases the load-bearing fix is identical and correct: the expiry is
/// `UntilControllersNextTurn`, so the trigger fires on opponents' turns instead
/// of being purged at the creating turn's cleanup (the pre-fix default-EndOfTurn
/// behavior, CR 603.7b).
#[test]
fn until_next_turn_delayed_trigger_relocates_duration_to_expiry_across_class() {
    fn delayed_ability(def: &AbilityDefinition) -> &AbilityDefinition {
        let mut cur = def;
        loop {
            if matches!(&*cur.effect, Effect::CreateDelayedTrigger { .. }) {
                return cur;
            }
            cur = cur
                .sub_ability
                .as_deref()
                .expect("class fixture must contain a CreateDelayedTrigger");
        }
    }

    // Sig 1: plain inner effect (Don't Move's "destroy it").
    let sig1 = parse_effect_chain(
        "Until your next turn, whenever a creature becomes tapped, destroy it.",
        AbilityKind::Spell,
    );
    let sig1_ability = delayed_ability(&sig1);
    let Effect::CreateDelayedTrigger { condition, .. } = &*sig1_ability.effect else {
        unreachable!();
    };
    let DelayedTriggerCondition::WheneverEvent { expiry, .. } = condition else {
        panic!("expected WheneverEvent, got {condition:?}");
    };
    assert_eq!(
        *expiry,
        WheneverEventExpiry::UntilControllersNextTurn {
            after: TurnGate::AfterCreationTurn,
        },
        "sig1: 'until your next turn' must land on the expiry"
    );
    assert_eq!(
        sig1_ability.duration, None,
        "sig1: a plain inner effect leaves no residual duration on the creator ability"
    );

    // Sig 2: inner "… until end of turn" buff (Jace, Architect of Thought's +1).
    let sig2 = parse_effect_chain(
        "Until your next turn, whenever a creature an opponent controls attacks, \
         it gets -1/-0 until end of turn.",
        AbilityKind::Spell,
    );
    let sig2_ability = delayed_ability(&sig2);
    let Effect::CreateDelayedTrigger { condition, .. } = &*sig2_ability.effect else {
        unreachable!();
    };
    let DelayedTriggerCondition::WheneverEvent { expiry, .. } = condition else {
        panic!("expected WheneverEvent, got {condition:?}");
    };
    assert_eq!(
        *expiry,
        WheneverEventExpiry::UntilControllersNextTurn {
            after: TurnGate::AfterCreationTurn,
        },
        "sig2: 'until your next turn' must STILL land on the expiry, not be shadowed \
         by the inner 'until end of turn'"
    );
    assert_eq!(
        sig2_ability.duration,
        Some(engine::types::ability::Duration::UntilEndOfTurn),
        "sig2: the inner buff's residual 'until end of turn' surfaces on the creator ability"
    );
}

/// Turn-structure pump (auto no-attacks/blocks, drain trigger order, no-op
/// cleanup discard, pass priority).
fn pump(runner: &mut GameRunner) -> bool {
    use engine::types::game_state::WaitingFor;
    use engine::types::GameAction;
    match runner.state().waiting_for.clone() {
        WaitingFor::DeclareAttackers { .. } => runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .is_ok(),
        WaitingFor::DeclareBlockers { .. } => runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .is_ok(),
        WaitingFor::OrderTriggers { .. } => {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            true
        }
        WaitingFor::DiscardChoice { .. } => runner
            .act(GameAction::SelectCards { cards: vec![] })
            .is_ok(),
        WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority).is_ok(),
        _ => false,
    }
}

fn advance_past_turn(runner: &mut GameRunner, from_turn: u32) {
    for _ in 0..400 {
        if runner.state().turn_number > from_turn {
            return;
        }
        if !pump(runner) {
            break;
        }
    }
    assert!(
        runner.state().turn_number > from_turn,
        "stalled advancing past turn {from_turn} (now turn {}, phase {:?})",
        runner.state().turn_number,
        runner.state().phase
    );
}

/// Build the Kang-shaped rider programmatically (its shape is pinned by the
/// parse test above) and install it via the production `resolve_ability_chain`.
fn install_kang_rider(state: &mut GameState, source: engine::types::identifiers::ObjectId) {
    let mut trigger = TriggerDefinition::new(TriggerMode::DamageDone);
    trigger.damage_kind = DamageKindFilter::CombatOnly;
    // SelfRef source keeps the empty-set guard out of scope (it applies only to a
    // pre-bind ParentTarget source); Gap C retention/purge is independent of the
    // source filter.
    trigger.valid_source = Some(TargetFilter::SelfRef);
    trigger.valid_target = Some(TargetFilter::Player);
    let inner = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WheneverEvent {
                trigger: Box::new(trigger),
                expiry: WheneverEventExpiry::UntilControllersNextTurn {
                    after: TurnGate::AfterCreationTurn,
                },
            },
            effect: Box::new(inner),
            uses_tracked_set: false,
        },
    );
    let resolved = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(state, &resolved, &mut events, 0)
        .expect("Kang rider installs via resolve_ability_chain");
}

/// Gap C (load-bearing): an "until your next turn" `WheneverEvent` SURVIVES the
/// creating turn's cleanup (retention disjunct) and is PURGED at the controller's
/// next turn's untap. Reverting the retention disjunct drops it at the creating
/// turn's cleanup (fails the survives-assert); reverting the untap purge leaks it
/// forever (fails the purged-assert).
#[test]
fn until_next_turn_rider_survives_intervening_turn_then_purges() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Kang Dynasty", "Enchantment.")
        .id();
    // Stock both libraries so multi-turn draw steps don't cause a draw-from-empty
    // loss that would end the game before the purge boundary.
    for i in 0..12 {
        scenario.add_card_to_library_top(P0, &format!("P0 Lib {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Lib {i}"));
    }
    let mut runner = scenario.build();

    let creation_turn = runner.state().turn_number;
    install_kang_rider(runner.state_mut(), source);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "rider installed on the creating turn"
    );

    // Cross P0's cleanup into the opponent's (intervening) turn.
    advance_past_turn(&mut runner, creation_turn);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "CR 603.7b: an until-your-next-turn WheneverEvent survives the creating \
         turn's cleanup (fires on the opponent's turn)"
    );

    // Cross the opponent's turn into P0's next turn (untap purges it).
    let opponent_turn = runner.state().turn_number;
    advance_past_turn(&mut runner, opponent_turn);
    assert_eq!(
        runner.state().active_player,
        P0,
        "advanced to the controller's next turn"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 603.7b / CR 502.4: the rider is purged at the controller's next turn's \
         untap step (does not leak into later turns)"
    );
}
