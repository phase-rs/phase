//! CR 601.2c + CR 603.3d + CR 102.2: the Exodus Oath cycle's printed target
//! restriction ("target player who controls more ⟨type⟩ than they do and is
//! their opponent") must be enforced when the triggered ability is put on the
//! stack — and the printed subject of that sentence ("THAT PLAYER chooses …")
//! must be the seat that announces it.
//!
//! Before the target-position relative-clause hook, the whole restriction was
//! parsed away: the ability announced `TargetFilter::Player`, every seat was a
//! legal target (including the upkeep player themselves), and the trigger
//! resolved on every upkeep no matter what was on the battlefield. That is the
//! reported Oath of Druids defect — a free creature every turn regardless of
//! board state. The announcing player was dropped with it, leaving the
//! enchantment's controller choosing on every player's upkeep.
//!
//! Independent halves are pinned separately, because each is invisible in the
//! board state that only exercises the others:
//!  * with no seat satisfying the comparison, CR 603.3d removes the ability from
//!    the stack rather than letting it resolve untargeted;
//!  * the comparison is anchored on the UPKEEP player (CR 603.2 "they"), not on
//!    the enchantment's controller — so on the SAME board the trigger does
//!    something on one player's upkeep and nothing on the other's;
//!  * the announcement is made by the upkeep player (CR 601.2c), observable only
//!    where the announced target and the ability's controller differ.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::TargetRef;

const OATH_OF_DRUIDS: &str = "At the beginning of each player's upkeep, that player chooses target player who controls more creatures than they do and is their opponent. The first player may reveal cards from the top of their library until they reveal a creature card. If the first player does, that player puts that card onto the battlefield and all other cards revealed this way into their graveyard.";

const OATH_OF_LIEGES: &str = "At the beginning of each player's upkeep, that player chooses target player who controls more lands than they do and is their opponent. The first player may search their library for a basic land card, put that card onto the battlefield, then shuffle.";

/// Continue inside the upkeep step the runner is already in, returning the next
/// prompt it raises, or `None` once the step ends with nothing but priority
/// windows — which is what CR 603.3d's "the ability is simply removed from the
/// stack" looks like from outside the engine.
///
/// Priority is passed inside the step rather than returned, because the Oath's
/// two decision points arrive in sequence: the CR 601.2c target announcement at
/// stack placement, then the CR 608.2d "may" once the ability resolves off the
/// stack.
fn prompt_in_current_upkeep(runner: &mut GameRunner) -> Option<WaitingFor> {
    let upkeep_player = runner.state().active_player;
    for _ in 0..200 {
        if runner.state().phase != Phase::Upkeep || runner.state().active_player != upkeep_player {
            return None;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).ok();
            }
            other => return Some(other),
        }
    }
    panic!("upkeep step never ended");
}

/// Advance to the next upkeep step (leaving any the caller is already inside)
/// and report whose it is, plus the first prompt raised in it.
fn next_upkeep_prompt(runner: &mut GameRunner) -> (PlayerId, Option<WaitingFor>) {
    let mut left_current_step = runner.state().phase != Phase::Upkeep;
    for _ in 0..600 {
        let in_upkeep = runner.state().phase == Phase::Upkeep;
        if !left_current_step {
            left_current_step = !in_upkeep;
        } else if in_upkeep {
            let upkeep_player = runner.state().active_player;
            return (upkeep_player, prompt_in_current_upkeep(runner));
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).ok();
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .ok();
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .ok();
            }
            other => panic!("unexpected prompt outside an upkeep step: {other:?}"),
        }
    }
    panic!("game did not reach an upkeep step");
}

/// Assert the prompt is the Oath's target announcement, made by `announcer`
/// over exactly `legal`, and answer it with the sole legal target.
///
/// CR 601.2c: a slot whose announcing seat is not the ability's controller is
/// never auto-selected, even when only one assignment is legal — the engine
/// refuses to journal an announcement on another seat's behalf. So this prompt
/// comes up on every upkeep the trigger survives, and the announcer is directly
/// observable.
fn announce_sole_target(
    runner: &mut GameRunner,
    prompt: Option<WaitingFor>,
    announcer: PlayerId,
    legal: &[TargetRef],
) {
    let Some(WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        selection,
        ..
    }) = prompt
    else {
        panic!("expected the Oath's target announcement, got {prompt:?}");
    };
    assert_eq!(
        player, announcer,
        "the printed subject of \"that player chooses target player\" announces the target"
    );
    assert_eq!(
        target_slots.first().and_then(|slot| slot.chooser),
        Some(announcer),
        "the slot must carry the announcing seat so multiplayer routing agrees with the prompt"
    );
    assert_eq!(selection.current_legal_targets, legal);
    let target = selection.current_legal_targets[0].clone();
    runner
        .act(GameAction::SelectTargets {
            targets: vec![target],
        })
        .expect("announcing the sole legal target must be accepted");
}

fn creature_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature))
        })
        .count()
}

/// CR 603.3d: no seat controls more creatures than the upkeep player, so no
/// legal target can be chosen and the ability is simply removed from the stack.
/// This is the reported defect stated directly — nobody gets a free creature.
#[test]
fn oath_of_druids_with_no_legal_target_never_reaches_the_reveal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    scenario.add_enchantment_from_oracle(P0, "Oath of Druids", OATH_OF_DRUIDS);
    // Deliberately symmetric: both seats control zero creatures, so
    // "controls more creatures than they do" is false for every candidate.
    let mut runner = scenario.build();

    for _ in 0..2 {
        let (player, prompt) = next_upkeep_prompt(&mut runner);
        assert!(
            prompt.is_none(),
            "with no seat controlling more creatures, {player:?}'s Oath trigger must be \
             removed from the stack (CR 603.3d), got {prompt:?}"
        );
    }

    assert_eq!(
        creature_count(&runner),
        0,
        "no Oath trigger resolved, so no creature can have been put onto the battlefield"
    );
}

/// CR 603.2 + CR 109.5 + CR 601.2c: the comparison is anchored on the UPKEEP
/// player, not on the enchantment's controller, and that same player announces
/// the target. On one fixed board the trigger therefore runs its whole flow on
/// the behind player's upkeep and does nothing at all on the ahead player's — a
/// controller-anchored comparison would behave the same way on both.
#[test]
fn oath_of_druids_anchors_the_comparison_on_the_upkeep_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    // P0 controls the Oath AND is the seat ahead on creatures.
    scenario.add_enchantment_from_oracle(P0, "Oath of Druids", OATH_OF_DRUIDS);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    let mut runner = scenario.build();

    // P1's upkeep: P1 controls 0 creatures, P0 controls 1, and P0 is P1's
    // opponent — so P1 announces P0, then gets the reveal offered to them.
    let (player, prompt) = next_upkeep_prompt(&mut runner);
    assert_eq!(player, P1, "the first upkeep reached must be P1's");
    announce_sole_target(&mut runner, prompt, P1, &[TargetRef::Player(P0)]);
    match prompt_in_current_upkeep(&mut runner) {
        Some(WaitingFor::OptionalEffectChoice { player, .. }) => assert_eq!(
            player, P1,
            "the reveal is the FIRST player's option — the upkeep player, not the Oath's controller"
        ),
        other => panic!("expected the optional reveal on P1's upkeep, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("the Oath reveal is a 'may', so declining must be legal");

    // P0's own upkeep on the same board: nobody controls more creatures than P0,
    // so the trigger has no legal target and never reaches the reveal.
    let (player, prompt) = next_upkeep_prompt(&mut runner);
    assert_eq!(player, P0, "the second upkeep reached must be P0's");
    assert!(
        prompt.is_none(),
        "the seat that is AHEAD on creatures has no legal target on its own upkeep; \
         a controller-anchored comparison would wrongly prompt here, got {prompt:?}"
    );
}

/// CR 115.1 + CR 102.2: with more than one candidate the announced legal set is
/// visible directly. It must contain exactly the opponents who control more
/// creatures than the upkeep player — never the upkeep player themselves, and
/// never an opponent who is not ahead.
#[test]
fn oath_of_druids_legal_targets_exclude_self_and_seats_not_ahead() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p2 = PlayerId(2);
    for &pid in &[P0, P1, p2] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    scenario.add_enchantment_from_oracle(P0, "Oath of Druids", OATH_OF_DRUIDS);
    // P0 and P2 are both ahead of P1; P1 (the upkeep player) controls none.
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(p2, "Runeclaw Bear", 2, 2);
    let mut runner = scenario.build();

    let (player, prompt) = next_upkeep_prompt(&mut runner);
    assert_eq!(player, P1, "the first upkeep reached must be P1's");
    let Some(WaitingFor::TriggerTargetSelection { selection, .. }) = prompt else {
        panic!("two legal targets must surface a target prompt, got {prompt:?}");
    };
    assert_eq!(
        selection.current_legal_targets,
        vec![TargetRef::Player(P0), TargetRef::Player(p2)],
        "only opponents controlling MORE creatures than the upkeep player are legal; \
         the upkeep player must never be able to target themselves"
    );
}

/// CR 601.2c + CR 603.3a: the ability stays under the enchantment controller's
/// control, but the printed subject moves the ANNOUNCEMENT to the upkeep player.
/// Three seats, because with a single legal target the two seats' choices are
/// indistinguishable.
#[test]
fn oath_of_druids_target_is_announced_by_the_upkeep_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p2 = PlayerId(2);
    for &pid in &[P0, P1, p2] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    scenario.add_enchantment_from_oracle(P0, "Oath of Druids", OATH_OF_DRUIDS);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(p2, "Runeclaw Bear", 2, 2);
    let mut runner = scenario.build();

    let (upkeep_player, prompt) = next_upkeep_prompt(&mut runner);
    assert_eq!(upkeep_player, P1, "the first upkeep reached must be P1's");
    let Some(WaitingFor::TriggerTargetSelection {
        player,
        trigger_controller,
        target_slots,
        ..
    }) = prompt
    else {
        panic!("two legal targets must surface a target prompt");
    };
    assert_eq!(
        trigger_controller,
        Some(P0),
        "the ability is still controlled by the Oath's controller (CR 603.3a)"
    );
    assert_eq!(
        player, P1,
        "but the UPKEEP player announces the target — the printed subject of \
         \"that player chooses target player\" overrides CR 601.2c's default"
    );
    assert_eq!(
        target_slots.first().and_then(|slot| slot.chooser),
        Some(P1),
        "the slot must carry the announcing player so multiplayer routing agrees \
         with the prompt"
    );
}

/// The comparison is not creature-specific: the same clause grammar carries the
/// `controls more ⟨type⟩` axis, so Oath of Lieges counts LANDS. Asserted from
/// both sides, because a filter that silently matched everything would satisfy
/// the positive case alone.
#[test]
fn oath_of_lieges_counts_lands_not_creatures() {
    // Negative side: a creature is not a land, so nothing satisfies the clause.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Oath of Lieges", OATH_OF_LIEGES);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    let mut runner = scenario.build();
    let (player, prompt) = next_upkeep_prompt(&mut runner);
    assert_eq!(player, P1);
    assert!(
        prompt.is_none(),
        "a creature must not satisfy Oath of Lieges's LAND comparison, got {prompt:?}"
    );

    // Positive side: one more land on the opponent's side, and the upkeep player
    // announces that opponent and is then offered the search.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Oath of Lieges", OATH_OF_LIEGES);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let (player, prompt) = next_upkeep_prompt(&mut runner);
    assert_eq!(player, P1);
    announce_sole_target(&mut runner, prompt, P1, &[TargetRef::Player(P0)]);
    assert!(
        matches!(
            prompt_in_current_upkeep(&mut runner),
            Some(WaitingFor::OptionalEffectChoice { player, .. }) if player == P1
        ),
        "the opponent is ahead on lands, so P1's Oath trigger must reach its 'may'"
    );
}
