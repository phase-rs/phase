//! Sibling rows for #11338 — Aboleth Spawn and Strict Proctor observer
//! triggers firing from verbatim Oracle text (no hand-installed shapes),
//! plus the copy-fidelity rows (retarget, modal modes) and the
//! replacement-is-not-a-trigger negative. Entry paths mirror V9 (cast) and
//! as_enters_dice_counters (PlayLand): only a real zone-change entry drives
//! these tests, never battlefield placement.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text from `client/public/card-data.json` (#11338).
const ABOLETH_SPAWN: &str = "Flash\nWard {2}\nProbing Telepathy — Whenever a creature entering under an opponent's control causes a triggered ability of that creature to trigger, you may copy that ability. You may choose new targets for the copy.";
const STRICT_PROCTOR: &str = "Flying\nWhenever a permanent entering causes a triggered ability to trigger, counter that ability unless its controller pays {2}.";
const GRAVEDIGGER: &str = "When this creature enters, you may return target creature card from your graveyard to your hand.";
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
const ELVISH_VISIONARY: &str = "When this creature enters, draw a card.";
const LLANOWAR_ELVES: &str = "{T}: Add {G}.";
const PROPHETIC_PRISM: &str =
    "When this artifact enters, draw a card.\n{1}, {T}: Add one mana of any color.";
const ARBALEST_ENGINEERS: &str = "When this creature enters, choose one —\n• This creature deals 1 damage to any target.\n• Put a +1/+1 counter on target creature. It gains trample and haste until end of turn.\n• Create a tapped Powerstone token. (It's an artifact with \"{T}: Add {C}. This mana can't be spent to cast a nonartifact spell.\")";
const GOLGARI_GUILDGATE: &str = "This land enters tapped.\n{T}: Add {B} or {G}.";

fn triggered_entries_of(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(&entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == source)
        })
        .count()
}

fn drained_or_over(runner: &GameRunner) -> bool {
    runner.state().stack.is_empty()
        || matches!(runner.state().waiting_for, WaitingFor::GameOver { .. })
}

/// Answer every `OrderTriggers` prompt with the identity order.
fn order_identity(runner: &mut GameRunner) {
    for _ in 0..8 {
        let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
            return;
        };
        let order: Vec<usize> = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("trigger ordering is accepted");
    }
    panic!("OrderTriggers never cleared");
}

/// Make `player` the active player with priority in their precombat main phase.
fn give_turn(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.phase = Phase::PreCombatMain;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Cast `spell` from its owner's hand and pass priority until it has left the
/// stack (it resolved). Stops at the first non-priority prompt.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).commit();
    for _ in 0..8 {
        let on_stack = runner.state().stack.iter().any(|entry| entry.id == spell);
        if !on_stack || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass is accepted");
    }
    panic!("spell never resolved");
}

fn mana_pool(units: &[(usize, ManaType)]) -> Vec<ManaUnit> {
    units
        .iter()
        .flat_map(|(count, kind)| {
            (0..*count).map(|_| ManaUnit::new(*kind, ObjectId(0), false, Vec::new()))
        })
        .collect()
}

/// Drive until the optional copy prompt appears (or the game drains/ends).
/// Deliberately answers nothing else: an unexpected prompt stalls the loop
/// and fails the caller's assertion with visible state.
fn drive_to_copy_prompt(runner: &mut GameRunner) {
    for _ in 0..40 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            return;
        }
        if drained_or_over(runner) {
            return;
        }
        runner.pass_both_players();
        order_identity(runner);
    }
}

/// V-ABOLETH + V-RETTARGET: the opponent's Gravedigger ETB is observed, the
/// copy is accepted and retargeted to a different graveyard card; both cards
/// are returned. Y-in-hand proves the retarget took effect (keep would
/// return X twice-attempted, leaving Y behind).
#[test]
fn aboleth_copies_opponents_etb_with_new_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aboleth = scenario
        .add_creature_from_oracle(P0, "Aboleth Spawn", 2, 3, ABOLETH_SPAWN)
        .id();
    // X and W give the original two legal targets (so placement pauses);
    // Y lives in the COPY's controller yard — "your graveyard" rebinds to P0
    // on the copy (CR 707.10), so a P1-yard card would be an illegal pick.
    let x = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    let w = scenario
        .add_creature_to_graveyard(P1, "Llanowar Elves", 1, 1)
        .id();
    let y = scenario
        .add_creature_to_graveyard(P0, "Elvish Visionary", 1, 1)
        .id();
    let gravedigger = scenario
        .add_creature_to_hand_from_oracle(P1, "Gravedigger", 2, 2, GRAVEDIGGER)
        .id();
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);

    cast_and_resolve(&mut runner, gravedigger);
    // The original ETB pauses for its target at placement: choose X.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "original ETB must pause for its target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(x)],
        })
        .expect("target X for the original");
    order_identity(&mut runner);

    // Reach guard: the observed ETB is on the stack.
    assert!(
        triggered_entries_of(runner.state(), gravedigger) > 0,
        "gravedigger ETB must be on the stack"
    );

    drive_to_copy_prompt(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "aboleth must prompt the copy, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept is accepted");

    // CR 707.10c: the copy has a target (X, inherited) so retarget is armed,
    // and the choosing player is the copy's controller (P0).
    match runner.state().waiting_for.clone() {
        WaitingFor::CopyRetarget { player, .. } => {
            assert_eq!(player, P0, "retarget belongs to the copy controller");
        }
        other => panic!("expected CopyRetarget after accepting, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(y)),
        })
        .expect("retarget to Y");

    // The copy resolves first (its own optional), then the original.
    for _ in 0..2 {
        for _ in 0..40 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ) {
                break;
            }
            if drained_or_over(&runner) {
                break;
            }
            runner.pass_both_players();
            order_identity(&mut runner);
        }
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "each gravedigger trigger offers its return, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept the return");
    }
    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().objects[&aboleth].zone == Zone::Battlefield,
        "aboleth stays put"
    );
    assert!(
        runner.state().players[1].hand.contains(&x),
        "original target X returned to P1"
    );
    assert!(
        runner.state().players[0].hand.contains(&y),
        "retargeted copy returned Y to P0 — the retarget took effect"
    );
    assert!(
        runner.state().players[1].graveyard.contains(&w),
        "unremarked card W untouched"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-RETTARGET hostile (R2, CR 707.10c): keeping the copy's now-illegal
/// target fizzles the copy without resolving (CR 608.2b — no prompt); the
/// original still returns X. No crash, clean drain.
#[test]
fn aboleth_copy_keep_targets_fizzles_original_cleanly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Aboleth Spawn", 2, 3, ABOLETH_SPAWN)
        .id();
    let x = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    // Z is the copy's legal alternative (P0's yard): the walk opens and the
    // test declines it via KeepAll, keeping the now-illegal X.
    let z = scenario
        .add_creature_to_graveyard(P0, "Elvish Visionary", 1, 1)
        .id();
    let gravedigger = scenario
        .add_creature_to_hand_from_oracle(P1, "Gravedigger", 2, 2, GRAVEDIGGER)
        .id();
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);

    // X is the only card in P1's yard, so the original auto-targets (no
    // placement prompt for a single legal target).
    cast_and_resolve(&mut runner, gravedigger);
    order_identity(&mut runner);

    drive_to_copy_prompt(&mut runner);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept is accepted");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }),
        "kept-target copy still opens the retarget walk, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep is accepted");

    // The copy kept illegal X, so it fizzles without resolving (CR 608.2b:
    // all targets illegal → removed, no prompt). Exactly one return
    // optional follows: the original's.
    for _ in 0..40 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the original offers its return, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the return");
    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().players[1].hand.contains(&x),
        "X returned exactly once (by the original; the copy fizzled)"
    );
    assert!(
        runner.state().players[0].graveyard.contains(&z),
        "declined alternative Z untouched"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-ABOLETH-NEG (R5): the same entry causes the entrant's own trigger AND
/// another permanent's trigger (Soul Warden) — Aboleth observes exactly one
/// (`CausingObject`, not `Any`).
#[test]
fn aboleth_observes_only_the_entering_creatures_own_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Aboleth Spawn", 2, 3, ABOLETH_SPAWN)
        .id();
    let warden = scenario
        .add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let visionary = scenario
        .add_creature_to_hand_from_oracle(P1, "Elvish Visionary", 1, 1, ELVISH_VISIONARY)
        .id();
    // Fresh scenarios have empty libraries — stage draws (Visionary + turns).
    for _ in 0..5 {
        scenario.add_card_to_library_top(P1, "Island");
    }
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);

    cast_and_resolve(&mut runner, visionary);
    order_identity(&mut runner);

    // Reach guards: the entry caused BOTH triggers.
    assert!(
        triggered_entries_of(runner.state(), visionary) > 0,
        "visionary ETB must be on the stack"
    );
    assert!(
        triggered_entries_of(runner.state(), warden) > 0,
        "warden trigger must be on the stack"
    );

    // Exactly one copy prompt: decline it, then fail on any second one.
    drive_to_copy_prompt(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "aboleth must prompt once, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline is accepted");
    for _ in 0..40 {
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "exactly one firing — no second prompt"
        );
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-ABOLETH-TYPE: a non-creature entering trigger (Prophetic Prism) is not
/// observed — `core_types: [Creature]` rejects the artifact.
#[test]
fn aboleth_ignores_noncreature_entering_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Aboleth Spawn", 2, 3, ABOLETH_SPAWN)
        .id();
    let prism = scenario
        .add_artifact_to_hand_from_oracle(P1, "Prophetic Prism", PROPHETIC_PRISM)
        .id();
    // Fresh scenarios have empty libraries — stage the Prism draw + turns.
    for _ in 0..5 {
        scenario.add_card_to_library_top(P1, "Island");
    }
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);
    let hand_before = runner.state().players[1].hand.len();

    cast_and_resolve(&mut runner, prism);
    order_identity(&mut runner);

    // No prompt may ever appear; the ETB still resolves (reach guard below).
    for _ in 0..40 {
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "no copy prompt for a non-creature entry"
        );
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert_eq!(
        runner.state().players[1].hand.len(),
        hand_before,
        "prism ETB drew (cast -1, draw +1) — the trigger resolved unobserved"
    );
    assert_eq!(
        runner.state().objects[&prism].zone,
        Zone::Battlefield,
        "prism entered"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-ABOLETH-SILENT: a triggerless entry (vanilla Bears) triggers NO firing
/// and NO prompt — the runtime pre-fix discriminator (pre-fix the
/// `ChangesZone` mistype fires spuriously here).
#[test]
fn aboleth_silent_for_triggerless_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Aboleth Spawn", 2, 3, ABOLETH_SPAWN)
        .id();
    // Bare textless object — Bears have no rules text, so there is no Oracle
    // to parse; the V9 drive below produces a real zone-change entry.
    let bears = scenario
        .add_creature_to_hand(P1, "Grizzly Bears", 2, 2)
        .id();
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);

    cast_and_resolve(&mut runner, bears);
    order_identity(&mut runner);

    for _ in 0..40 {
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "no copy prompt for a triggerless entry"
        );
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "reach guard: the bears entered"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-COPY-MODES (R1, CR 707.10): a copied modal trigger keeps its chosen
/// modes with no mode re-prompt. Mode 3 (Powerstone) is target-free, so no
/// retarget walk opens either.
#[test]
fn aboleth_copied_modal_trigger_keeps_chosen_mode() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Aboleth Spawn", 2, 3, ABOLETH_SPAWN)
        .id();
    let engineers = scenario
        .add_creature_to_hand_from_oracle(P1, "Arbalest Engineers", 2, 2, ARBALEST_ENGINEERS)
        .id();
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);

    runner.cast(engineers).commit();
    // Drive to the copy prompt, answering the ORIGINAL's single mode choice
    // (mode 3) and tripping on any second one (the copy must not re-prompt).
    let mut mode_prompts = 0;
    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        // Explicit prompt handling (no wildcard): only Priority and ordering
        // are passed through; anything else fails loudly instead of stalling.
        match runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice { .. } => {
                mode_prompts += 1;
                assert_eq!(
                    mode_prompts, 1,
                    "only the original chooses modes — the copy must not re-prompt"
                );
                runner
                    .act(GameAction::SelectModes { indices: vec![2] })
                    .expect("mode 3 is accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => break,
            WaitingFor::OrderTriggers { .. } => order_identity(&mut runner),
            WaitingFor::Priority { .. } => {
                runner.pass_both_players();
            }
            other => panic!("unexpected prompt in modal drive: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "aboleth must prompt the copy, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(mode_prompts, 1, "the original chose its mode exactly once");
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept is accepted");

    // Target-free copy: no retarget walk opens.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }),
        "no retarget prompt for a target-free copy, got {:?}",
        runner.state().waiting_for
    );
    for _ in 0..40 {
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::AbilityModeChoice { .. }
            ),
            "the copy must not re-prompt modes"
        );
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    // The copy is controlled by P0 (CR 707.10), so its token is P0's while
    // the original's is P1's — the split proves which resolution made which.
    let powerstones_p0 = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.controller == P0
                && obj.zone == Zone::Battlefield
                && obj.card_types.subtypes.iter().any(|s| s == "Powerstone")
        })
        .count();
    let powerstones_p1 = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.controller == P1
                && obj.zone == Zone::Battlefield
                && obj.card_types.subtypes.iter().any(|s| s == "Powerstone")
        })
        .count();
    assert_eq!(
        powerstones_p1, 1,
        "the original resolved mode 3 (P1 Powerstone)"
    );
    assert_eq!(
        powerstones_p0, 1,
        "the copy resolved mode 3 (P0 Powerstone, same chosen mode)"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-PROCTOR pay path: the observed ability's controller (P0) is prompted
/// (I5) and pays {2}; the ability resolves.
#[test]
fn proctor_counter_paid_lets_ability_resolve() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P1, "Strict Proctor", 1, 3, STRICT_PROCTOR)
        .id();
    let visionary = scenario
        .add_creature_to_hand_from_oracle(P0, "Elvish Visionary", 1, 1, ELVISH_VISIONARY)
        .id();
    scenario.with_mana_pool(P0, mana_pool(&[(2, ManaType::Colorless)]));
    // Fresh scenarios have empty libraries — stage the Visionary draw.
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Island");
    }
    let mut runner = scenario.build();
    give_turn(&mut runner, P0);
    let hand_before = runner.state().players[0].hand.len();

    cast_and_resolve(&mut runner, visionary);
    order_identity(&mut runner);

    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    match runner.state().waiting_for.clone() {
        WaitingFor::UnlessPayment { player, .. } => {
            assert_eq!(player, P0, "the observed ability's controller pays (I5)");
        }
        other => panic!("expected the unless prompt, got {other:?}"),
    }
    // Reach guard: the causing ETB is still on the stack pre-decision.
    assert!(
        triggered_entries_of(runner.state(), visionary) > 0,
        "visionary ETB must be on the stack pre-decision"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("pay is accepted");

    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before,
        "paid: ETB resolved (cast -1, draw +1)"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-PROCTOR decline path: the observed ability is countered and does not
/// resolve.
#[test]
fn proctor_counter_unpaid_removes_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P1, "Strict Proctor", 1, 3, STRICT_PROCTOR)
        .id();
    let visionary = scenario
        .add_creature_to_hand_from_oracle(P0, "Elvish Visionary", 1, 1, ELVISH_VISIONARY)
        .id();
    let mut runner = scenario.build();
    give_turn(&mut runner, P0);
    let hand_before = runner.state().players[0].hand.len();

    cast_and_resolve(&mut runner, visionary);
    order_identity(&mut runner);

    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "expected the unless prompt, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        triggered_entries_of(runner.state(), visionary) > 0,
        "visionary ETB must be on the stack pre-decision"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline is accepted");

    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before - 1,
        "countered: no draw (cast -1 only)"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-PROCTOR I2 case: Proctor's `Any` admits another permanent's trigger
/// caused by the same entry (Soul Warden), which Aboleth rejects.
#[test]
fn proctor_observes_other_source_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Strict Proctor", 1, 3, STRICT_PROCTOR)
        .id();
    let warden = scenario
        .add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let elves = scenario
        .add_creature_to_hand_from_oracle(P1, "Llanowar Elves", 1, 1, LLANOWAR_ELVES)
        .id();
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);
    let life_before = runner.state().players[1].life;

    cast_and_resolve(&mut runner, elves);
    order_identity(&mut runner);

    // Reach guard: the warden trigger (other source) is on the stack.
    assert!(
        triggered_entries_of(runner.state(), warden) > 0,
        "warden trigger must be on the stack"
    );
    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "proctor must prompt for the other-source trigger, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline is accepted");

    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert_eq!(
        runner.state().players[1].life,
        life_before,
        "countered warden trigger gains no life"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// V-NO-REPLACEMENT (R8/R9): a land entering tapped fires no observer
/// (replacements are not triggers); the paired positive proves the same
/// Proctor still counters a real trigger.
#[test]
fn proctor_silent_for_tapped_entry_but_counters_real_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Strict Proctor", 1, 3, STRICT_PROCTOR)
        .id();
    let mut gate_builder = scenario.add_land_to_hand(P0, "Golgari Guildgate");
    gate_builder.from_oracle_text(GOLGARI_GUILDGATE);
    let gate = gate_builder.id();
    let visionary = scenario
        .add_creature_to_hand_from_oracle(P1, "Elvish Visionary", 1, 1, ELVISH_VISIONARY)
        .id();
    // Stage P1 draws for the paired positive: without a library card, a
    // failed counter decks P1 and the hand assertion would pass vacuously
    // (a decked draw adds nothing).
    for _ in 0..5 {
        scenario.add_card_to_library_top(P1, "Island");
    }
    let mut runner = scenario.build();

    // P0 plays the tapped land: no Proctor firing may occur.
    let card_id = runner.state().objects[&gate].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: gate,
            card_id,
        })
        .expect("land play is accepted");
    for _ in 0..40 {
        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
            "no unless prompt for a tapped entry"
        );
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().objects[&gate].tapped,
        "reach guard: the land entered tapped (replacement applied)"
    );

    // Paired positive: a real ETB on the same board IS countered.
    give_turn(&mut runner, P1);
    let hand_before = runner.state().players[1].hand.len();
    cast_and_resolve(&mut runner, visionary);
    order_identity(&mut runner);
    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "proctor must prompt for the real trigger, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline is accepted");
    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert_eq!(
        runner.state().players[1].hand.len(),
        hand_before - 1,
        "countered: no draw"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Order P0's two observer triggers with Proctor's on top (resolves first).
/// Output position 0 is the bottom of the controller's stack slot
/// (`handle_order_triggers`), so Spawn goes first, Proctor last.
fn order_proctor_over_spawn(runner: &mut GameRunner, proctor: ObjectId, spawn: ObjectId) {
    for _ in 0..8 {
        let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
            return;
        };
        assert_eq!(triggers.len(), 2, "exactly the two observer triggers");
        let pos = |id: ObjectId| {
            triggers
                .iter()
                .position(|t| t.source_id == id)
                .expect("observer listed")
        };
        runner
            .act(GameAction::OrderTriggers {
                order: vec![pos(spawn), pos(proctor)],
            })
            .expect("trigger ordering is accepted");
    }
    panic!("OrderTriggers never cleared");
}

/// V-LKI-COPY (F1 close): the observed trigger is countered before the
/// observer resolves, and the copy is still created from last-known
/// information (CR 113.1c + CR 608.2h + CR 707.10). P0 controls Aboleth Spawn
/// and Strict Proctor; P1 casts Elvish Visionary; Proctor's trigger resolves
/// first and P1 declines to pay; accepting Spawn's copy creates a copy of the
/// countered draw trigger, so P0 draws.
#[test]
fn aboleth_copies_countered_trigger_from_lki() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spawn = scenario
        .add_creature_from_oracle(P0, "Aboleth Spawn", 2, 3, ABOLETH_SPAWN)
        .id();
    let proctor = scenario
        .add_creature_from_oracle(P0, "Strict Proctor", 1, 3, STRICT_PROCTOR)
        .id();
    let visionary = scenario
        .add_creature_to_hand_from_oracle(P1, "Elvish Visionary", 1, 1, ELVISH_VISIONARY)
        .id();
    // Both libraries staged: P0 draws from the copy, and a failed counter
    // would grow P1's hand instead of decking P1 (anti-vacuity).
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Island");
    }
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);
    let p0_hand_before = runner.state().players[0].hand.len();
    let p1_hand_before = runner.state().players[1].hand.len();

    cast_and_resolve(&mut runner, visionary);
    order_proctor_over_spawn(&mut runner, proctor, spawn);

    // Proctor resolves first: P1 (the observed ability's controller) declines.
    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    match runner.state().waiting_for.clone() {
        WaitingFor::UnlessPayment { player, .. } => {
            assert_eq!(player, P1, "the observed ability's controller pays");
        }
        other => panic!("expected the unless prompt, got {other:?}"),
    }
    // Reach guard: the Visionary ETB is still on the stack pre-decision.
    assert!(
        triggered_entries_of(runner.state(), visionary) > 0,
        "visionary ETB must be on the stack pre-decision"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline is accepted");
    // Counter confirmation: the observed trigger left the stack.
    assert_eq!(
        triggered_entries_of(runner.state(), visionary),
        0,
        "declined: visionary ETB countered off the stack"
    );

    // Spawn resolves next: accept the copy of the departed trigger.
    drive_to_copy_prompt(&mut runner);
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice {
            player, source_id, ..
        } => {
            assert_eq!(player, P0, "P0 decides the copy");
            assert_eq!(source_id, spawn, "the copy offer is Spawn's");
        }
        other => panic!("expected the copy prompt, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept is accepted");

    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    // The copy of the countered draw trigger resolved: P0 drew.
    assert_eq!(
        runner.state().players[0].hand.len(),
        p0_hand_before + 1,
        "LKI copy resolved: P0 drew from the copied trigger"
    );
    // And the original never resolved: P1 cast (-1) with no draw.
    assert_eq!(
        runner.state().players[1].hand.len(),
        p1_hand_before - 1,
        "countered: original drew nothing"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at Priority, got {:?}",
        runner.state().waiting_for
    );
}
