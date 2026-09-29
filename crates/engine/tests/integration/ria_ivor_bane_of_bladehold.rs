//! Ria Ivor, Bane of Bladehold — a beginning-of-combat TRIGGER that installs a
//! one-shot, source-scoped, player-recipient combat-damage prevention shield,
//! driven through the real trigger → target → combat pipeline.
//!
//! Verbatim Oracle text under test:
//!   "Battle cry (Whenever this creature attacks, each other attacking creature
//!    gets +1/+0 until end of turn.)
//!    At the beginning of combat on your turn, the next time target creature
//!    would deal combat damage to one or more players this combat, prevent that
//!    damage. If damage is prevented this way, create that many 1/1 colorless
//!    Phyrexian Mite artifact creature tokens with toxic 1 and "This token can't
//!    block.""
//!
//! Defects this guards against (#9121):
//!   1. Inside a trigger body the declared "target creature" subject was
//!      declined, so the prevention sentence stayed an `Unimplemented` gap and
//!      the "prevented this way" rider fired as an independent sibling.
//!   2. The recipient clause "to one or more players" was dropped, so a shield
//!      would have prevented the chosen creature's damage to a blocker too.
//!   3. The recipient scope would have surfaced as a spurious player target
//!      slot beside the one declared target (the damage source).
//!
//! CR 603.3d: the trigger's target is chosen as it is put on the stack.
//! CR 609.7a + CR 609.7b: the chosen creature is the captured damage source,
//! rechecked live at damage time. CR 615.8: only that source's next damage
//! instance is prevented. CR 615.5: "prevented this way" reads the prevented
//! amount. CR 511.2: "this combat" effects end at end of combat. CR 510.2:
//! combat damage is dealt simultaneously. CR 120.1: the player recipient
//! domain. CR 115.10a: the recipient scope is not a target. CR 113.7a +
//! CR 611.2a: the shield is independent of Ria once created. CR 111.2: the
//! trigger's controller creates the tokens. CR 702.164a: Toxic.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CombatDamageScope, DamageTargetFilter, DamageTargetPlayerScope, Effect,
    ShieldKind, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CombatDamageAssignmentMode, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const RIA_NAME: &str = "Ria Ivor, Bane of Bladehold";
const RIA_ORACLE: &str = "Battle cry (Whenever this creature attacks, each other attacking creature gets +1/+0 until end of turn.)\nAt the beginning of combat on your turn, the next time target creature would deal combat damage to one or more players this combat, prevent that damage. If damage is prevented this way, create that many 1/1 colorless Phyrexian Mite artifact creature tokens with toxic 1 and \"This token can't block.\"";

fn add_ria(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, RIA_NAME, 3, 4)
        .from_oracle_text_with_keywords(&["Battle cry"], RIA_ORACLE)
        .id()
}

/// Pass priority out of precombat main until Ria's beginning-of-combat
/// trigger asks for its target (CR 603.3d), and return the offered slots.
fn drive_to_trigger_target_selection(
    runner: &mut GameRunner,
    ria: ObjectId,
) -> Vec<TargetSelectionSlot> {
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                source_id,
                ..
            } => {
                assert_eq!(
                    source_id,
                    Some(ria),
                    "the target prompt must belong to Ria's beginning-of-combat trigger"
                );
                return target_slots;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass toward beginning of combat");
            }
            other => panic!("unexpected WaitingFor before Ria's target prompt: {other:?}"),
        }
    }
    panic!(
        "Ria's trigger never asked for a target; phase {:?}, waiting {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
}

/// Choose `chosen` for Ria's trigger and resolve it.
///
/// CR 115.10a: the trigger declares exactly one target (the damage source);
/// the "to one or more players" recipient scope must not surface a second,
/// player slot.
fn target_and_resolve(
    runner: &mut GameRunner,
    ria: ObjectId,
    chosen: ObjectId,
) -> Vec<TargetSelectionSlot> {
    let slots = drive_to_trigger_target_selection(runner, ria);
    assert_eq!(
        slots.len(),
        1,
        "CR 115.10a: the trigger declares exactly one target, got {slots:?}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        })
        .expect("the chosen creature must be a legal target");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "Ria's trigger must have resolved"
    );
    slots
}

/// The pending one-shot prevention shields installed by Ria's trigger.
fn one_shot_shields(runner: &GameRunner) -> Vec<&engine::types::ability::ReplacementDefinition> {
    runner
        .state()
        .pending_damage_replacements
        .iter()
        .filter(|r| r.shield_kind == ShieldKind::PreventionOneShot && !r.is_consumed)
        .collect()
}

/// Declare `attacks` and `blocks`, stopping at the declare-blockers priority
/// window — after blockers are declared and before combat damage is dealt.
fn declare_combat(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
    blocks: &[(ObjectId, ObjectId)],
) {
    let mut attacked = false;
    let mut blocked = false;
    for _ in 0..40 {
        let phase = runner.state().phase;
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if phase == Phase::DeclareBlockers => return,
            WaitingFor::Priority { .. } => {
                assert!(
                    !matches!(
                        phase,
                        Phase::CombatDamage | Phase::EndCombat | Phase::PostCombatMain
                    ),
                    "combat advanced past declare blockers before the checkpoint"
                );
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass toward combat");
            }
            WaitingFor::DeclareAttackers { .. } if !attacked => {
                attacked = true;
                runner
                    .declare_attackers(attacks)
                    .expect("declaring the attackers must succeed");
            }
            WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                runner
                    .declare_blockers(blocks)
                    .expect("declaring the blockers must succeed");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected WaitingFor while declaring combat: {other:?}"),
        }
    }
    panic!("never reached the declare-blockers priority window");
}

/// Deal combat damage and advance to postcombat main. A trampler's damage is
/// assigned greedily: lethal to each blocker, the rest to the player
/// (CR 702.19b).
fn finish_combat(runner: &mut GameRunner) {
    for _ in 0..40 {
        let phase = runner.state().phase;
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if phase == Phase::PostCombatMain => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass through combat");
            }
            WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                trample,
                ..
            } => {
                let mut remaining = total_damage;
                let mut assignments = Vec::new();
                for slot in &blockers {
                    let assign = remaining.min(slot.lethal_minimum);
                    assignments.push((slot.blocker_id, assign));
                    remaining -= assign;
                }
                assert!(trample.is_some(), "only the trample case prompts here");
                runner
                    .act(GameAction::AssignCombatDamage {
                        mode: CombatDamageAssignmentMode::Normal,
                        assignments,
                        trample_damage: remaining,
                        controller_damage: 0,
                    })
                    .expect("the greedy trample assignment must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected WaitingFor during combat damage: {other:?}"),
        }
    }
    panic!("never reached postcombat main");
}

/// Phyrexian Mite tokens controlled by `player`.
fn mites(runner: &GameRunner, player: engine::types::player::PlayerId) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj.controller == player
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Mite"))
            })
        })
        .copied()
        .collect()
}

fn source_scoped(id: ObjectId) -> Option<TargetFilter> {
    Some(TargetFilter::And {
        filters: vec![
            TargetFilter::SpecificObject { id },
            TargetFilter::Typed(TypedFilter::creature()),
        ],
    })
}

/// T1 — CR 603.3d + CR 115.10a + CR 609.7a + CR 615.5 + CR 615.8: the trigger
/// offers exactly one creature-only slot; the resolved shield is source-scoped
/// to the chosen Bear, combat-only and player-recipient; the Bear's unblocked
/// combat damage to P1 is prevented and exactly that many (3) Mites are
/// created for Ria's controller. Ria does not attack, so battle cry never pumps
/// the Bear (power stays 3).
#[test]
fn ria_prevents_targeted_creatures_player_damage_and_creates_that_many_mites() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ria = add_ria(&mut scenario);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    // (a) Exactly one target slot: creatures only, no player (U2).
    let slots = target_and_resolve(&mut runner, ria, bear);
    let legal = &slots[0].legal_targets;
    assert!(
        legal.contains(&TargetRef::Object(ria)) && legal.contains(&TargetRef::Object(bear)),
        "both creatures are legal targets: {legal:?}"
    );
    assert!(
        legal.iter().all(|t| matches!(
            t,
            TargetRef::Object(id) if runner.state().objects[id]
                .card_types
                .core_types
                .contains(&CoreType::Creature)
        )),
        "CR 115.10a: the recipient scope is not a target — no player may be offered: {legal:?}"
    );

    // (b) The installed shield.
    let shields = one_shot_shields(&runner);
    assert_eq!(shields.len(), 1, "exactly one one-shot prevention shield");
    let shield = shields[0];
    assert_eq!(
        shield.damage_source_filter,
        source_scoped(bear),
        "CR 609.7a: the chosen Bear is the captured damage source"
    );
    assert_eq!(shield.combat_scope, Some(CombatDamageScope::CombatOnly));
    assert_eq!(
        shield.damage_target_filter,
        Some(DamageTargetFilter::Player {
            player: DamageTargetPlayerScope::Any
        }),
        "CR 120.1: 'to one or more players' scopes the shield to player recipients"
    );

    declare_combat(&mut runner, &[(bear, AttackTarget::Player(P1))], &[]);
    assert_eq!(
        runner.state().objects[&bear].power,
        Some(3),
        "Ria did not attack, so battle cry must not pump the Bear"
    );
    finish_combat(&mut runner);

    // (c) The Bear's combat damage to P1 was prevented.
    assert_eq!(
        runner.life(P1),
        p1_life,
        "the Bear's 3 combat damage is prevented"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .map(|p| p.poison_counters),
        Some(0),
        "no poison: the Bear has no toxic and dealt no damage"
    );

    // (d) Exactly that many Mites, with the printed characteristics.
    let created = mites(&runner, P0);
    assert_eq!(
        created.len(),
        3,
        "CR 615.5: 'that many' = the 3 prevented damage"
    );
    for id in &created {
        let obj = &runner.state().objects[id];
        assert_eq!(
            (obj.power, obj.toughness),
            (Some(1), Some(1)),
            "Mite is 1/1"
        );
        assert!(obj.color.is_empty(), "Mite is colorless: {:?}", obj.color);
        assert!(
            obj.card_types.core_types.contains(&CoreType::Artifact)
                && obj.card_types.core_types.contains(&CoreType::Creature),
            "Mite is an artifact creature: {:?}",
            obj.card_types.core_types
        );
        assert!(
            obj.card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Phyrexian")),
            "Mite is Phyrexian: {:?}",
            obj.card_types.subtypes
        );
        assert!(
            obj.keywords.contains(&Keyword::Toxic(1)),
            "CR 702.164a: Mite has toxic 1: {:?}",
            obj.keywords
        );
        assert!(
            obj.static_definitions
                .iter_unchecked()
                .any(|d| d.mode == StaticMode::CantBlock),
            "Mite can't block: {:?}",
            obj.static_definitions
        );
    }
    assert!(mites(&runner, P1).is_empty(), "CR 111.2: no Mites for P1");
}

/// T2 — CR 120.1 + CR 511.2: the shield is player-scoped, so the targeted
/// Bear's combat damage to a BLOCKER is dealt normally (the 2/2 blocker is
/// destroyed), no Mites are created, P1 takes no damage, and the unused
/// shield is gone after combat.
#[test]
fn ria_shield_does_not_prevent_damage_to_a_blocker_and_ends_with_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ria = add_ria(&mut scenario);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    target_and_resolve(&mut runner, ria, bear);
    declare_combat(
        &mut runner,
        &[(bear, AttackTarget::Player(P1))],
        &[(blocker, bear)],
    );
    // Reach guard: the shield exists, unused, immediately before combat damage.
    assert_eq!(
        one_shot_shields(&runner).len(),
        1,
        "shield present before damage"
    );
    assert_eq!(runner.state().objects[&blocker].damage_marked, 0);
    finish_combat(&mut runner);

    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Graveyard,
        "the Bear's 3 combat damage to the blocker is NOT prevented — the 2/2 dies"
    );
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        2,
        "reach guard: combat damage was exchanged"
    );
    assert!(
        mites(&runner, P0).is_empty(),
        "nothing was prevented, so no Mites"
    );
    assert_eq!(runner.life(P1), p1_life);
    assert!(
        !runner
            .state()
            .pending_damage_replacements
            .iter()
            .any(|r| r.shield_kind == ShieldKind::PreventionOneShot),
        "CR 511.2: the 'this combat' shield ends with combat"
    );
}

/// T3 — CR 609.7a + CR 510.2: with two unblocked attackers and only the Bear
/// targeted, the untargeted 2/2's damage is dealt and only the Bear's 3 is
/// prevented (Ria does not attack, so the Bear's power stays 3).
#[test]
fn ria_shield_binds_only_the_targeted_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ria = add_ria(&mut scenario);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let other = scenario.add_creature(P0, "Other", 2, 2).id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    target_and_resolve(&mut runner, ria, bear);
    declare_combat(
        &mut runner,
        &[
            (bear, AttackTarget::Player(P1)),
            (other, AttackTarget::Player(P1)),
        ],
        &[],
    );
    finish_combat(&mut runner);

    assert_eq!(
        runner.life(P1),
        p1_life - 2,
        "only the untargeted attacker's 2 damage is dealt"
    );
    assert_eq!(mites(&runner, P0).len(), 3, "Mites == the Bear's power (3)");
}

/// T4 — CR 702.19b + CR 615.5: a 4/4 trampling Bear blocked by a 1/1 assigns
/// 1 to the blocker (dealt) and 3 to P1 (prevented); "that many" is the 3
/// prevented, not the Bear's full 4.
#[test]
fn ria_prevents_trample_excess_and_creates_mites_for_the_prevented_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ria = add_ria(&mut scenario);
    let bear = scenario.add_creature(P0, "Bear", 4, 4).trample().id();
    let chump = scenario.add_creature(P1, "Chump", 1, 1).id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    target_and_resolve(&mut runner, ria, bear);
    declare_combat(
        &mut runner,
        &[(bear, AttackTarget::Player(P1))],
        &[(chump, bear)],
    );
    finish_combat(&mut runner);

    assert_eq!(
        runner.state().objects[&chump].zone,
        Zone::Graveyard,
        "the 1 damage to the blocker is dealt"
    );
    assert_eq!(
        runner.life(P1),
        p1_life,
        "the 3 trample excess is prevented"
    );
    assert_eq!(
        mites(&runner, P0).len(),
        3,
        "Mites == the 3 damage prevented"
    );
}

/// T5 — CR 113.7a + CR 611.2a: once the trigger resolves, the shield exists
/// independently of Ria; Ria leaving the battlefield before damage does not
/// remove it, and the tokens are still created for Ria's controller (CR 111.2).
#[test]
fn ria_shield_survives_ria_leaving_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ria = add_ria(&mut scenario);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    target_and_resolve(&mut runner, ria, bear);
    engine::game::zones::move_to_zone(runner.state_mut(), ria, Zone::Exile, &mut Vec::new());
    assert!(
        !runner.state().battlefield.contains(&ria),
        "reach guard: Ria is gone before damage"
    );

    declare_combat(&mut runner, &[(bear, AttackTarget::Player(P1))], &[]);
    finish_combat(&mut runner);

    assert_eq!(
        runner.life(P1),
        p1_life,
        "the shield still prevents the Bear's damage"
    );
    assert_eq!(
        mites(&runner, P0).len(),
        3,
        "the rider still creates 3 Mites for P0"
    );
}

fn chain_has_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(&*def.effect, Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
}

/// T6 — full-card parse (local stand-in for coverage gap_count 0): battle cry
/// is recognized (CR 702.91) and no ability, trigger or static carries a gap.
/// Guards that Ria's own tail ("to one or more players") is on the success
/// side of the fail-closed arm.
#[test]
fn ria_full_card_parses_without_gaps() {
    let parsed = parse_oracle_text(
        RIA_ORACLE,
        RIA_NAME,
        &["Battle cry".to_string()],
        &["Creature".to_string()],
        &["Phyrexian".to_string(), "Knight".to_string()],
    );
    assert!(
        parsed.extracted_keywords.contains(&Keyword::Battlecry),
        "battle cry must be recognized: {:?}",
        parsed.extracted_keywords
    );
    let combat_trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase)
        .expect("reach guard: the beginning-of-combat trigger must exist");
    assert!(
        matches!(
            combat_trigger.execute.as_deref().map(|b| &*b.effect),
            Some(Effect::PreventDamage { .. })
        ),
        "reach guard: the trigger body is the prevention shield"
    );
    for trigger in &parsed.triggers {
        assert!(
            !matches!(trigger.mode, TriggerMode::Unknown(_)),
            "unrecognized trigger: {trigger:?}"
        );
        if let Some(body) = trigger.execute.as_deref() {
            assert!(!chain_has_unimplemented(body), "trigger gap: {body:?}");
        }
    }
    for ability in &parsed.abilities {
        assert!(
            !chain_has_unimplemented(ability),
            "ability gap: {ability:?}"
        );
    }
    for st in &parsed.statics {
        assert!(
            !matches!(st.mode, StaticMode::Other(_)),
            "static gap: {st:?}"
        );
    }
}
