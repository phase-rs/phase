//! Regression (issue #6381): Benevolent Offering's two independent "Choose an
//! opponent." instructions must each accept the SAME opponent in a two-player
//! game. Official ruling: "You may choose the same opponent for each of the
//! effects, or you may choose different opponents." (Confirmed identically
//! for the "Offering" cycle: Infernal/Intellectual/Sylvan Offering.)
//!
//! Before the fix, `ChoiceType::Opponent`/`ChoiceType::Player` unconditionally
//! excluded players already chosen earlier in the same resolution (correct
//! only for Gluntch, the Bestower's ordinal-cued "choose a second/third
//! player"). In a two-player game that made the SECOND "Choose an opponent."
//! impossible — CR 609.3 turned it into a no-op, so "that player" never got
//! bound for the life-gain clause and the chosen opponent gained 0 life
//! instead of 2 life per creature they control.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChoiceType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const BENEVOLENT_OFFERING: &str = "Choose an opponent. You and that player each create three 1/1 white Spirit \
     creature tokens with flying.\nChoose an opponent. You gain 2 life for each creature you control and that \
     player gains 2 life for each creature they control.";

fn floating_mana(color: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| {
            ManaUnit::new(
                color,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            )
        })
        .collect()
}

fn player_life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .life
}

/// Assert a `NamedChoice(Opponent)` prompt is showing, that `opponent` is
/// among the legal (non-excluded) options, then answer it.
fn choose_opponent(runner: &mut GameRunner, opponent: PlayerId) {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            choice_type,
            options,
            ..
        } => {
            assert!(
                matches!(
                    choice_type,
                    ChoiceType::Opponent {
                        restriction: None,
                        ..
                    }
                ),
                "expected an unrestricted opponent choice, got {choice_type:?}"
            );
            assert!(
                options.contains(&opponent.0.to_string()),
                "opponent P{} must remain a legal pick (Offering cycle ruling allows \
                 repeating an earlier choice); options={options:?}",
                opponent.0
            );
        }
        other => panic!("expected NamedChoice(Opponent), got {other:?}"),
    }
    runner
        .act(engine::types::actions::GameAction::ChooseOption {
            choice: opponent.0.to_string(),
        })
        .expect("ChooseOption(opponent) must succeed");
}

#[test]
fn benevolent_offering_allows_choosing_the_same_opponent_twice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [
            floating_mana(ManaType::Colorless, 3),
            floating_mana(ManaType::White, 1),
        ]
        .concat(),
    );

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Benevolent Offering", true, BENEVOLENT_OFFERING)
        .id();

    let mut runner = scenario.build();
    let life_before_p0 = player_life(&runner, P0);
    let life_before_p1 = player_life(&runner, P1);

    runner.cast(spell).resolve();

    // First "Choose an opponent." (fronting the twin token creation).
    choose_opponent(&mut runner, P1);
    // Second "Choose an opponent." — must offer P1 again, not exclude it.
    choose_opponent(&mut runner, P1);

    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }
        runner
            .act(engine::types::actions::GameAction::PassPriority)
            .ok();
    }

    // CR 111.7: each player controls exactly three of the created Spirit tokens.
    let p0_spirits = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P0 && o.name == "Spirit")
        .count();
    let p1_spirits = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P1 && o.name == "Spirit")
        .count();
    assert_eq!(p0_spirits, 3, "the caster must control three Spirit tokens");
    assert_eq!(
        p1_spirits, 3,
        "the chosen opponent must control three Spirit tokens"
    );

    // CR 119.3: each player gains 2 life per creature they control (their own
    // three Spirit tokens). Under the pre-fix bug, P1's gain was 0 because the
    // second Choose(Opponent) resolved as an impossible no-op.
    assert_eq!(
        player_life(&runner, P0) - life_before_p0,
        6,
        "the caster must gain 2 life per creature controlled (3 Spirits)"
    );
    assert_eq!(
        player_life(&runner, P1) - life_before_p1,
        6,
        "the chosen opponent must gain 2 life per creature controlled (3 Spirits) \
         — this is the reported defect: it read 0 before the fix"
    );
}
