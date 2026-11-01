//! Issue #6461 — Ephemerate / Rebound: the next-upkeep "you may cast this
//! card from exile" offer resolves DURING the delayed trigger's resolution
//! (CR 608.2g), not as a lingering until-end-of-turn permission.
//!
//! https://github.com/phase-rs/phase/issues/6461
//!
//! Reported bug: a rebounded Ephemerate stays castable at any point during
//! the turn after the upkeep trigger resolves, instead of only as that
//! trigger resolves. CR 702.88a's "you may cast this card from exile" names
//! no duration, so per CR 608.2g the cast happens as the resolving trigger's
//! instruction executes — accept puts the spell on the stack immediately,
//! decline leaves it in exile. This mirrors Suspend's last-counter cast
//! (CR 702.62a), which already takes the `DuringResolution` path.

use crate::support::shared_card_db;
use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{CastingPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text from client/public/card-data.json.
const EPHEMERATE_ORACLE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";

/// Drive the turn structure until the rebound upkeep optional-cast prompt
/// surfaces, draining the stack and auto-passing combat steps as needed.
/// Mirrors `drive_until_suspend_cast_prompt` (issue #3271): the rebound
/// delayed trigger fires at upkeep, goes on the stack, and its optional body
/// prompts `OptionalEffectChoice` on resolution.
fn drive_until_rebound_cast_prompt(runner: &mut GameRunner) -> Result<(), String> {
    for _ in 0..300 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => return Ok(()),
            WaitingFor::Priority { .. } => {
                if !runner.state().stack.is_empty() {
                    runner
                        .act(GameAction::PassPriority)
                        .map_err(|e| format!("PassPriority (stack drain): {e:?}"))?;
                    continue;
                }
                if runner.state().phase == Phase::DeclareAttackers {
                    runner
                        .act(GameAction::DeclareAttackers {
                            attacks: vec![],
                            bands: vec![],
                        })
                        .map_err(|e| format!("DeclareAttackers: {e:?}"))?;
                } else if runner.state().phase == Phase::DeclareBlockers {
                    runner
                        .act(GameAction::DeclareBlockers {
                            assignments: vec![],
                        })
                        .map_err(|e| format!("DeclareBlockers: {e:?}"))?;
                } else {
                    runner
                        .act(GameAction::PassPriority)
                        .map_err(|e| format!("PassPriority: {e:?}"))?;
                    runner
                        .act(GameAction::PassPriority)
                        .map_err(|e| format!("PassPriority (second): {e:?}"))?;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .map_err(|e| format!("DeclareAttackers: {e:?}"))?;
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .map_err(|e| format!("DeclareBlockers: {e:?}"))?;
            }
            other => {
                return Err(format!("unexpected waiting_for: {other:?}"));
            }
        }
    }
    Err("timed out waiting for rebound upkeep cast prompt".to_string())
}

/// Fast-forward to the controller's next untap step so the armed rebound
/// delayed trigger fires on the drive through upkeep. Mirrors the issue
/// #3271 suspend harness.
fn fast_forward_to_next_untap(runner: &mut GameRunner) {
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
}

/// True iff `card` carries a LINGERING exile-cast permission — an
/// `ExileWithAltCost` grant with no resolution-cast cleanup marker, i.e. one
/// exercisable at a later priority window rather than as part of the
/// resolving trigger (CR 608.2g). A during-resolution cast's transient grant
/// always carries `resolution_cleanup: Some(_)`.
fn has_lingering_exile_cast_permission(runner: &GameRunner, card: ObjectId) -> bool {
    runner.state().objects[&card]
        .casting_permissions
        .iter()
        .any(|p| {
            matches!(
                p,
                CastingPermission::ExileWithAltCost {
                    resolution_cleanup: None,
                    ..
                }
            )
        })
}

/// CR 702.88a + CR 608.2g: accepting the upkeep rebound offer casts the
/// targetless sorcery DURING the trigger's resolution — it is on the stack
/// immediately. Terramorph is the real-card carrier (a sorcery with no
/// cast-time targets, so no intervening prompt can explain a delay).
/// Pre-fix this fails: the card stays in Exile and only a lingering
/// `UntilEndOfTurn` permission is stamped, making it castable at any later
/// priority window that turn.
#[test]
fn rebound_accept_casts_targetless_sorcery_during_upkeep_resolution() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let terramorph = scenario.add_real_card(P0, "Terramorph", Zone::Hand, db);
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.cast(terramorph).search_first_legal().resolve();

    // Reach-guards: the hand cast resolved, exiled (CR 702.88a displacing
    // CR 608.2n), and armed exactly one next-upkeep delayed trigger
    // (CR 603.7a).
    assert_eq!(
        runner.state().objects[&terramorph].zone,
        Zone::Exile,
        "a Rebound spell cast from hand must exile as it resolves"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "resolution must arm one next-upkeep Rebound delayed trigger"
    );

    fast_forward_to_next_untap(&mut runner);
    drive_until_rebound_cast_prompt(&mut runner).expect("must reach rebound cast prompt");

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept rebound upkeep recast");

    assert_eq!(
        runner.state().objects[&terramorph].zone,
        Zone::Stack,
        "CR 608.2g: accepting the upkeep offer must cast Terramorph during \
         the trigger's resolution — zone = {:?}, waiting_for = {:?}",
        runner.state().objects[&terramorph].zone,
        runner.state().waiting_for,
    );
    assert_eq!(runner.state().stack.len(), 1);
    assert!(
        !has_lingering_exile_cast_permission(&runner, terramorph),
        "CR 608.2g: the recast must not stamp a lingering permission \
         exercisable at a later priority window"
    );
}

/// CR 702.88a + CR 608.2g, the reported card: accepting Ephemerate's upkeep
/// offer opens its target selection DURING the trigger's resolution (not a
/// lingering grant followed by priority). Completing the cast blinks the
/// creature a second time; the rebound copy resolves to its owner's
/// graveyard (CR 608.2n — not re-armed, since it was not cast from hand).
/// Pre-fix this fails at the prompt shape: accept yields `Priority` plus a
/// lingering permission instead of `TargetSelection`.
#[test]
fn ephemerate_rebound_recast_resolves_only_during_upkeep_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The scenario builder parses abilities from Oracle text but does not infer
    // keywords from it, so Rebound rides explicitly (production card-data
    // carries it via the MTGJSON keyword hint).
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .with_keyword(Keyword::Rebound)
        .id();
    // Two legal blink targets so the rebound cast's target selection cannot
    // be auto-assigned away: the prompt must observably surface.
    let first = scenario.add_creature(P0, "First Target", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Target", 3, 3).id();

    let mut runner = scenario.build();

    let outcome = runner.cast(ephemerate).target_objects(&[first]).resolve();

    // Reach-guards: the hand cast blinked its target, exiled itself
    // (CR 702.88a), and armed the delayed trigger (CR 603.7a).
    outcome.assert_zone(&[first], Zone::Battlefield);
    outcome.assert_zone(&[ephemerate], Zone::Exile);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "resolution must arm one next-upkeep Rebound delayed trigger"
    );

    fast_forward_to_next_untap(&mut runner);
    drive_until_rebound_cast_prompt(&mut runner).expect("must reach rebound cast prompt");

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept rebound upkeep recast");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "CR 608.2g: accepting must initiate the rebound cast during the \
         trigger's resolution (target selection now), not grant a lingering \
         permission and pass priority — got {:?}",
        runner.state().waiting_for
    );

    // The during-resolution cast carries only a transient grant (marked
    // with resolution cleanup), never a lingering permission — checked
    // here, while the cast is still in flight, where a lingering grant
    // would still be observable.
    assert!(
        !has_lingering_exile_cast_permission(&runner, ephemerate),
        "accepting must not stamp a lingering exile-cast permission"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(second)),
        })
        .expect("choose rebound blink target");
    assert_eq!(
        runner.state().objects[&ephemerate].zone,
        Zone::Stack,
        "the rebound cast must be on the stack once its target is chosen"
    );
    assert!(
        !has_lingering_exile_cast_permission(&runner, ephemerate),
        "no lingering exile-cast permission may survive the rebound cast"
    );

    // Drain the stack: the rebound Ephemerate resolves its blink with no
    // further prompts.
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve rebound cast");
    }
    assert!(
        runner.state().stack.is_empty(),
        "the rebound cast must resolve without stalling"
    );
    assert_eq!(
        runner.state().objects[&second].zone,
        Zone::Battlefield,
        "the rebound Ephemerate must blink its target"
    );
    assert_eq!(
        runner.state().objects[&ephemerate].zone,
        Zone::Graveyard,
        "CR 702.88a + CR 608.2n: a rebound cast from exile is not re-armed \
         and resolves to its owner's graveyard"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 603.7b: the one-shot rebound trigger is consumed"
    );
}

/// CR 702.88a + CR 608.2g no-regression pin: declining the upkeep offer
/// leaves the card in exile with no permission, and it is not castable at
/// any later priority window that turn. Passes both before and after the
/// #6461 fix; it pins the "only at upkeep resolution" half of the contract.
#[test]
fn rebound_decline_leaves_card_uncastable_for_rest_of_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The scenario builder does not infer keywords from Oracle text (see above).
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .with_keyword(Keyword::Rebound)
        .id();
    let target = scenario.add_creature(P0, "Blink Target", 2, 2).id();

    let mut runner = scenario.build();

    let outcome = runner.cast(ephemerate).target_objects(&[target]).resolve();

    // Reach-guards: hand cast resolved, exiled, and armed the trigger.
    outcome.assert_zone(&[target], Zone::Battlefield);
    outcome.assert_zone(&[ephemerate], Zone::Exile);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    fast_forward_to_next_untap(&mut runner);
    drive_until_rebound_cast_prompt(&mut runner).expect("must reach rebound cast prompt");

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline rebound upkeep recast");

    assert_eq!(
        runner.state().objects[&ephemerate].zone,
        Zone::Exile,
        "a declined rebound stays in exile"
    );
    assert!(
        runner.state().objects[&ephemerate]
            .casting_permissions
            .is_empty(),
        "declining must grant no casting permission at all"
    );
    assert!(
        !legal_actions(runner.state()).iter().any(|action| {
            matches!(
                action,
                GameAction::CastSpell { object_id, .. }
                | GameAction::CastSpellForFree { object_id, .. }
                if *object_id == ephemerate
            )
        }),
        "a declined rebound must offer no cast action at later priority windows"
    );
}
