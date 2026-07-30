//! Regression for issue #2940: Krark, the Thumbless must run the return-to-hand
//! branch on a lost flip and the copy branch on a won flip — not the reverse.
//!
//! https://github.com/phase-rs/phase/issues/2940

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{CopyRetargetPermission, Effect};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KRARK: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
    If you lose the flip, return that spell to its owner's hand. \
    If you win the flip, copy that spell, and you may choose new targets for the copy.";

const DRAW_SPELL: &str = "Draw a card.";

fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn library_len(
    state: &engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .unwrap_or(0)
}

fn setup_krark_and_draw_spell(seed: u64) -> (GameScenario, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Krark, the Thumbless", 2, 2, KRARK);
    for i in 0..5 {
        scenario.add_spell_to_library_top(P0, &format!("Library {i}"), true);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draw Spell", true, DRAW_SPELL)
        .id();
    scenario.with_mana_pool(P0, floating_colorless(10));
    (scenario, spell)
}

#[test]
fn krark_oracle_text_trigger_branches_match_direct_parse() {
    let parsed = parse_oracle_text(
        KRARK,
        "Krark, the Thumbless",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Krark should parse a trigger");
    let execute = trigger
        .execute
        .as_ref()
        .expect("trigger should have execute");
    let Effect::FlipCoin {
        win_effect,
        lose_effect,
        ..
    } = execute.effect.as_ref()
    else {
        panic!("expected FlipCoin, got {:?}", execute.effect);
    };
    let win = win_effect.as_ref().expect("win branch");
    let lose = lose_effect.as_ref().expect("lose branch");
    assert!(
        matches!(win.effect.as_ref(), Effect::CopySpell { .. }),
        "win branch should be CopySpell, got {:?}",
        win.effect
    );
    assert!(
        matches!(
            lose.effect.as_ref(),
            Effect::Bounce { .. }
                | Effect::ChangeZone {
                    destination: Zone::Hand,
                    ..
                }
        ),
        "lose branch should bounce, got {:?}",
        lose.effect
    );
    if let Effect::CopySpell { retarget, .. } = win.effect.as_ref() {
        assert_eq!(
            *retarget,
            CopyRetargetPermission::MayChooseNewTargets,
            "comma-and retarget clause must patch the win-branch CopySpell"
        );
    }
}

#[test]
fn krark_battlefield_trigger_has_correct_flip_branches() {
    let (scenario, _) = setup_krark_and_draw_spell(0);
    let runner = scenario.build();
    let krark_id = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.name.contains("Krark"))
        })
        .expect("Krark on battlefield");
    let execute = &runner.state().objects[&krark_id].trigger_definitions[0]
        .definition
        .execute
        .as_ref()
        .expect("execute");
    let Effect::FlipCoin {
        win_effect,
        lose_effect,
        ..
    } = execute.effect.as_ref()
    else {
        panic!("expected FlipCoin, got {:?}", execute.effect);
    };
    assert!(matches!(
        win_effect.as_ref().unwrap().effect.as_ref(),
        Effect::CopySpell { .. }
    ));
    assert!(matches!(
        lose_effect.as_ref().unwrap().effect.as_ref(),
        Effect::Bounce { .. }
            | Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            }
    ));
}

/// CR 705 + CR 707.10c: Win branch copies (same targets) and both spells resolve;
/// lose branch bounces the spell before it resolves.
#[test]
fn krark_flip_branches_match_coin_outcome_in_cast_pipeline() {
    let mut saw_win = false;
    let mut saw_lose = false;

    for seed in 0..64 {
        let (scenario, spell) = setup_krark_and_draw_spell(seed);
        let mut runner = scenario.build();
        let lib_before = library_len(runner.state(), P0);
        let outcome = runner.cast(spell).resolve();
        let lib_after = library_len(outcome.state(), P0);
        let spell_zone = outcome.zone_of(spell);

        if spell_zone == Zone::Hand {
            assert_eq!(
                lib_before, lib_after,
                "seed {seed}: lose branch must bounce before Draw resolves"
            );
            assert!(
                outcome.stack_names().is_empty(),
                "seed {seed}: bounced spell must leave the stack"
            );
            saw_lose = true;
        } else {
            assert_eq!(
                lib_before.saturating_sub(lib_after),
                2,
                "seed {seed}: win branch must resolve original + copy (each draws)"
            );
            assert!(
                outcome.stack_names().is_empty(),
                "seed {seed}: stack should empty after both spells resolve"
            );
            saw_win = true;
        }
    }

    assert!(saw_win, "scan must observe at least one won flip");
    assert!(saw_lose, "scan must observe at least one lost flip");
}
