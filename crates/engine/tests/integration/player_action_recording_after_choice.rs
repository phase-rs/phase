//! `player_actions_this_turn` must record a choice-completed player action
//! exactly once, from whichever site actually publishes its
//! `PlayerPerformedAction` event.
//!
//! Each row's assertion is an exact `== 1`, which doubles as the guard against
//! a second recording site (e.g. accidentally also recording inside
//! `resolve_chain_body`'s window for an event that is ALSO published there).

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

/// Opt's Oracle text ("Scry 1.\nDraw a card.") cast and resolved
/// through the default driver, which answers `ScryChoice` by keeping the
/// looked-at card on top (CR 701.22a). The Scry must be recorded exactly
/// once; the `CardDrawn` count is a reach-guard that the whole spell (not
/// just its first line) actually resolved.
#[test]
fn opt_records_scry_once_after_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    let opt = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, "Scry 1.\nDraw a card.")
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(opt).resolve();

    let scry_count = outcome
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(player, action)| *player == P0 && *action == PlayerActionKind::Scry)
        .count();
    assert_eq!(
        scry_count, 1,
        "Opt's scry must be recorded in player_actions_this_turn exactly once"
    );
    let draws = outcome
        .events()
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P0))
        .count();
    assert_eq!(draws, 1, "reach-guard: Opt's Draw a card must also resolve");
}

/// A bare "Surveil 1." instant, driven the same way. Guards against a
/// second recording site for Surveil specifically — `surveil.rs::resolve`
/// emits its event BEFORE setting `WaitingFor::SurveilChoice` (unlike Scry),
/// so `resolve_chain_body`'s own window already sees it; this pins that no
/// double-count was introduced anywhere in the class fix.
#[test]
fn surveil_records_once_via_the_pre_choice_emission_site() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    let surveil_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Surveil Spell", true, "Surveil 1.")
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(surveil_spell).resolve();

    let surveil_count = outcome
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(player, action)| *player == P0 && *action == PlayerActionKind::Surveil)
        .count();
    assert_eq!(
        surveil_count, 1,
        "Surveil must be recorded in player_actions_this_turn exactly once"
    );
}

/// Contentious Plan's Oracle text ("Proliferate.\nDraw a
/// card."), with a +1/+1-counter creature on the battlefield so the
/// `ProliferateChoice` prompt actually opens (an empty-board proliferate
/// publishes from `emit_empty_proliferate_action`, a different site, not
/// under test here). `ProliferateChoice` is not one of the shared driver's
/// auto-answered prompts, so this drives `GameAction`s directly.
#[test]
fn contentious_plan_records_proliferate_once_after_choice() {
    use engine::types::ability::TargetRef;
    use engine::types::counter::CounterType;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    let creature = scenario.add_creature(P0, "Countered Creature", 2, 2).id();
    let plan = scenario
        .add_spell_to_hand_from_oracle(P0, "Contentious Plan", false, "Proliferate.\nDraw a card.")
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    runner
        .act(GameAction::CastSpell {
            object_id: plan,
            card_id: runner.state().objects[&plan].card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Contentious Plan cast accepted");

    let mut answered_proliferate_choice = false;
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ProliferateChoice { .. } => {
                answered_proliferate_choice = true;
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(creature)],
                    })
                    .expect("proliferate target choice accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected Contentious Plan prompt: {other:?}"),
        }
    }
    assert!(
        answered_proliferate_choice,
        "Contentious Plan must reach WaitingFor::ProliferateChoice during resolution"
    );

    let proliferate_count = runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(player, action)| *player == P0 && *action == PlayerActionKind::Proliferate)
        .count();
    assert_eq!(
        proliferate_count, 1,
        "Proliferate must be recorded in player_actions_this_turn exactly once"
    );
}

/// A mandatory "As an additional cost to cast this spell, collect
/// evidence 3." spell, with graveyard fuel of mana value 3. Collect evidence
/// as a MANDATORY additional cost detours straight to
/// `WaitingFor::CollectEvidenceChoice` — no `OptionalCostChoice` step, unlike
/// the "you may collect evidence" shape.
#[test]
fn mandatory_collect_evidence_records_once_after_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    scenario
        .add_creature_to_graveyard(P0, "Evidence Fuel", 2, 2)
        .with_mana_cost(ManaCost::generic(3));
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Test Mandatory Evidence Spell",
            false,
            "As an additional cost to cast this spell, collect evidence 3.\nDraw a card.",
        )
        .id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("mandatory collect-evidence cast accepted");

    let mut answered_evidence_choice = false;
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CollectEvidenceChoice { cards, .. } => {
                answered_evidence_choice = true;
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("collect-evidence selection accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected mandatory-evidence prompt: {other:?}"),
        }
    }
    assert!(
        answered_evidence_choice,
        "the mandatory collect-evidence cast must reach WaitingFor::CollectEvidenceChoice"
    );

    let evidence_count = runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(player, action)| *player == P0 && *action == PlayerActionKind::CollectEvidence)
        .count();
    assert_eq!(
        evidence_count, 1,
        "CollectEvidence must be recorded in player_actions_this_turn exactly once"
    );
}
