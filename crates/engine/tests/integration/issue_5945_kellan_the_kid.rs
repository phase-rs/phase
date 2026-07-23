//! Issue #5945 — Kellan, the Kid "ability doesn't exist".
//!
//! Kellan, the Kid ({G}{W}{U} Legendary Creature — Human Faerie Rogue):
//!   Flying, lifelink
//!   Whenever you cast a spell from anywhere other than your hand, you may cast a
//!   permanent spell with equal or lesser mana value from your hand without
//!   paying its mana cost. If you don't, you may put a land card from your hand
//!   onto the battlefield.
//!
//! The parser already produces a structurally-complete AST (an optional
//! `CastFromZone` hand-pick with a `Not(OptionalEffectPerformed)` land-drop sub).
//! The bug was at runtime: the hand-pick `CastFromZone` self-installs its own
//! resolution-time continuation, but the generic sub-stash in `resolve_chain_body`
//! prepended the land-drop ahead of it, so the `EffectZoneChoice` resume fed the
//! wrong ability into `complete_hand_pick_cast_from_zone` and errored
//! (`MissingParam("CastFromZone")`) — the user-visible "ability doesn't exist".
//!
//! Three defects were fixed and are pinned below:
//!   * Fix A — skip the generic sub-stash for a hand-pick `CastFromZone` that
//!     installed its own continuation (T2/T3).
//!   * Fix B — an empty selection ("If you don't") re-stashes the land-drop with
//!     `optional_effect_performed` reset to false (T4).
//!   * Fix C — an empty eligible pool makes the outer optional infeasible so the
//!     land-drop is offered directly rather than suppressed (T5).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CastingPermission, Effect,
    EffectOutcomeSignal, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use std::sync::Arc;

const KELLAN: &str = "Flying, lifelink\nWhenever you cast a spell from anywhere other than your hand, you may cast a permanent spell with equal or lesser mana value from your hand without paying its mana cost. If you don't, you may put a land card from your hand onto the battlefield.";

/// The subless analog (drops the "If you don't" land-drop clause) — an
/// Electrodominance-class optional hand cast, used to prove the fixes leave the
/// no-fallback path untouched (T7).
const KELLAN_SUBLESS: &str = "Flying, lifelink\nWhenever you cast a spell from anywhere other than your hand, you may cast a permanent spell with equal or lesser mana value from your hand without paying its mana cost.";

/// Create a spell in `P0`'s exile with a `{0}` `ExileWithAltCost` permission and
/// the given mana value, so casting it fires Kellan's "cast a spell from anywhere
/// other than your hand" trigger and the `equal or lesser mana value` filter
/// binds to this spell's mana value (CR 202.3). Mirrors the working repro harness.
fn add_free_exile_spell(runner: &mut GameRunner, mana_value: u32) -> ObjectId {
    let id = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(7777),
        P0,
        "Exile Bolt".to_string(),
        Zone::Exile,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::generic(mana_value);
    obj.base_mana_cost = ManaCost::generic(mana_value);
    let ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    Arc::make_mut(&mut obj.abilities).push(ability.clone());
    Arc::make_mut(&mut obj.base_abilities).push(ability);
    obj.casting_permissions
        .push(CastingPermission::ExileWithAltCost {
            cost: ManaCost::zero(),
            cast_transformed: false,
            constraint: None,
            granted_to: Some(P0),
            resolution_cleanup: None,
            duration: None,
            graveyard_replacement: None,
            enters_with_counter: None,
            enters_with_modifications: Vec::new(),
            mana_spend_permission: None,
        });
    id
}

fn cast_free_exile_spell(runner: &mut GameRunner, exile: ObjectId) {
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner
        .act(GameAction::CastSpell {
            object_id: exile,
            card_id: CardId(7777),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast exile spell");
}

/// Pass priority / drain trigger ordering until the game halts at an interactive
/// prompt (or at an empty-stack `Priority`). Never answers `OptionalEffectChoice`
/// or `EffectZoneChoice` — the caller asserts on and drives those explicitly.
fn advance_to_prompt(runner: &mut GameRunner, max_steps: usize) -> WaitingFor {
    for _ in 0..max_steps {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return runner.state().waiting_for.clone();
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => return other,
        }
    }
    panic!("advance_to_prompt exceeded {max_steps} steps");
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).map(|o| o.zone).unwrap()
}

/// T1 (SHAPE, anti-paraphrase): the parsed trigger is an optional `CastFromZone`
/// hand-pick whose sub is a `Not(OptionalEffectPerformed)`-gated land-drop
/// `ChangeZone` to the battlefield. Guards the runtime tests below against
/// resting on a paraphrased AST.
#[test]
fn kellan_parses_optional_cast_with_not_performed_land_fallback() {
    let parsed = parse_oracle_text(
        KELLAN,
        "Kellan, the Kid",
        &[],
        &["Creature".to_string()],
        &[],
    );

    let execute = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.as_ref())
        .filter(|exec| matches!(*exec.effect, Effect::CastFromZone { .. }))
        .expect("trigger with a CastFromZone execute");

    assert!(
        matches!(
            *execute.effect,
            Effect::CastFromZone {
                without_paying_mana_cost: true,
                ..
            }
        ),
        "head casts without paying mana cost"
    );
    assert!(execute.optional, "the cast is optional (\"you may cast\")");

    let sub = execute
        .sub_ability
        .as_ref()
        .expect("the \"If you don't\" land-drop sub");
    assert!(
        matches!(
            *sub.effect,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        ),
        "sub puts a card onto the battlefield"
    );
    assert!(sub.optional, "the land drop is optional (\"you may put\")");
    // CR 608.2c / CR 608.2d: "If you don't" == run the sub only when the optional
    // effect was NOT performed.
    assert!(
        matches!(
            sub.condition,
            Some(AbilityCondition::Not { ref condition })
                if matches!(
                    **condition,
                    AbilityCondition::EffectOutcome {
                        signal: EffectOutcomeSignal::OptionalEffectPerformed
                    }
                )
        ),
        "sub is gated on Not(OptionalEffectPerformed), got {:?}",
        sub.condition
    );
}

/// T2 + T3: accepting Kellan's trigger casts the chosen hand permanent for free
/// (no "ability doesn't exist" error, Fix A), and because a spell WAS cast the
/// "If you don't" land drop is NOT offered (the land stays in hand).
#[test]
fn accepting_casts_hand_permanent_free_and_skips_land_drop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kellan, the Kid", 3, 3)
        .from_oracle_text(KELLAN);
    let bear = scenario
        .add_creature_to_hand(P0, "Hand Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let land = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let exile = add_free_exile_spell(&mut runner, 3);

    cast_free_exile_spell(&mut runner, exile);

    // Kellan's optional cast.
    let p = advance_to_prompt(&mut runner, 40);
    assert!(matches!(p, WaitingFor::OptionalEffectChoice { .. }));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the cast");

    // The hand-pick — the eligible pool contains the MV-1 bear.
    match advance_to_prompt(&mut runner, 40) {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            assert!(cards.contains(&bear), "bear is an eligible cast target");
        }
        other => panic!("expected EffectZoneChoice for the cast pick, got {other:?}"),
    }
    // Selecting the bear must succeed (Fix A: no MissingParam("CastFromZone")).
    runner
        .act(GameAction::SelectCards { cards: vec![bear] })
        .expect("select the bear to cast for free");

    // A spell was cast, so the "If you don't" land drop must NOT be offered.
    assert!(
        matches!(
            advance_to_prompt(&mut runner, 40),
            WaitingFor::Priority { .. }
        ),
        "no land-drop prompt after casting"
    );
    assert_eq!(
        zone_of(&runner, bear),
        Zone::Battlefield,
        "bear cast for free"
    );
    assert_eq!(
        zone_of(&runner, land),
        Zone::Hand,
        "land stays in hand — the land drop is skipped when you cast"
    );
}

/// T4 (GAP 1): declining the cast at the hand pick (empty selection) offers the
/// "If you don't" land drop, and accepting it puts the land onto the battlefield.
#[test]
fn declining_the_cast_offers_the_land_drop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kellan, the Kid", 3, 3)
        .from_oracle_text(KELLAN);
    let bear = scenario
        .add_creature_to_hand(P0, "Hand Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let land = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let exile = add_free_exile_spell(&mut runner, 3);

    cast_free_exile_spell(&mut runner, exile);

    assert!(matches!(
        advance_to_prompt(&mut runner, 40),
        WaitingFor::OptionalEffectChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the cast");

    assert!(matches!(
        advance_to_prompt(&mut runner, 40),
        WaitingFor::EffectZoneChoice { .. }
    ));
    // Decline the cast by selecting no card.
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("decline the cast (empty selection)");

    // GAP 1: the land drop is offered immediately after the empty selection.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "land drop offered after declining the cast, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the land drop");

    // The land is the only land in hand, so the "put a land onto the
    // battlefield" ChangeZone auto-selects it (a single eligible, non-`up-to`
    // zone move needs no `EffectZoneChoice`) and the game returns to priority.
    advance_to_prompt(&mut runner, 40);
    assert_eq!(
        zone_of(&runner, land),
        Zone::Battlefield,
        "land entered via the \"If you don't\" fallback"
    );
    assert_eq!(zone_of(&runner, bear), Zone::Hand, "bear was not cast");
}

/// T5 (GAP 3): with no eligible permanent to cast, the outer optional is
/// infeasible, so the land drop is offered directly (rather than the cast option
/// being offered and then silently suppressing the land drop).
#[test]
fn empty_eligible_pool_offers_land_drop_directly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kellan, the Kid", 3, 3)
        .from_oracle_text(KELLAN);
    // Only permanent in hand is MV 3 — ineligible against an MV-2 trigger spell.
    let big = scenario
        .add_creature_to_hand(P0, "Hand Ogre", 4, 4)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let land = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let exile = add_free_exile_spell(&mut runner, 2);

    cast_free_exile_spell(&mut runner, exile);

    // Drive to completion accepting every optional and selecting the land whenever
    // a hand pick offers it. Because the cast pool is empty, the only optional is
    // the land drop (Fix C); without Fix C the land drop is suppressed and the
    // land stays in hand.
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
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
                let pick = if cards.contains(&land) {
                    vec![land]
                } else {
                    vec![]
                };
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("select");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(
        zone_of(&runner, land),
        Zone::Battlefield,
        "land drop offered directly when no permanent is castable"
    );
    assert_eq!(
        zone_of(&runner, big),
        Zone::Hand,
        "ineligible ogre not cast"
    );
}

/// T6: the "equal or lesser mana value" filter binds to the triggering spell's
/// mana value, so the eligible pool includes a permanent at exactly that value
/// and excludes one above it.
#[test]
fn cast_pool_respects_triggering_spell_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kellan, the Kid", 3, 3)
        .from_oracle_text(KELLAN);
    let at_value = scenario
        .add_creature_to_hand(P0, "Even Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let above_value = scenario
        .add_creature_to_hand(P0, "Big Bear", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut runner = scenario.build();
    let exile = add_free_exile_spell(&mut runner, 2);

    cast_free_exile_spell(&mut runner, exile);

    assert!(matches!(
        advance_to_prompt(&mut runner, 40),
        WaitingFor::OptionalEffectChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the cast");

    match advance_to_prompt(&mut runner, 40) {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            assert!(
                cards.contains(&at_value),
                "MV == trigger MV is eligible (CR 202.3)"
            );
            assert!(!cards.contains(&above_value), "MV > trigger MV is excluded");
        }
        other => panic!("expected EffectZoneChoice, got {other:?}"),
    }
}

/// T7: a subless optional hand cast (Electrodominance-class) is unaffected —
/// declining with an empty selection consumes the continuation with no crash, no
/// card enters, and the game returns to priority (the consume-and-no-op path).
#[test]
fn subless_hand_cast_decline_is_a_clean_no_op() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kellan, the Kid", 3, 3)
        .from_oracle_text(KELLAN_SUBLESS);
    let bear = scenario
        .add_creature_to_hand(P0, "Hand Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    let exile = add_free_exile_spell(&mut runner, 3);

    cast_free_exile_spell(&mut runner, exile);

    assert!(matches!(
        advance_to_prompt(&mut runner, 40),
        WaitingFor::OptionalEffectChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the cast");

    assert!(matches!(
        advance_to_prompt(&mut runner, 40),
        WaitingFor::EffectZoneChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("decline the cast (empty selection)");

    assert!(
        runner.state().active_ability_continuation().is_none(),
        "no continuation lingers after a subless decline"
    );
    assert!(matches!(
        advance_to_prompt(&mut runner, 40),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(zone_of(&runner, bear), Zone::Hand, "nothing was cast");
}
