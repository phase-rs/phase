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
use engine::types::ability::{ChoiceType, ControllerRef, Effect, TargetFilter, TypedFilter};
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

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

const INTELLECTUAL_OFFERING: &str = "Choose an opponent. You and that player each draw three cards.\nChoose an \
     opponent. Untap all nonland permanents you control and all nonland permanents that player controls.";

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.owner == player && o.zone == engine::types::zones::Zone::Hand)
        .count()
}

/// Runtime counterpart to `intellectual_offering_second_draw_binds_to_chosen_opponent`
/// below: drives the real cast/resolution pipeline (not just the parsed AST)
/// so the fix is proven all the way through `game/effects/draw.rs`'s
/// `ChosenPlayer` resolution (`game/effects/mod.rs`'s `resolve_player_for_context_ref`),
/// not just the parser. An AST-only assertion would stay green even if the
/// resolver drew for the wrong player.
#[test]
fn intellectual_offering_draws_three_for_caster_and_chosen_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [
            floating_mana(ManaType::Colorless, 4),
            floating_mana(ManaType::Blue, 1),
        ]
        .concat(),
    );
    // Seed both libraries well past the three cards each side draws.
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island", "Island", "Island", "Island"]);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Intellectual Offering", true, INTELLECTUAL_OFFERING)
        .id();

    let mut runner = scenario.build();
    // The spell itself occupies P0's hand until it resolves off the stack;
    // measure the draw delta from the post-cast baseline, not pre-cast.
    let hand_before_p1 = hand_count(&runner, P1);

    runner.cast(spell).resolve();
    let hand_before_p0 = hand_count(&runner, P0);

    // First "Choose an opponent." (fronting the twin three-card draw).
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

    assert_eq!(
        hand_count(&runner, P0) - hand_before_p0,
        3,
        "the caster must draw three cards"
    );
    assert_eq!(
        hand_count(&runner, P1) - hand_before_p1,
        3,
        "the chosen opponent must draw three cards — this is the runtime proof \
         that ChosenPlayer{{index: 0}} (not the unrelated ScopedPlayer default) \
         resolves the second Draw's recipient"
    );
}

/// Intellectual Offering shares Benevolent Offering's "Choose an opponent.
/// You and that player each <body>." shape, so it exercises the SAME
/// `try_parse_compound_subject_each` fix: "that player" must rebind to the
/// resolution-scoped chosen player (`ChosenPlayer { index }`), not the
/// unrelated vote/fan-out `ScopedPlayer` axis. Locks in that the whole
/// "Offering" cycle — not just Benevolent Offering — benefits.
///
/// AST-shape companion to `intellectual_offering_draws_three_for_caster_and_chosen_opponent`
/// above; kept as a SHAPE test (see the `card-test` skill) because it pins
/// the exact parser output distinct from the runtime draw-count proof.
#[test]
fn intellectual_offering_second_draw_binds_to_chosen_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Intellectual Offering", true, INTELLECTUAL_OFFERING)
        .id();
    let runner = scenario.build();
    let ability = &runner.state().objects.get(&spell).unwrap().abilities[0];
    assert!(
        matches!(
            ability.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Opponent { .. },
                ..
            }
        ),
        "head must be Choose(Opponent), got {:?}",
        ability.effect
    );
    let first_draw = ability.sub_ability.as_ref().expect("first Draw node");
    assert!(
        matches!(
            first_draw.effect.as_ref(),
            Effect::Draw {
                target: TargetFilter::OriginalController,
                ..
            }
        ),
        "the caster's draw must target OriginalController, got {:?}",
        first_draw.effect
    );
    let second_draw = first_draw.sub_ability.as_ref().expect("second Draw node");
    assert!(
        matches!(
            second_draw.effect.as_ref(),
            Effect::Draw {
                target: TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::ChosenPlayer { index: 0 }),
                    ..
                }),
                ..
            }
        ),
        "the chosen opponent's draw must bind to ChosenPlayer{{index: 0}}, not ScopedPlayer, got {:?}",
        second_draw.effect
    );
}

/// Regression guard for the fix above: `inject_subject_target`'s new
/// `GainLife` arm must NOT rebind the recipient when the detected subject
/// isn't a genuine player reference. Angel of Destiny's "you and that player
/// each gain that much life" is a compound subject that `GainLife` doesn't
/// support in `rewrite_recipient_on_link` (Token/Draw/Discard/Mill/Pump/
/// GenericEffect only), so it falls through to a non-player-denoting subject
/// filter; the safe no-subject `Controller` default must survive rather than
/// being corrupted into an incoherent recipient. This is a pre-existing gap
/// (the damaged player still doesn't gain life) — not fixed here — but the
/// `player` field must stay a well-defined `Controller`, not silently swap to
/// something meaningless.
#[test]
fn angel_of_destiny_combat_damage_gain_life_keeps_well_defined_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_from_oracle(
            P0,
            "Angel of Destiny",
            3,
            4,
            "Flying, double strike\nWhenever a creature you control deals combat damage to a player, you and that player each gain that much life.\nAt the beginning of your end step, if you have at least 15 life more than your starting life total, each player this creature attacked this turn loses the game.",
        )
        .id();
    let runner = scenario.build();
    let obj = runner.state().objects.get(&creature).unwrap();
    let damage_trigger = obj
        .trigger_definitions
        .iter_unchecked()
        .find(|entry| matches!(entry.definition.mode, TriggerMode::DamageDone))
        .expect("Angel of Destiny must have a DamageDone trigger");
    let execute = damage_trigger
        .definition
        .execute
        .as_ref()
        .expect("DamageDone trigger must have an execute body");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::GainLife {
                player: TargetFilter::Controller,
                ..
            }
        ),
        "GainLife.player must stay the well-defined Controller default, not an \
         unresolved compound-subject filter like Any, got {:?}",
        execute.effect
    );
}
