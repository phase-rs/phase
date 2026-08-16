//! Production-path integration coverage for the WHO villainous-choice
//! scoped-player parser/continuation fix (upstream PR #3183).
//!
//! These tests drive the REAL parse → cast/resolution pipeline — they parse the
//! shipping Oracle text through `add_creature_from_oracle` /
//! `add_spell_to_hand_from_oracle` (the production parser the PR modifies) and
//! resolve via `apply`, answering each `WaitingFor` with a real `GameAction`.
//! They do NOT hand-build a `ResolvedAbility`; the `choose_one_of.rs` unit tests
//! cover that layer. The discriminating seam here is:
//!
//!   parsed Oracle → `Effect::Choose(Opponent)` → `WaitingFor::NamedChoice`
//!   → chosen-player recorded into the continuation's `chosen_players`
//!   → drain → `Effect::ChooseOneOf { chooser }` →
//!   `WaitingFor::ChooseOneOfBranch { player }`
//!
//! and the assertion in both tests is WHICH player is prompted for the
//! villainous choice:
//!
//!   1. The Master, Gallifrey's End — the controller first chooses the
//!      highest-life opponent (`Choose(Opponent)` with a most-life
//!      restriction); the parser lowers the villainous chooser to
//!      `PlayerFilter::ChosenPlayer { index: 0 }`, so the engine must prompt
//!      THAT chosen opponent, not the controller.
//!   2. This Is How It Ends — targets a creature, then its OWNER faces the
//!      villainous choice; the parser lowers the chooser to
//!      `PlayerFilter::ParentObjectTargetOwner`, so the engine must prompt the
//!      target creature's owner.
//!
//! Fail-on-revert: revert the PR's parser change and the chooser lowers back to
//! `PlayerFilter::Controller` (or the villainous clause drops entirely), so the
//! `ChooseOneOfBranch { player }` is the controller (P0) instead of the chosen
//! opponent / target owner — both assertions below then fail.
//!
//! CR 701.55a: "[A player] faces a villainous choice — [A], or [B]" means that
//! player chooses A or B, then performs the chosen option.
//! CR 701.55d: If multiple players face a villainous choice, the process runs for
//! each in APNAP order.
//! CR 608.2c: Apply the rules of English to the whole text — "That player" refers
//! to the opponent chosen by the preceding "choose an opponent" instruction.
//! CR 108.3: The owner of a card is the player who started the game with it — the
//! anchor for "Target creature's owner … faces a villainous choice".

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// The Master, Gallifrey's End — the shipping dies-trigger Oracle text. The
/// villainous chooser is the chosen most-life opponent (`ChosenPlayer { 0 }`).
const THE_MASTER_ORACLE: &str = "Make Them Pay — Whenever a nontoken artifact \
     creature you control dies, you may exile it. If you do, choose an opponent \
     with the most life among your opponents. That player faces a villainous \
     choice — They lose 4 life, or you create a token that's a copy of that card.";

/// This Is How It Ends — the shipping sorcery Oracle text. The villainous
/// chooser is the target creature's owner (`ParentObjectTargetOwner`).
const THIS_IS_HOW_IT_ENDS_ORACLE: &str = "Target creature's owner shuffles it \
     into their library, then faces a villainous choice — They lose 5 life, or \
     they shuffle another creature they own into their library.";

/// Drive the engine forward until it pauses on a player-facing choice
/// (`NamedChoice` / `ChooseOneOfBranch`) or a terminal state. Passes priority
/// through empty windows, declares no attackers/blockers so the turn rolls, and
/// accepts any optional ("you may exile it") effect along the way so the
/// dependent `Choose(Opponent)` / villainous chain proceeds (CR 608.2d).
fn advance_to_choice(runner: &mut GameRunner) {
    for _ in 0..240 {
        match &runner.state().waiting_for {
            WaitingFor::NamedChoice { .. } | WaitingFor::ChooseOneOfBranch { .. } => return,
            WaitingFor::OptionalEffectChoice { .. } => {
                if runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// CR 608.2c + CR 701.55a: The controller (P0) first chooses the highest-life
/// opponent; "That player faces a villainous choice" then refers to THAT chosen
/// opponent (rules of English), not to The Master's controller.
#[test]
fn the_master_prompts_the_chosen_most_life_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Libraries so draw steps never deck anyone out before the assertion.
    for &pid in &[P0, P1, P2] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    // Distinct life totals so "the opponent with the most life among your
    // opponents" has a unique answer: P2 (40) > P1 (20). The restriction must
    // narrow the legal `Choose(Opponent)` options to P2 alone.
    scenario.with_life(P0, 30);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 40);

    // The Master on P0's battlefield, parsed from the real Oracle text.
    scenario
        .add_creature_from_oracle(P0, "The Master, Gallifrey's End", 4, 3, THE_MASTER_ORACLE)
        .id();

    // A nontoken artifact creature P0 controls, pre-marked with lethal damage
    // (2 marked on a 2/2). Its death — via the real state-based-action destroy
    // on the first `apply` — triggers Make Them Pay through `process_triggers`.
    let victim = scenario
        .add_creature(P0, "Bot", 2, 2)
        .with_damage_marked(2)
        .id();

    let mut runner = scenario.build();
    // Make the victim an artifact creature (nontoken by construction) so it
    // matches the trigger's "nontoken artifact creature you control" filter.
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&victim)
            .expect("victim exists");
        if !obj.card_types.core_types.contains(&CoreType::Artifact) {
            obj.card_types.core_types.push(CoreType::Artifact);
        }
        obj.base_card_types = obj.card_types.clone();
    }

    // Advance to the first resolution choice. The state-based-action destroy
    // fires the dies-trigger; Make Them Pay is optional ("you may exile it"),
    // and `advance_to_choice` accepts it, then the `Choose(Opponent)` surfaces.
    advance_to_choice(&mut runner);

    // CR 704.5g: lethal-damage SBA destroyed the artifact creature, firing Make
    // Them Pay; the accepted optional ("you may exile it. If you do, …") then
    // exiled it. Having left the battlefield, the villainous chain is underway.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "the dying artifact creature must be exiled by the accepted 'may exile it'"
    );

    // First pause: `Choose(Opponent)` restricted to the most-life opponent — the
    // legal options must be exactly P2 (the controller picks the chosen player).
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player, options, ..
        } => {
            assert_eq!(*player, P0, "the controller (P0) chooses the opponent");
            assert_eq!(
                options,
                &[P2.0.to_string()],
                "the most-life restriction must narrow the choice to P2 alone; got {options:?}"
            );
        }
        other => panic!("expected the Choose(Opponent) NamedChoice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: P2.0.to_string(),
        })
        .expect("choosing the most-life opponent (P2) must succeed");

    // Second pause: the villainous choice — it MUST be faced by the chosen
    // opponent (P2), not by The Master's controller (P0). Pre-fix the chooser
    // lowered to `Controller`, so this prompted P0.
    advance_to_choice(&mut runner);
    let lose_life_index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            player, branches, ..
        } => {
            assert_eq!(
                *player, P2,
                "the chosen most-life opponent (P2) faces the villainous choice, \
                 not The Master's controller (P0)"
            );
            assert_eq!(branches.len(), 2, "lose-4-life or copy-token");
            branches
                .iter()
                .position(|b| matches!(&*b.effect, Effect::LoseLife { .. }))
                .expect("a 'They lose 4 life' branch must exist")
        }
        other => panic!("expected the villainous ChooseOneOfBranch scoped to P2, got {other:?}"),
    };

    // End-to-end: resolving the "They lose 4 life" branch must drain 4 life from
    // the chosen opponent (P2: 40 → 36), proving the scoped branch resolves
    // against the prompted player, not the controller (CR 701.55a).
    let p2_life_before = runner.life(P2);
    runner
        .act(GameAction::ChooseBranch {
            index: lose_life_index,
        })
        .expect("resolving the lose-life branch must succeed");
    assert_eq!(
        runner.life(P2),
        p2_life_before - 4,
        "the chosen opponent (P2) must lose 4 life, not The Master's controller"
    );
    assert_eq!(
        runner.life(P0),
        30,
        "The Master's controller (P0) must not lose life from the villainous choice"
    );
}

/// CR 108.3 + CR 701.55a: This Is How It Ends targets a creature; its OWNER
/// faces the villainous choice after the shuffle clause — not the spell's
/// caster.
#[test]
fn this_is_how_it_ends_prompts_the_target_creatures_owner() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    // The sorcery in P0's hand, parsed from the real Oracle text.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "This Is How It Ends", false, THIS_IS_HOW_IT_ENDS_ORACLE)
        .id();

    // The target creature is OWNED by the opponent (P1). After the shuffle, P1
    // (its owner) must face the villainous choice.
    let target_creature = scenario.add_creature(P1, "P1 Creature", 3, 3).id();

    let mut runner = scenario.build();

    // Cast the sorcery at the opponent's creature through the real pipeline.
    runner.cast(spell).target_object(target_creature).commit();

    // Drive resolution to the villainous choice.
    advance_to_choice(&mut runner);

    // The villainous choice MUST be faced by the target creature's owner (P1),
    // not by the spell's caster (P0). Pre-fix the villainous clause was dropped
    // (the parse produced a bare Shuffle), so this branch never surfaced.
    let lose_life_index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            player, branches, ..
        } => {
            assert_eq!(
                *player, P1,
                "the target creature's owner (P1) faces the villainous choice, \
                 not the spell's caster (P0)"
            );
            assert_eq!(branches.len(), 2, "lose-5-life or shuffle-another-creature");
            branches
                .iter()
                .position(|b| matches!(&*b.effect, Effect::LoseLife { .. }))
                .expect("a 'They lose 5 life' branch must exist")
        }
        other => panic!("expected the villainous ChooseOneOfBranch scoped to P1, got {other:?}"),
    };

    // End-to-end: resolving the "They lose 5 life" branch must drain 5 life from
    // the target creature's owner (P1: 20 → 15), proving the scoped branch
    // resolves against the prompted owner, not the caster (CR 701.55a).
    let p0_life_before = runner.life(P0);
    let p1_life_before = runner.life(P1);
    runner
        .act(GameAction::ChooseBranch {
            index: lose_life_index,
        })
        .expect("resolving the lose-life branch must succeed");
    assert_eq!(
        runner.life(P1),
        p1_life_before - 5,
        "the target creature's owner (P1) must lose 5 life"
    );
    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "the spell's caster (P0) must not lose life from the villainous choice"
    );
}
