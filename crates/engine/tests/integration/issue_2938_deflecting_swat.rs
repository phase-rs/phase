//! Issue #2938 — Deflecting Swat must offer a retarget step for the targeted
//! spell or ability (CR 115.7d), not silently resolve as a no-op `TargetOnly`.
//!
//! Root cause: the effect-chain chunk loop must retain the full "you may choose
//! new targets for ..." surface form so the retarget parser can distinguish true
//! retarget effects from copy-retarget riders, then carry that retained "you
//! may" modal onto the parsed `ChangeTargets` ability.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastingVariant, RetargetScope, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DEFLECTING_SWAT_ORACLE: &str =
    "If you control a commander, you may cast this spell without paying its mana cost.\n\
     You may choose new targets for target spell or ability.";

const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn pass_priority_until<F>(runner: &mut engine::game::scenario::GameRunner, mut done: F)
where
    F: FnMut(&WaitingFor) -> bool,
{
    for _ in 0..32 {
        if done(&runner.state().waiting_for) {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority must succeed while resolving Deflecting Swat");
            }
            other => panic!("unexpected wait state while driving resolution: {other:?}"),
        }
    }
    panic!("priority loop exhausted before reaching expected wait state");
}

#[test]
fn deflecting_swat_parses_change_targets_not_target_only() {
    let parsed = parse_oracle_text(
        DEFLECTING_SWAT_ORACLE,
        "Deflecting Swat",
        &[],
        &["Instant".to_string()],
        &[],
    );

    assert_eq!(parsed.abilities.len(), 1);
    assert!(matches!(
        parsed.abilities[0].effect.as_ref(),
        Effect::ChangeTargets {
            scope: RetargetScope::All,
            forced_to: None,
            ..
        }
    ));
    assert!(
        parsed.abilities[0].optional,
        "CR 608.2d: 'you may choose new targets' must be optional at resolution"
    );

    let Effect::ChangeTargets { target, .. } = parsed.abilities[0].effect.as_ref() else {
        unreachable!();
    };
    let TargetFilter::Or { filters } = target else {
        panic!("expected Or(StackSpell, StackAbility), got {target:?}");
    };
    assert!(filters.contains(&TargetFilter::StackSpell));
}

#[test]
fn deflecting_swat_retargets_opponent_spell_on_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Bear", 2, 2).id();
    let redirect_host = scenario.add_creature(P0, "Goblin", 1, 1).id();
    let swat = scenario
        .add_spell_to_hand_from_oracle(P0, "Deflecting Swat", true, DEFLECTING_SWAT_ORACLE)
        .id();

    let mut runner = scenario.build();
    add_mana(
        &mut runner,
        P0,
        &[ManaType::Red, ManaType::Colorless, ManaType::Colorless],
    );

    // P1's Lightning Bolt already on the stack targeting the bear.
    let bolt_parsed = parse_oracle_text(
        LIGHTNING_BOLT_ORACLE,
        "Lightning Bolt",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let bolt_ability = ResolvedAbility::new(
        bolt_parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Object(victim)],
        engine::types::identifiers::ObjectId(0),
        P1,
    );
    let bolt_id = create_object(
        runner.state_mut(),
        CardId(77),
        P1,
        "Lightning Bolt".to_string(),
        Zone::Stack,
    );
    {
        let bolt_obj = runner.state_mut().objects.get_mut(&bolt_id).unwrap();
        bolt_obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: bolt_id,
        source_id: bolt_id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: Some(bolt_ability),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    runner.cast(swat).target_objects(&[bolt_id]).commit();

    pass_priority_until(&mut runner, |waiting| {
        matches!(waiting, WaitingFor::OptionalEffectChoice { .. })
    });

    let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for else {
        unreachable!();
    };
    assert_eq!(*player, P0);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept Deflecting Swat retarget choice");

    pass_priority_until(&mut runner, |waiting| {
        matches!(waiting, WaitingFor::RetargetChoice { .. })
    });

    let WaitingFor::RetargetChoice {
        player,
        stack_entry_index,
        scope,
        current_targets,
        legal_new_targets,
    } = &runner.state().waiting_for
    else {
        unreachable!();
    };
    assert_eq!(*player, P0);
    assert!(matches!(scope, RetargetScope::All));
    assert_eq!(current_targets, &vec![TargetRef::Object(victim)]);
    assert!(
        legal_new_targets.contains(&TargetRef::Object(redirect_host)),
        "legal retarget set must include alternative creatures, got {legal_new_targets:?}"
    );

    let stack_index = *stack_entry_index;
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![TargetRef::Object(redirect_host)],
        })
        .expect("retarget submission must succeed");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    let bolt_targets = runner
        .state()
        .stack
        .get(stack_index)
        .and_then(|e| e.ability())
        .map(|a| a.targets.clone())
        .expect("bolt still on stack");
    assert_eq!(bolt_targets, vec![TargetRef::Object(redirect_host)]);
}
