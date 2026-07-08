//! Stensian Sanguinist (SOC, prepare mechanic) — runtime regression.
//!
//! Oracle:
//!   "Whenever you attack, target creature gains deathtouch until end of turn.
//!    Whenever that creature deals combat damage to a player this combat, this
//!    creature becomes prepared. (While it's prepared, you may cast a copy of
//!    its spell. Doing so unprepares it.)"
//!
//! Before the fix, the second sentence (window `" this combat, "`, not
//! `" this turn, "`) failed the `try_parse_whenever_this_turn` split and fell
//! through to `Effect::Unimplemented`, so the "becomes prepared" delayed trigger
//! never installed and the creature never became prepared on combat damage.
//!
//! This test drives the full pipeline (`add_real_card` + rehydrate, so the
//! hydrated Prepare back-face is present and the fix is not masked by
//! re-parsing): attack → resolve the `YouAttack` trigger choosing the attacker
//! as "target creature" → deal combat damage to the opponent → assert the
//! "this combat" delayed trigger fires and Stensian becomes prepared, and that
//! `CastPreparedCopy` is then a legal action. CR 603.7b/603.7c + CR 722.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{DelayedTriggerCondition, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;
use crate::support::shared_card_db as load_db;

/// CR 603.7b/603.7c + CR 510 + CR 722: the "this combat" delayed trigger must
/// install on attack and make Stensian prepared once the targeted creature deals
/// combat damage to a player.
#[test]
fn stensian_sanguinist_becomes_prepared_on_combat_damage() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stensian = scenario.add_real_card(P0, "Stensian Sanguinist", Zone::Battlefield, db);
    let attacker = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Discriminating negative: the parsed Stensian ability tree must contain no
    // Effect::Unimplemented. Before the fix the "becomes prepared" clause lowered
    // to Unimplemented and this assertion fails.
    let stensian_obj = runner
        .state()
        .objects
        .get(&stensian)
        .expect("Stensian must hydrate");
    let parsed_json =
        serde_json::to_string(&stensian_obj.trigger_definitions).expect("serialize triggers");
    assert!(
        !parsed_json.contains("\"Unimplemented\""),
        "Stensian's parsed ability tree must contain no Effect::Unimplemented"
    );

    // Move to combat and declare the attack — fires the YouAttack trigger.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Resolve the YouAttack trigger, choosing the ATTACKER (not Stensian itself)
    // as "target creature" so it is the creature that deals combat damage.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "YouAttack trigger must prompt for a target creature, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(attacker)),
        })
        .expect("choosing the attacker as the deathtouch target should succeed");
    runner.advance_until_stack_empty();

    // The attacker gained deathtouch until end of turn.
    assert!(
        runner
            .state()
            .objects
            .get(&attacker)
            .expect("attacker exists")
            .has_keyword(&Keyword::Deathtouch),
        "attacker must gain deathtouch from the YouAttack trigger"
    );

    // The "this combat" delayed trigger is installed. Its CONDITION watches the
    // attacker (the "that creature" damage source, ParentTarget → SpecificObject
    // at resolution); its EFFECT prepares Stensian itself ("this creature
    // becomes prepared" → SelfRef, the source — NOT the targeted attacker).
    let installed = runner.state().delayed_triggers.iter().any(|dt| {
        let DelayedTriggerCondition::WheneverEvent { trigger } = &dt.condition else {
            return false;
        };
        trigger.valid_source == Some(TargetFilter::SpecificObject { id: attacker })
            && matches!(
                dt.ability.effect,
                Effect::BecomePrepared {
                    target: TargetFilter::SelfRef
                }
            )
            && dt.source_id == stensian
    });
    assert!(
        installed,
        "attack must install a 'this combat' WheneverEvent delayed trigger whose damage source is \
         the attacker and whose effect prepares Stensian (SelfRef); got {:?}",
        runner.state().delayed_triggers
    );

    // Drive the combat-damage step. The attacker is unblocked and deals combat
    // damage to P1, firing the delayed trigger. Collect events to confirm the
    // BecamePrepared event was emitted.
    let mut became_prepared = false;
    for _ in 0..60 {
        let prepared_now = runner
            .state()
            .objects
            .get(&stensian)
            .and_then(|o| o.prepared.as_ref())
            .is_some();
        if runner.state().phase == Phase::PostCombatMain && prepared_now {
            break;
        }
        let result = match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("empty blocks"),
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority")
            }
            other => panic!("unexpected waiting state during combat: {other:?}"),
        };
        if result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::BecamePrepared { object_id } if *object_id == stensian))
        {
            became_prepared = true;
        }
    }

    // Durable observable: Stensian is prepared.
    assert!(
        runner
            .state()
            .objects
            .get(&stensian)
            .and_then(|o| o.prepared.as_ref())
            .is_some(),
        "Stensian must be prepared after the attacker deals combat damage to a player"
    );
    assert!(
        became_prepared,
        "a GameEvent::BecamePrepared for Stensian must have been emitted"
    );

    // CR 722.3c: with Stensian prepared, casting the prepared copy is a legal
    // special action while its controller holds priority at sorcery speed and
    // can pay the prepare spell's cost ({X}{B}{B}, X=0 → {B}{B}). We are at
    // PostCombatMain with P0 priority (their own main phase). Fund two black
    // mana so the castability probe (mana payable + sorcery timing) succeeds,
    // then assert the action is offered.
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    {
        let pool = &mut runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap()
            .mana_pool
            .mana;
        pool.push(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Black,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
        pool.push(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Black,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
    let offers_cast = engine::ai_support::legal_actions(runner.state())
        .iter()
        .any(|a| matches!(a, GameAction::CastPreparedCopy { source } if *source == stensian));
    assert!(
        offers_cast,
        "CastPreparedCopy for Stensian must be a legal action once it is prepared and castable; \
         legal_actions = {:?}",
        engine::ai_support::legal_actions(runner.state())
    );
}
