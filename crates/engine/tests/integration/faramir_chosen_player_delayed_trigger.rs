//! Faramir, Prince of Ithilien — chosen-player delayed trigger + retrospective
//! player-pair attack predicate (phase 5a, Verification Matrix rows 1-4, 7).
//!
//! Verbatim Oracle text (data/card-data.json `oracle_text`):
//!   "At the beginning of your end step, choose an opponent. At the beginning
//!   of that player's next end step, you draw a card if they didn't attack you
//!   that turn. Otherwise, create three 1/1 white Human Soldier creature
//!   tokens."
//!
//! Row 1 (binding=chosen): the delayed trigger's stamped target is the chosen
//! opponent (P1), not the controller (P0) — `ability.targets == [Player(P1)]`.
//! Row 2 (chosen's NEXT end step): silent through P0's own end step, fires at
//! P1's end step, one-shot consumed (exactly once). Row 3 (retrospective at
//! resolution, this-turn window): attacked-you takes the token arm,
//! didn't-attack takes the draw arm, and a planeswalker-only attack takes the
//! DRAW arm (charter row 3: planeswalker/battle attacks do NOT count,
//! CR 508.6 + Faramir ruling 2023-06-16). Row 4 (both arms): draw means hand
//! +1 with zero tokens; tokens means 3 Humans with zero draws. Row 7
//! (choice-not-target + multi-authority): no `TargetSelection` window anywhere
//! in the sequence (CR 115.10a — the Choose is not a target, so hexproof on
//! the chosen player cannot fizzle the delayed fire), and two successive
//! triggers choosing different opponents bind/fire/branch independently.
//!
//! Mutation-tested: reverting the S2 temporal rebind leaves the delayed
//! binding at `ParentTargetOwner` (row-1 stamp assertion flips); reverting the
//! S3b they-arm drops the Draw condition so the Otherwise falls back to
//! `Unimplemented` (zero-Unimplemented shape assertions flip); reverting the
//! S1 ledger to the collapsed read makes the planeswalker-only leg create
//! tokens (row-3 assertion flips).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::types::ability::{
    ChoiceType, ControllerRef, StaticDefinition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{LayersDirty, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

// Verbatim Oracle text (data/card-data.json) — building the test card from the
// real card's exact text ensures the parser takes the production branch.
const FARAMIR: &str = "At the beginning of your end step, choose an opponent. At the beginning of that player's next end step, you draw a card if they didn't attack you that turn. Otherwise, create three 1/1 white Human Soldier creature tokens.";

const PAD: u32 = 40;

/// Faramir (3/3, matches the printed card) for P0 plus a vanilla attacker for
/// P1 on a 3-player board with padded libraries (turn cycles draw). With
/// `with_walker`, P0 also gets a planeswalker for the row-3
/// planeswalker-only leg.
fn faramir_table(with_walker: bool) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let faramir = scenario
        .add_creature_from_oracle(P0, "Faramir, Prince of Ithilien", 3, 3, FARAMIR)
        .id();
    let brute = scenario.add_vanilla(P1, 2, 2);
    let walker = with_walker.then(|| {
        scenario
            .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, "+1: You gain 1 life.")
            .id()
    });
    for _ in 0..PAD {
        scenario.add_spell_to_library_top(P0, "Pad", false);
        scenario.add_spell_to_library_top(P1, "Pad", false);
        scenario.add_spell_to_library_top(P2, "Pad", false);
    }
    (scenario.build(), faramir, brute, walker)
}

/// "You have hexproof" (the Leyline of Sanctity shape) for `player`, so row 7
/// proves the delayed fire does not fizzle against a hexproof chosen player.
fn grant_hexproof(runner: &mut GameRunner, player: PlayerId) {
    let grantor = create_object(
        runner.state_mut(),
        CardId(9001),
        player,
        "You Have Hexproof Source".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&grantor)
        .expect("the grantor was just created")
        .static_definitions =
        vec![
            StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        ]
        .into();
    runner.state_mut().layers_dirty = LayersDirty::Full;
    engine::game::layers::flush_layers(runner.state_mut());
}

/// Drive priority (answering combat windows) until `phase` begins for `who`.
/// `attack` is a one-shot P1 attack declaration, consumed at P1's first
/// declare-attackers window. Any `TargetSelection` window is a hard failure:
/// Faramir's sequence declares no slots (row 7 mechanism pin).
fn drive_to_phase(
    runner: &mut GameRunner,
    phase: Phase,
    who: PlayerId,
    attack: &mut Option<(ObjectId, AttackTarget)>,
) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 2000,
            "never reached {:?}'s {:?} (phase {:?}, waiting {})",
            who,
            phase,
            runner.state().phase,
            runner.waiting_for_kind()
        );
        if runner.state().phase == phase && runner.state().active_player == who {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                let attacks = if runner.state().active_player == P1 {
                    attack.take().map(|a| vec![a]).unwrap_or_default()
                } else {
                    vec![]
                };
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks,
                        bands: vec![],
                    })
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare blockers");
            }
            WaitingFor::TargetSelection { .. } => {
                panic!(
                    "Faramir's sequence must never prompt for targets (choice-not-target, CR 115.10a); got {:?}",
                    runner.state().waiting_for
                );
            }
            WaitingFor::NamedChoice { .. } => {
                panic!(
                    "no choice prompt is expected inside the drive window; got {:?}",
                    runner.state().waiting_for
                );
            }
            _ => {
                let _ = runner.act(GameAction::PassPriority);
            }
        }
        runner.advance_until_stack_empty();
    }
}

/// Resolve Faramir's end-step trigger by choosing `who`; returns with the
/// delayed trigger installed. Pins row 1 (stamp = chosen player).
fn resolve_faramir_trigger_choose(runner: &mut GameRunner, who: PlayerId) {
    runner.advance_until_stack_empty();
    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Faramir's trigger must pause on the opponent choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(
        matches!(choice_type, ChoiceType::Opponent { .. }),
        "the choice must be Choose(Opponent), got {:?}",
        choice_type
    );
    runner
        .act(GameAction::ChooseOption {
            choice: who.0.to_string(),
        })
        .expect("choose the opponent");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "choosing must install exactly one delayed trigger"
    );
    assert_eq!(
        runner.state().delayed_triggers[0].ability.targets,
        vec![TargetRef::Player(who)],
        "row 1: the delayed trigger must stamp the CHOSEN player, not the controller"
    );
}

fn p0_hand(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .hand
        .len()
}

fn p0_human_soldier_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.controller == P0
                && o.name == "Human Soldier"
                && o.zone == Zone::Battlefield
        })
        .map(|o| o.id)
        .collect()
}

/// Row 2 + row 3 (didn't-attack) + row 4 (draw arm) + row 7 (no-targeting pin
/// plus a second trigger choosing a different opponent on the next cycle).
#[test]
fn faramir_draws_when_chosen_opponent_did_not_attack_you() {
    let (mut runner, _faramir, _brute, _walker) = faramir_table(false);

    // P0's end step: trigger fires, choose P1.
    drive_to_phase(&mut runner, Phase::End, P0, &mut None);
    resolve_faramir_trigger_choose(&mut runner, P1);
    let hand0 = p0_hand(&runner);

    // Row 2 (silence leg): through the rest of P0's turn and all of P1's turn
    // up to postcombat, the trigger stays pending and nothing happens.
    drive_to_phase(&mut runner, Phase::PostCombatMain, P1, &mut None);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "row 2: the delayed trigger must stay pending until the CHOSEN player's end step"
    );
    assert_eq!(
        p0_hand(&runner),
        hand0,
        "row 2: silence before the chosen end step means no early draw"
    );
    assert!(
        p0_human_soldier_tokens(&runner).is_empty(),
        "row 2: silence before the chosen end step means no early tokens"
    );

    // Fire leg: P1 attacked nobody, so the draw arm fires exactly once.
    drive_to_phase(&mut runner, Phase::End, P1, &mut None);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "row 2: the one-shot delayed trigger must be consumed after firing exactly once"
    );
    assert_eq!(
        p0_hand(&runner),
        hand0 + 1,
        "row 4: the draw arm draws exactly one card"
    );
    assert!(
        p0_human_soldier_tokens(&runner).is_empty(),
        "row 4: the draw arm creates zero tokens"
    );

    // Row 7 (multi-authority): next cycle chooses a DIFFERENT opponent and
    // binds/fires/branches independently.
    drive_to_phase(&mut runner, Phase::End, P0, &mut None);
    resolve_faramir_trigger_choose(&mut runner, P2);
    let hand1 = p0_hand(&runner);
    drive_to_phase(&mut runner, Phase::PostCombatMain, P2, &mut None);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "row 7: the second trigger stays pending until P2's end step"
    );
    assert_eq!(
        p0_hand(&runner),
        hand1,
        "row 7: silence before P2's end step means no early draw"
    );
    drive_to_phase(&mut runner, Phase::End, P2, &mut None);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "row 7: the second delayed trigger must fire exactly once and be consumed"
    );
    assert_eq!(
        p0_hand(&runner),
        hand1 + 1,
        "row 7: the second trigger draws exactly one card for the other chosen opponent"
    );
    assert!(
        p0_human_soldier_tokens(&runner).is_empty(),
        "row 7: the second trigger creates zero tokens"
    );
}

/// Row 3 (attacked-you) + row 4 (token arm) + row 7 (hexproof chosen player
/// does not fizzle the delayed fire).
#[test]
fn faramir_creates_tokens_when_chosen_opponent_attacked_you_despite_hexproof() {
    let (mut runner, _faramir, brute, _walker) = faramir_table(false);
    grant_hexproof(&mut runner, P1);

    drive_to_phase(&mut runner, Phase::End, P0, &mut None);
    resolve_faramir_trigger_choose(&mut runner, P1);
    let hand0 = p0_hand(&runner);

    // P1 attacks P0 directly during P1's combat; silence leg pins no early
    // fire through the attack itself.
    let mut attack = Some((brute, AttackTarget::Player(P0)));
    drive_to_phase(&mut runner, Phase::PostCombatMain, P1, &mut attack);
    assert!(
        attack.is_none(),
        "test setup error: P1's declare-attackers window never surfaced"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the delayed trigger must still be pending at P1's postcombat"
    );
    assert_eq!(
        p0_hand(&runner),
        hand0,
        "silence through P1's combat means no early draw"
    );

    // Fire leg: P1 attacked P0, so the token arm fires exactly once.
    drive_to_phase(&mut runner, Phase::End, P1, &mut None);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the one-shot delayed trigger must be consumed after firing"
    );
    let tokens = p0_human_soldier_tokens(&runner);
    assert_eq!(
        tokens.len(),
        3,
        "row 4: the token arm creates exactly three Human Soldiers, got {}",
        tokens.len()
    );
    for id in &tokens {
        let token = &runner.state().objects[id];
        assert_eq!(token.power, Some(1), "token must be 1/1");
        assert_eq!(token.toughness, Some(1), "token must be 1/1");
    }
    assert_eq!(
        p0_hand(&runner),
        hand0,
        "row 4: the token arm draws zero cards (row 7: hexproof on the chosen player fizzles nothing)"
    );
}

/// Row 3 characterization: a planeswalker-only attack is NOT attacking you
/// (charter row 3, CR 508.6 + Faramir ruling 2023-06-16) — the draw arm fires.
#[test]
fn faramir_draws_when_chosen_opponent_only_attacked_a_planeswalker() {
    let (mut runner, _faramir, brute, walker) = faramir_table(true);
    let walker = walker.expect("row-3 leg needs the planeswalker");

    drive_to_phase(&mut runner, Phase::End, P0, &mut None);
    resolve_faramir_trigger_choose(&mut runner, P1);
    let hand0 = p0_hand(&runner);

    // P1 attacks ONLY P0's planeswalker during P1's combat.
    let mut attack = Some((brute, AttackTarget::Planeswalker(walker)));
    drive_to_phase(&mut runner, Phase::PostCombatMain, P1, &mut attack);
    assert!(
        attack.is_none(),
        "test setup error: P1's declare-attackers window never surfaced"
    );
    // Ledger pin: the planeswalker attack recorded no direct-player attack.
    assert!(
        !runner
            .state()
            .has_attacked_player_directly_this_turn(P1, P0),
        "row 3: a planeswalker-only attack must not enter the direct-player ledger"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the delayed trigger must still be pending at P1's postcombat"
    );
    assert_eq!(
        p0_hand(&runner),
        hand0,
        "silence through the planeswalker attack means no early draw"
    );

    // Fire leg: planeswalker-only is NOT attacking you — the draw arm fires.
    drive_to_phase(&mut runner, Phase::End, P1, &mut None);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the one-shot delayed trigger must be consumed after firing"
    );
    assert_eq!(
        p0_hand(&runner),
        hand0 + 1,
        "row 3: planeswalker-only attack takes the DRAW arm"
    );
    assert!(
        p0_human_soldier_tokens(&runner).is_empty(),
        "row 3: planeswalker-only attack creates zero tokens"
    );
}
