//! Viewer redaction of announcement-bound usage keys (CR 602.2a +
//! CR 602.5c + CR 400.2).
//!
//! The announced usage key and the activation record are engine authority:
//! they ride prompt-held carriers, paused mana abilities, the stack, and the
//! announcement journals, but no viewer projection may carry them. Three
//! carriers are covered: the active stack ability, a parked in-flight
//! activation (target prompt), and a paused mana ability (color-choice
//! prompt) — plus the per-priority guard and both journals. Trusted
//! save/restore round-trips the same key (retained, only redacted).
//!
//! Verbatim Oracle texts (Scryfall): Quirion Ranger — "Return a Forest you
//! control to its owner's hand: Untap target creature. Activate only once
//! each turn." Brigid, Doun's Mind — "{T}: Add X {G} or X {W}, where X is
//! the number of other creatures you control."
//!
//! Revert detectors: restoring any cleared field makes the corresponding
//! `is_none` assertion fail; restoring a journal/pending clear makes the
//! emptiness assertion fail.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, ManaChoice, ManaChoiceContext, ManaChoicePrompt, PendingCast, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const BRIGID_TEXT: &str =
    "{T}: Add X {G} or X {W}, where X is the number of other creatures you control.";

/// A viewer with no seat at the table.
fn unseated() -> PlayerId {
    PlayerId(9)
}

/// Ranger board: one Untapper, one tappable target, one bounce Forest.
fn ranger_board(db: &engine::database::CardDatabase) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ranger = scenario.add_real_card(P0, "Quirion Ranger", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&bear)
        .expect("tappable")
        .tapped = true;
    (runner, ranger, bear, forest)
}

fn ranger_index(runner: &GameRunner, ranger: ObjectId) -> usize {
    runner.state().objects[&ranger]
        .abilities
        .iter()
        .position(|ability| {
            ability.kind == engine::types::ability::AbilityKind::Activated
                && ability
                    .description
                    .as_deref()
                    .is_some_and(|text| text.contains("Untap"))
        })
        .expect("the Ranger carries its printed Untap")
}

/// Announce the Ranger's activation and stop at its target prompt, with the
/// announcement-bound key and record riding the parked carrier.
fn announce_to_target_prompt(runner: &mut GameRunner, ranger: ObjectId) -> PendingCast {
    runner
        .act(GameAction::ActivateAbility {
            source_id: ranger,
            ability_index: ranger_index(runner, ranger),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announcement must be accepted");
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "expected the target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        pending_cast.ability.announced_usage_key.is_some(),
        "reach-guard: the announcement bound the key before targets"
    );
    assert!(
        pending_cast.ability.activation_record.is_some(),
        "reach-guard: the activation record rides the prompt"
    );
    pending_cast.as_ref().clone()
}

/// CR 400.2 + CR 602.2a + CR 602.5c: a parked in-flight activation and the
/// resolved stack entry project no announced key and no activation record,
/// and neither do the announcement journals or the per-priority guard — for
/// the opponent and for an unseated viewer alike.
#[test]
fn activation_prompt_and_stack_project_no_announced_key() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, ranger, bear, forest) = ranger_board(db);
    let viewers = [P1, unseated()];

    // Stage 1: the target prompt holds the parked carrier.
    let parked = announce_to_target_prompt(&mut runner, ranger);
    let parked_key = parked
        .ability
        .announced_usage_key
        .clone()
        .expect("the parked carrier is keyed");
    for viewer in viewers {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        let WaitingFor::TargetSelection { pending_cast, .. } = &filtered.waiting_for else {
            panic!(
                "{viewer:?} must see the same target prompt, got {:?}",
                filtered.waiting_for
            );
        };
        assert!(
            pending_cast.ability.announced_usage_key.is_none(),
            "{viewer:?} sees a parked announced key"
        );
        assert!(
            pending_cast.ability.activation_record.is_none(),
            "{viewer:?} sees a parked activation record"
        );
    }

    // Stage 2: answer target + bounce cost — the ability is placed with its
    // key, the journal counts it, and the per-priority guard names it.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("target must be accepted");
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("the bounce cost must be accepted");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the ability must be placed, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the announced ability must sit on the stack"
    );
    let StackEntryKind::ActivatedAbility { ability, .. } =
        &runner.state().stack.back().expect("placed").kind
    else {
        panic!("the stack top must be the announced activation");
    };
    assert_eq!(
        ability.announced_usage_key.as_ref(),
        Some(&parked_key),
        "reach-guard: placement publishes the announced key, never a re-derivation"
    );
    // The record is consumed by value into the journal at placement
    // (`record_activated_ability_placed` takes it), so the stack entry
    // carries the key but no record — while the parked prompt carrier above
    // still rode both.
    assert!(
        ability.activation_record.is_none(),
        "reach-guard: placement moved the record into the journal"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&parked_key)
            .copied(),
        Some(1),
        "reach-guard: the journal counts the announcement"
    );
    assert!(
        runner.state().pending_activations.contains(&parked_key),
        "reach-guard: the per-priority guard names the announcement"
    );

    // Trusted round-trip retains the key (redaction only redacts).
    let round_tripped: engine::types::game_state::GameState =
        serde_json::from_str(&serde_json::to_string(runner.state()).expect("serializes"))
            .expect("deserializes");
    let StackEntryKind::ActivatedAbility { ability, .. } =
        &round_tripped.stack.back().expect("placed").kind
    else {
        panic!("the round-tripped stack top must be the announced activation");
    };
    assert_eq!(
        ability.announced_usage_key.as_ref(),
        Some(&parked_key),
        "save/restore must retain the announced key"
    );

    for viewer in viewers {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        let StackEntryKind::ActivatedAbility { ability, .. } =
            &filtered.stack.back().expect("placed").kind
        else {
            panic!("{viewer:?} must see the stack entry");
        };
        assert!(
            ability.announced_usage_key.is_none(),
            "{viewer:?} sees a stack-resident announced key"
        );
        assert!(
            ability.activation_record.is_none(),
            "{viewer:?} sees a stack-resident activation record"
        );
        assert!(
            filtered.pending_activations.is_empty(),
            "{viewer:?} sees the per-priority guard"
        );
        assert!(
            filtered.activated_abilities_this_turn.is_empty(),
            "{viewer:?} sees the turn journal"
        );
        assert!(
            filtered.activated_abilities_this_game.is_empty(),
            "{viewer:?} sees the game journal"
        );
    }
}

/// CR 400.2 + CR 602.2a + CR 605.3b: a paused mana ability (Brigid's
/// color-choice prompt) projects no announced usage key — the choke point
/// clears it — while the authoritative state keeps it.
#[test]
fn pending_mana_pause_projects_no_announced_key() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brigid = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID_TEXT)
        .id();
    // Two other creatures the active player controls — X resolves to 2.
    // No pool funding: the {T} cost taps Brigid, and the assertions read the
    // exact produced count.
    scenario.add_creature(P0, "Other Creature A", 1, 1);
    scenario.add_creature(P0, "Other Creature B", 1, 1);
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: brigid,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Brigid's mana ability must succeed");
    let WaitingFor::ChooseManaColor {
        choice, context, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the color-choice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(choice, ManaChoicePrompt::SingleColor { .. }),
        "reach-guard: the AnyOneColor production offers a color choice"
    );
    let ManaChoiceContext::ManaAbility(pending) = context else {
        panic!("the color prompt must retain the paused mana ability");
    };
    assert!(
        pending.announced_usage_key.is_some(),
        "reach-guard: the mana announcement bound the key before the pause"
    );

    for viewer in [P1, unseated()] {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        let WaitingFor::ChooseManaColor { context, .. } = &filtered.waiting_for else {
            panic!(
                "{viewer:?} must see the same color prompt, got {:?}",
                filtered.waiting_for
            );
        };
        let ManaChoiceContext::ManaAbility(pending) = context else {
            panic!("{viewer:?} must see the paused mana ability");
        };
        assert!(
            pending.announced_usage_key.is_none(),
            "{viewer:?} sees a paused announced key"
        );
    }

    // The authoritative path completes undisturbed by the projection.
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("submitting the Green color choice must succeed");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        2,
        "X = 2 other creatures yields two green mana"
    );
}
