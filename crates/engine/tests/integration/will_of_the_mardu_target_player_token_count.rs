//! Will of the Mardu (and its class sibling Ferrafor, Young Yew): a token
//! count read off a target-bound population ("a number of ... tokens equal to
//! the number of creatures target player controls") must declare its own
//! target-player slot.
//!
//! The defect: `Effect::Token` was missing from the quantity arm of
//! `effect_target_slot_filter`, so no player slot was declared. At resolution
//! the count read no declared player, evaluated to 0, and no tokens were
//! created. The tokens' owner is the controller (CR 111.2), so the effect's
//! `owner` never supplied the slot either.
//!
//! CR 115.1 + CR 601.2c: targets are announced as the spell is cast, one per
//! instance of the word "target".
//! CR 700.2c: a mode's targets are chosen only if that mode was chosen.
//! CR 608.2c: the chosen modes resolve in printed order, so the damage mode's
//! "creatures you control" sees the Warriors the first mode created.
//! CR 903.3d: "you control a commander" means any commander on the
//! battlefield the caster controls, whoever owns it.
//! CR 603.3d: a triggered ability (Ferrafor's ETB) announces its targets as it
//! is put on the stack.
//!
//! Revert-fail: with the `Effect::Token` arm removed, T0 sees one target slot
//! instead of two, and T1 sees zero Warriors and 2 damage instead of 3 and 5.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::cast_spell_action;

/// Verbatim Oracle text. "Will of the Mardu deals damage" normalizes to the
/// self-reference because the scenario card carries the same name.
const WILL_OF_THE_MARDU: &str = "Choose one. If you control a commander as you cast this spell, you may choose both instead.\n\
    • Create a number of 1/1 red Warrior creature tokens equal to the number of creatures target player controls.\n\
    • Will of the Mardu deals damage to target creature equal to the number of creatures you control.";

/// Verbatim Oracle text of the class sibling.
const FERRAFOR_YOUNG_YEW: &str = "When Ferrafor enters, create a number of 1/1 green Saproling creature tokens equal to the number of counters among creatures target player controls.\n\
    {T}: Double the number of each kind of counter on target creature.";

/// Where the commander that gates "you may choose both instead" sits.
#[derive(Clone, Copy, Debug)]
enum CommanderSetup {
    /// P0 owns and controls a commander on the battlefield.
    ControlledByCaster,
    /// The same creature on P0's battlefield, but not a commander.
    Absent,
    /// P0's commander is in the command zone, not on the battlefield.
    InCommandZone,
    /// P1 owns and controls the commander.
    ControlledByOpponent,
    /// P1 owns the commander; P0 controls it.
    StolenByCaster,
}

struct MarduBoard {
    runner: GameRunner,
    will: ObjectId,
    victim: ObjectId,
}

/// P0: "Bear" plus the "Test Commander" creature placed per `setup`.
/// P1: "Victim" (1/9, survives every damage total here) and two 1/1 "Grunt"s,
/// so P1 controls 3 creatures unless the commander also sits with P1.
fn mardu_board(setup: CommanderSetup) -> MarduBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature(P0, "Bear", 2, 2);
    match setup {
        CommanderSetup::ControlledByCaster => {
            scenario
                .add_creature(P0, "Test Commander", 2, 2)
                .commander();
        }
        CommanderSetup::Absent => {
            scenario.add_creature(P0, "Test Commander", 2, 2);
        }
        CommanderSetup::InCommandZone => {
            let commander = scenario.add_creature(P0, "Test Commander", 2, 2).id();
            scenario.with_commander(commander);
        }
        CommanderSetup::ControlledByOpponent => {
            scenario
                .add_creature(P1, "Test Commander", 2, 2)
                .commander();
        }
        CommanderSetup::StolenByCaster => {
            scenario
                .add_creature(P1, "Test Commander", 2, 2)
                .commander()
                .controlled_by(P0);
        }
    }

    let victim = scenario.add_creature(P1, "Victim", 1, 9).id();
    scenario.add_creature(P1, "Grunt", 1, 1);
    scenario.add_creature(P1, "Grunt", 1, 1);

    let will = scenario
        .add_spell_to_hand_from_oracle(P0, "Will of the Mardu", true, WILL_OF_THE_MARDU)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    MarduBoard {
        runner: scenario.build(),
        will,
        victim,
    }
}

/// Tokens of `subtype` on the battlefield under `controller`.
fn tokens_with_subtype(state: &GameState, controller: PlayerId, subtype: &str) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = &state.objects[id];
            obj.is_token
                && obj.controller == controller
                && obj.card_types.subtypes.iter().any(|s| s == subtype)
        })
        .collect()
}

/// Creatures on the battlefield under `controller`.
fn creatures_controlled(state: &GameState, controller: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &state.objects[*id];
            obj.controller == controller && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count()
}

/// Cast the spell and return the effective cap the mode prompt offers.
fn cast_to_mode_choice(runner: &mut GameRunner, will: ObjectId) -> usize {
    let cast = cast_spell_action(runner, will);
    runner
        .act(cast)
        .expect("casting Will of the Mardu must be accepted");
    match &runner.state().waiting_for {
        WaitingFor::ModeChoice { modal, .. } => modal.max_choices,
        other => panic!("expected the mode prompt, got {other:?}"),
    }
}

/// Pass priority until the stack is empty.
fn pass_until_stack_empty(runner: &mut GameRunner) {
    for _ in 0..16 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            other => panic!("unexpected window while resolving: {other:?}"),
        };
        runner
            .act(action)
            .expect("passing priority must be accepted");
    }
    panic!("the stack did not empty within the window budget");
}

/// Choose only the token mode, assert the prompt asks for exactly one player,
/// answer it with `target`, and resolve. This is the reach guard for the
/// single-mode rows: the fluent driver answers prompts itself and cannot
/// assert on them.
fn cast_token_mode_only(runner: &mut GameRunner, will: ObjectId, target: PlayerId) {
    cast_to_mode_choice(runner, will);
    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("choosing the token mode must be accepted");
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the token mode must ask for its target player; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        target_slots.len(),
        1,
        "the token mode declares exactly one target: the player whose creatures are counted"
    );
    assert!(
        target_slots[0]
            .legal_targets
            .iter()
            .all(|t| matches!(t, TargetRef::Player(_))),
        "the token mode's slot must offer players only: {:?}",
        target_slots[0].legal_targets
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(target)),
        "the intended player must be a legal target: {:?}",
        target_slots[0].legal_targets
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(target)),
        })
        .expect("choosing the target player must be accepted");
    pass_until_stack_empty(runner);
}

/// Cast, try to choose both modes, and report the cap and the result.
fn choose_both_result(setup: CommanderSetup) -> (usize, Result<(), String>) {
    let MarduBoard {
        mut runner, will, ..
    } = mardu_board(setup);
    let cap = cast_to_mode_choice(&mut runner, will);
    let result = runner
        .act(GameAction::SelectModes {
            indices: vec![0, 1],
        })
        .map(|_| ())
        .map_err(|err| format!("{err:?}"));
    (cap, result)
}

/// T0: with a commander, choosing both modes surfaces TWO target slots: the
/// token count's player (CR 115.1 + CR 601.2c) and the damage mode's creature.
#[test]
fn choosing_both_modes_declares_the_token_count_player_slot() {
    let MarduBoard {
        mut runner,
        will,
        victim,
    } = mardu_board(CommanderSetup::ControlledByCaster);

    // Reach guard: the commander lifts the cap to two modes.
    assert_eq!(
        cast_to_mode_choice(&mut runner, will),
        2,
        "controlling a commander must allow choosing both modes"
    );
    runner
        .act(GameAction::SelectModes {
            indices: vec![0, 1],
        })
        .expect("choosing both modes must be accepted with a commander");

    let WaitingFor::TargetSelection {
        target_slots,
        mode_labels,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "both modes declare targets; got {:?}",
            runner.state().waiting_for
        );
    };

    assert_eq!(
        target_slots.len(),
        2,
        "the token count's target player and the damage mode's target creature are two \
         instances of \"target\"; slots: {target_slots:?}"
    );
    // Slot 0: mode 1's player, announced first (printed order).
    assert!(
        target_slots[0]
            .legal_targets
            .iter()
            .all(|t| matches!(t, TargetRef::Player(_))),
        "slot 0 must offer players only: {:?}",
        target_slots[0].legal_targets
    );
    for player in [P0, P1] {
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Player(player)),
            "\"target player\" admits every player; {player:?} missing from {:?}",
            target_slots[0].legal_targets
        );
    }
    // Slot 1: mode 2's creature.
    assert!(
        target_slots[1]
            .legal_targets
            .iter()
            .all(|t| matches!(t, TargetRef::Object(_))),
        "slot 1 must offer objects only: {:?}",
        target_slots[1].legal_targets
    );
    assert!(
        target_slots[1]
            .legal_targets
            .contains(&TargetRef::Object(victim)),
        "the damage mode must be able to target the victim: {:?}",
        target_slots[1].legal_targets
    );
    // The labels name the mode each slot belongs to.
    assert_eq!(
        mode_labels.len(),
        target_slots.len(),
        "mode labels run parallel to the slots: {mode_labels:?}"
    );
    assert!(
        mode_labels[0]
            .as_deref()
            .is_some_and(|label| label.to_lowercase().contains("warrior")),
        "slot 0 belongs to the token mode: {mode_labels:?}"
    );
}

/// T1: choose both with a commander, targeting P1 (3 creatures). Mode 1 makes
/// 3 Warriors that P0 owns and controls (CR 111.2); mode 2 then counts P0's 5
/// creatures, Warriors included (CR 608.2c).
#[test]
fn choose_both_creates_warriors_for_target_players_creatures_then_counts_them() {
    let MarduBoard {
        mut runner,
        will,
        victim,
    } = mardu_board(CommanderSetup::ControlledByCaster);

    let outcome = runner
        .cast(will)
        .modes(&[0, 1])
        .target_player(P1)
        .target_objects(&[victim])
        .resolve();
    let state = outcome.state();

    let warriors = tokens_with_subtype(state, P0, "Warrior");
    assert_eq!(
        warriors.len(),
        3,
        "one Warrior per creature the target player (P1) controls"
    );
    for id in &warriors {
        let obj = &state.objects[id];
        assert_eq!(obj.owner, P0, "the caster creates and owns the tokens");
        assert_eq!((obj.power, obj.toughness), (Some(1), Some(1)));
        assert!(
            obj.color.contains(&ManaColor::Red),
            "Warriors are red: {:?}",
            obj.color
        );
    }
    assert!(
        tokens_with_subtype(state, P1, "Warrior").is_empty(),
        "the target player does not get the tokens"
    );
    assert_eq!(creatures_controlled(state, P0), 5);
    assert_eq!(
        creatures_controlled(state, P1),
        3,
        "P1's creatures are unchanged"
    );
    assert_eq!(
        outcome.damage_marked(victim),
        5,
        "mode 2 counts the creatures P0 controls after mode 1 resolved"
    );
    assert!(state.stack.is_empty(), "the spell finished resolving");
    assert_eq!(outcome.zone_of(will), Zone::Graveyard);
}

/// T2: the order modes are submitted in does not change the printed
/// resolution order (CR 608.2c): tokens still come first.
#[test]
fn choose_both_resolves_in_printed_order_regardless_of_submission_order() {
    let MarduBoard {
        mut runner,
        will,
        victim,
    } = mardu_board(CommanderSetup::ControlledByCaster);

    let outcome = runner
        .cast(will)
        .modes(&[1, 0])
        .target_player(P1)
        .target_objects(&[victim])
        .resolve();
    let state = outcome.state();

    assert_eq!(tokens_with_subtype(state, P0, "Warrior").len(), 3);
    assert_eq!(outcome.damage_marked(victim), 5);
    assert!(state.stack.is_empty());
}

/// T3: the count reads the DECLARED player. Targeting P0 (2 creatures) makes
/// 2 Warriors, and mode 2 then counts 4. T1 is the paired positive with P1.
#[test]
fn token_count_reads_the_declared_player() {
    let MarduBoard {
        mut runner,
        will,
        victim,
    } = mardu_board(CommanderSetup::ControlledByCaster);

    let outcome = runner
        .cast(will)
        .modes(&[0, 1])
        .target_player(P0)
        .target_objects(&[victim])
        .resolve();
    let state = outcome.state();

    assert_eq!(
        tokens_with_subtype(state, P0, "Warrior").len(),
        2,
        "one Warrior per creature P0 controlled when targeted"
    );
    assert_eq!(creatures_controlled(state, P0), 4);
    assert_eq!(creatures_controlled(state, P1), 3);
    assert_eq!(outcome.damage_marked(victim), 4);
}

/// T4: without a commander, choosing one mode works for either mode. Mode 1
/// alone needs no creature target (CR 700.2c); mode 2 alone makes no tokens.
#[test]
fn choose_one_without_commander_runs_either_mode() {
    // Mode 1 only.
    let MarduBoard {
        mut runner,
        will,
        victim,
    } = mardu_board(CommanderSetup::Absent);
    cast_token_mode_only(&mut runner, will, P1);
    let state = runner.state();
    assert_eq!(
        tokens_with_subtype(state, P0, "Warrior").len(),
        3,
        "mode 1 alone makes one Warrior per creature P1 controls"
    );
    assert_eq!(
        state.objects[&victim].damage_marked, 0,
        "mode 2 was not chosen"
    );
    assert_eq!(state.objects[&will].zone, Zone::Graveyard);

    // Mode 2 only.
    let MarduBoard {
        mut runner,
        will,
        victim,
    } = mardu_board(CommanderSetup::Absent);
    let outcome = runner
        .cast(will)
        .modes(&[1])
        .target_object(victim)
        .resolve();
    assert_eq!(
        outcome.damage_marked(victim),
        2,
        "mode 2 alone counts P0's two creatures"
    );
    assert!(
        tokens_with_subtype(outcome.state(), P0, "Warrior").is_empty(),
        "mode 1 was not chosen"
    );
}

/// T5: choosing both is available only while P0 controls a commander on the
/// battlefield (CR 903.3d). Each rejected setup is paired with the accepted
/// commander-on-battlefield control.
#[test]
fn choose_both_requires_controlling_a_commander_on_the_battlefield() {
    // Positive control: P0 controls its own commander.
    let (cap, result) = choose_both_result(CommanderSetup::ControlledByCaster);
    assert_eq!(cap, 2);
    assert!(result.is_ok(), "choosing both must be accepted: {result:?}");

    // CR 903.3d: "a commander", whoever owns it. A stolen commander counts.
    let (cap, result) = choose_both_result(CommanderSetup::StolenByCaster);
    assert_eq!(cap, 2, "a commander P0 controls but P1 owns still counts");
    assert!(result.is_ok(), "choosing both must be accepted: {result:?}");

    for setup in [
        CommanderSetup::Absent,
        CommanderSetup::InCommandZone,
        CommanderSetup::ControlledByOpponent,
    ] {
        let (cap, result) = choose_both_result(setup);
        assert_eq!(cap, 1, "{setup:?}: no commander on P0's battlefield");
        let Err(message) = result else {
            panic!("{setup:?}: choosing both must be rejected");
        };
        assert!(
            message.contains("between 1 and 1"),
            "{setup:?}: unexpected rejection: {message}"
        );
    }
}

/// T5 (stolen commander, full resolution): the accepted stolen-commander
/// setup resolves both modes like T1. P0 controls Bear and the stolen
/// commander; P1 controls Victim and two Grunts.
#[test]
fn stolen_commander_lets_both_modes_resolve() {
    let MarduBoard {
        mut runner,
        will,
        victim,
    } = mardu_board(CommanderSetup::StolenByCaster);

    let outcome = runner
        .cast(will)
        .modes(&[0, 1])
        .target_player(P1)
        .target_objects(&[victim])
        .resolve();

    assert_eq!(tokens_with_subtype(outcome.state(), P0, "Warrior").len(), 3);
    assert_eq!(outcome.damage_marked(victim), 5);
}

/// T6: a target player with no creatures is still announced, and the count
/// is 0, so no Warriors are made.
#[test]
fn target_player_with_no_creatures_makes_no_warriors() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Bear", 2, 2);
    scenario
        .add_creature(P0, "Test Commander", 2, 2)
        .commander();
    let will = scenario
        .add_spell_to_hand_from_oracle(P0, "Will of the Mardu", true, WILL_OF_THE_MARDU)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    cast_token_mode_only(&mut runner, will, P1);

    let state = runner.state();
    assert_eq!(
        state.objects[&will].zone,
        Zone::Graveyard,
        "the spell resolved"
    );
    assert!(
        tokens_with_subtype(state, P0, "Warrior").is_empty(),
        "P1 controls no creatures, so the count is 0"
    );
    assert_eq!(creatures_controlled(state, P0), 2);
}

/// T7: Ferrafor's ETB trigger has the same shape: "a number of ... tokens
/// equal to the number of counters among creatures target player controls".
/// The trigger announces its player (CR 603.3d) and the count reads it.
/// Targeting P1 (4 counters) makes 4 Saprolings; targeting P0 (1 counter)
/// makes 1, which proves the declared player is the one read.
#[test]
fn ferrafor_etb_counts_counters_among_target_players_creatures() {
    fn ferrafor_board() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Seedling", 1, 1)
            .with_plus_counters(1);
        scenario
            .add_creature(P1, "Elder", 2, 2)
            .with_plus_counters(3);
        scenario
            .add_creature(P1, "Sprout", 1, 1)
            .with_plus_counters(1);
        let ferrafor = scenario
            .add_creature_to_hand_from_oracle(P0, "Ferrafor, Young Yew", 4, 7, FERRAFOR_YOUNG_YEW)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        (scenario.build(), ferrafor)
    }

    let (mut runner, ferrafor) = ferrafor_board();
    let outcome = runner.cast(ferrafor).target_player(P1).resolve();
    assert_eq!(
        outcome.zone_of(ferrafor),
        Zone::Battlefield,
        "Ferrafor must resolve and enter"
    );
    assert!(outcome.state().stack.is_empty(), "the ETB trigger resolved");
    let saprolings = tokens_with_subtype(outcome.state(), P0, "Saproling");
    assert_eq!(
        saprolings.len(),
        4,
        "one Saproling per counter among P1's creatures"
    );
    for id in &saprolings {
        assert_eq!(outcome.state().objects[id].owner, P0);
    }

    let (mut runner, ferrafor) = ferrafor_board();
    let outcome = runner.cast(ferrafor).target_player(P0).resolve();
    assert_eq!(outcome.zone_of(ferrafor), Zone::Battlefield);
    assert_eq!(
        tokens_with_subtype(outcome.state(), P0, "Saproling").len(),
        1,
        "targeting P0 counts only the counter among P0's creatures"
    );
}
