//! CR 608.2c + CR 608.2d: "choose a different opponent/player" offers only
//! players the resolving ability has not already chosen during this
//! resolution. The reference set belongs to the resolving ability: it survives
//! that ability's `repeat_for` iterations and interactive pauses, is fed by its
//! own interactive and random answers, and is never fed by another object's
//! choice made mid-resolution — the "As ~ enters, choose a player" replacement
//! of a permanent the ability puts onto the battlefield (CR 614.1c,
//! CR 614.12a; that answer is the entering object's linked value, CR 607.2d).
//! When no eligible player remains, the choice does nothing (CR 609.3).
//!
//! Within one repetition (or after one Choose), "that player" names the player
//! chosen by that Choose (CR 608.2c) — never an earlier repetition's choice nor
//! a foreign choice.
//!
//! The repeated distinct-choice instrument is a hand-built ability definition
//! driven through the production cast/resolve entry: at PHASE_BASE no card's
//! parse places a `DistinctFromPriorChoices` choice under a `repeat_for`
//! (measured census), so no printed card can carry the shape yet.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ChoiceType, ChosenAttribute, ControllerRef, Effect,
    PlayerChoiceDistinctness, QuantityExpr, SubAbilityLink, TargetFilter, TargetSelectionMode,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{NamedChoiceSource, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::{
    ResolutionFrame, ResolutionStateWire, RESOLUTION_STATE_WIRE_VERSION,
};
use engine::types::zones::Zone;

const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// True-Name Nemesis, verbatim Oracle text.
const TRUE_NAME_NEMESIS: &str =
    "As this creature enters, choose a player.\nThis creature has protection from the chosen player.";

/// The base-parseable dependent instruction pair the instrument repeats.
const CHOOSE_THEN_LOSE: &str = "Choose an opponent. That player loses 1 life.";

fn sorcery_abilities(oracle: &str) -> Vec<AbilityDefinition> {
    parse_oracle_text(oracle, "Instrument", &[], &["Sorcery".to_string()], &[]).abilities
}

/// `Choose{Opponent, distinctness}` repeated three times, each repetition's
/// dependent "That player loses 1 life." parked as a `ContinuationStep`.
fn repeated_choice_def(distinctness: PlayerChoiceDistinctness) -> AbilityDefinition {
    let parsed = sorcery_abilities(CHOOSE_THEN_LOSE)
        .into_iter()
        .next()
        .expect("choose-then-lose parses to one spell ability");
    let mut lose_life = *parsed
        .sub_ability
        .expect("the dependent LoseLife node follows the Choose");
    assert!(
        matches!(*lose_life.effect, Effect::LoseLife { .. }),
        "reach-guard: the dependent node is LoseLife, got {:?}",
        lose_life.effect
    );
    lose_life.sub_link = SubAbilityLink::ContinuationStep;
    let mut def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type: ChoiceType::Opponent {
                restriction: None,
                distinctness,
            },
            persist: false,
            selection: TargetSelectionMode::Chosen,
        },
    );
    def.repeat_for = Some(QuantityExpr::Fixed { value: 3 });
    def.sub_ability = Some(Box::new(lose_life));
    def
}

fn board_with_spell(players: u8, def: AbilityDefinition) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(players, 608);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Distinct Instrument", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(def)
        .id();
    (scenario.build(), spell)
}

struct Prompt {
    player: PlayerId,
    choice_type: ChoiceType,
    options: Vec<String>,
    source: Option<NamedChoiceSource>,
}

fn prompt(runner: &GameRunner) -> Option<Prompt> {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            source,
            ..
        } => Some(Prompt {
            player: *player,
            choice_type: choice_type.clone(),
            options: options.clone(),
            source: source.clone(),
        }),
        _ => None,
    }
}

fn option_set(players: &[PlayerId]) -> Vec<String> {
    let mut set: Vec<String> = players.iter().map(|p| p.0.to_string()).collect();
    set.sort();
    set
}

fn sorted(mut options: Vec<String>) -> Vec<String> {
    options.sort();
    options
}

fn answer(runner: &mut GameRunner, player: PlayerId) {
    runner
        .act(GameAction::ChooseOption {
            choice: player.0.to_string(),
        })
        .expect("answering a legal player must succeed");
}

fn lives(runner: &GameRunner) -> Vec<(PlayerId, i32)> {
    runner
        .state()
        .players
        .iter()
        .map(|p| (p.id, p.life))
        .collect()
}

fn lost(before: &[(PlayerId, i32)], runner: &GameRunner, player: PlayerId) -> i32 {
    let start = before
        .iter()
        .find(|(id, _)| *id == player)
        .map(|(_, life)| *life)
        .expect("player present before");
    start - runner.life(player)
}

/// The three-prompt walk of the repeated instrument: asserts each prompt is
/// the caster's opponent choice (reach-guard), checks its option set, then
/// answers it.
fn expect_opponent_prompt(runner: &GameRunner, ordinal: usize) -> Prompt {
    let p = prompt(runner).unwrap_or_else(|| {
        panic!(
            "reach-guard: prompt {ordinal} must be a NamedChoice, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        p.player, P0,
        "reach-guard: prompt {ordinal} is the caster's choice"
    );
    assert!(
        matches!(p.choice_type, ChoiceType::Opponent { .. }),
        "reach-guard: prompt {ordinal} is an opponent choice, got {:?}",
        p.choice_type
    );
    p
}

/// V2.4a (C2.1 instrument, revert-failing): a `DistinctFromPriorChoices`
/// choice re-run by the repeat driver never re-offers an earlier iteration's
/// pick across the NamedChoice pause, and each iteration's "that player"
/// names that iteration's own pick (CR 608.2c + CR 608.2d).
#[test]
fn repeated_distinct_choice_never_reoffers_an_earlier_iterations_pick() {
    let (mut runner, spell) = board_with_spell(
        4,
        repeated_choice_def(PlayerChoiceDistinctness::DistinctFromPriorChoices),
    );
    let before = lives(&runner);
    runner.cast(spell).resolve();

    let first = expect_opponent_prompt(&runner, 1);
    assert_eq!(sorted(first.options), option_set(&[P1, P2, P3]));
    answer(&mut runner, P2);

    let second = expect_opponent_prompt(&runner, 2);
    assert_eq!(
        sorted(second.options.clone()),
        option_set(&[P1, P3]),
        "CR 608.2c + CR 608.2d: the second iteration must not re-offer P2, chosen in the first"
    );
    assert!(
        runner
            .act(GameAction::ChooseOption {
                choice: P2.0.to_string(),
            })
            .is_err(),
        "CR 608.2d: re-submitting the earlier pick is an illegal option"
    );
    let still_pending = expect_opponent_prompt(&runner, 2);
    assert_eq!(sorted(still_pending.options), sorted(second.options));
    answer(&mut runner, P3);

    let third = expect_opponent_prompt(&runner, 3);
    assert_eq!(sorted(third.options), option_set(&[P1]));
    answer(&mut runner, P1);

    assert!(
        prompt(&runner).is_none(),
        "exactly three prompts: the loop ends after its third iteration"
    );
    // CR 608.2c: each iteration's "that player" is that iteration's own pick.
    for opponent in [P1, P2, P3] {
        assert_eq!(
            lost(&before, &runner, opponent),
            1,
            "P{} chosen once, loses exactly 1 life",
            opponent.0
        );
    }
    assert_eq!(lost(&before, &runner, P0), 0);
}

/// V2.4b (sibling): an `Independent` repeated choice never consults the
/// reference set — every prompt offers every opponent, and the same opponent
/// may be chosen again (the Offering-cycle ruling).
#[test]
fn repeated_independent_choice_offers_every_opponent_each_iteration() {
    let (mut runner, spell) = board_with_spell(
        4,
        repeated_choice_def(PlayerChoiceDistinctness::Independent),
    );
    let before = lives(&runner);
    runner.cast(spell).resolve();
    for (ordinal, pick) in [(1, P2), (2, P2), (3, P1)] {
        let p = expect_opponent_prompt(&runner, ordinal);
        assert_eq!(sorted(p.options), option_set(&[P1, P2, P3]));
        answer(&mut runner, pick);
    }
    assert!(prompt(&runner).is_none(), "exactly three prompts");
    assert_eq!(
        lost(&before, &runner, P2),
        2,
        "P2 chosen twice loses 2 life"
    );
    assert_eq!(lost(&before, &runner, P1), 1);
    assert_eq!(lost(&before, &runner, P3), 0);
}

/// V2.4c (CR 609.3): with fewer eligible opponents than iterations, the
/// exhausted choice raises no prompt and its dependent instruction does
/// nothing.
#[test]
fn repeated_distinct_choice_with_no_eligible_opponent_does_nothing() {
    let (mut runner, spell) = board_with_spell(
        3,
        repeated_choice_def(PlayerChoiceDistinctness::DistinctFromPriorChoices),
    );
    let before = lives(&runner);
    runner.cast(spell).resolve();
    let first = expect_opponent_prompt(&runner, 1);
    assert_eq!(sorted(first.options), option_set(&[P1, P2]));
    answer(&mut runner, P1);
    let second = expect_opponent_prompt(&runner, 2);
    assert_eq!(sorted(second.options), option_set(&[P2]));
    answer(&mut runner, P2);
    assert!(
        prompt(&runner).is_none(),
        "CR 609.3: the third iteration has no eligible opponent and raises no prompt"
    );
    assert_eq!(lost(&before, &runner, P1), 1);
    assert_eq!(lost(&before, &runner, P2), 1);
    assert_eq!(lost(&before, &runner, P0), 0);
}

/// V2.7b: the reference set rides the persisted resolution state — a wire
/// round trip while the repeated choice is paused on its second prompt keeps
/// the earlier pick excluded.
#[test]
fn reference_set_survives_a_mid_pause_wire_round_trip() {
    let (mut runner, spell) = board_with_spell(
        4,
        repeated_choice_def(PlayerChoiceDistinctness::DistinctFromPriorChoices),
    );
    runner.cast(spell).resolve();
    expect_opponent_prompt(&runner, 1);
    answer(&mut runner, P2);
    expect_opponent_prompt(&runner, 2);

    let wire = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("paused resolution serializes through the current wire");
    assert_eq!(
        wire["resolution_state_version"],
        RESOLUTION_STATE_WIRE_VERSION
    );
    let restored: ResolutionStateWire =
        serde_json::from_value(wire).expect("paused resolution round-trips");
    *runner.state_mut() = restored.into_game_state();
    assert!(
        runner
            .state()
            .resolution_stack
            .iter()
            .any(|frame| matches!(frame, ResolutionFrame::RepeatFor(_))),
        "reach-guard: the repeat frame survives the restore"
    );

    let second = expect_opponent_prompt(&runner, 2);
    assert_eq!(
        sorted(second.options),
        option_set(&[P1, P3]),
        "CR 608.2c + CR 608.2d: the restored prompt still excludes the first pick"
    );
    answer(&mut runner, P3);
    let third = expect_opponent_prompt(&runner, 3);
    assert_eq!(
        sorted(third.options),
        option_set(&[P1]),
        "the restored loop's next iteration excludes both earlier picks"
    );
}

/// "Choose a player at random." then "Choose a second player to draw a
/// card.", the second choice linked as a `ContinuationStep` of the first.
fn random_then_distinct_def() -> AbilityDefinition {
    let mut def =
        sorcery_abilities("Choose a player at random. Choose a second player to draw a card.")
            .into_iter()
            .next()
            .expect("random-then-distinct parses to one spell ability");
    assert!(
        matches!(
            &*def.effect,
            Effect::Choose {
                choice_type: ChoiceType::Player {
                    distinctness: PlayerChoiceDistinctness::Independent
                },
                selection: TargetSelectionMode::Random,
                ..
            }
        ),
        "reach-guard: the head is a random player choice, got {:?}",
        def.effect
    );
    let second = def.sub_ability.as_mut().expect("second choice");
    assert!(
        matches!(
            &*second.effect,
            Effect::Choose {
                choice_type: ChoiceType::Player {
                    distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices
                },
                selection: TargetSelectionMode::Chosen,
                ..
            }
        ),
        "reach-guard: the second choice is distinct, got {:?}",
        second.effect
    );
    assert!(
        second
            .sub_ability
            .as_ref()
            .is_some_and(|draw| matches!(&*draw.effect, Effect::Draw { .. })),
        "reach-guard: the second chosen player draws"
    );
    second.sub_link = SubAbilityLink::ContinuationStep;
    def
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// V2.5b (integration): the random answer site feeds the reference set —
/// the following interactive `DistinctFromPriorChoices` prompt excludes the
/// game-selected player, and "the second player" draws.
/// Base-green (PHASE_BASE read the anaphor binding, which also held the random pick); it discriminates the reference-set record at the candidate (mutation-checked), not a revert to base.
#[test]
fn random_choice_is_excluded_from_a_following_distinct_choice() {
    let mut scenario = GameScenario::new_n_player(3, 701);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Card A", "Card B"]);
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Random Then Distinct", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(random_then_distinct_def())
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();

    let p = prompt(&runner).expect("reach-guard: one interactive prompt");
    assert_eq!(p.player, P0);
    assert!(matches!(p.choice_type, ChoiceType::Player { .. }));
    let random_pick = runner
        .state()
        .active_ability_continuation()
        .and_then(|pending| pending.chain.chosen_players.first().copied())
        .expect("the random pick is bound before the interactive prompt");
    assert_eq!(p.options.len(), 2, "{:?}", p.options);
    assert!(
        !p.options.contains(&random_pick.0.to_string()),
        "CR 608.2c + CR 608.2d: the game-selected P{} is a prior choice of this ability; options {:?}",
        random_pick.0,
        p.options
    );

    let interactive_pick = PlayerId(p.options[0].parse().expect("player id option"));
    let hands_before: Vec<usize> = [P0, P1, P2]
        .iter()
        .map(|&pl| hand_size(&runner, pl))
        .collect();
    answer(&mut runner, interactive_pick);
    assert!(prompt(&runner).is_none(), "exactly one interactive prompt");
    for (index, player) in [P0, P1, P2].into_iter().enumerate() {
        let drew = hand_size(&runner, player) - hands_before[index];
        let expected = usize::from(player == interactive_pick);
        assert_eq!(
            drew, expected,
            "CR 608.2c: only the second chosen player (P{}) draws",
            interactive_pick.0
        );
    }
}

const FOREIGN_FIRST: &str = "Return target creature card from your graveyard to the \
     battlefield. Choose an opponent. That player loses 2 life.";

/// A 4-player board: True-Name Nemesis in P0's graveyard, `oracle` as a {0}
/// sorcery in P0's hand.
fn nemesis_board(oracle: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 614);
    scenario.at_phase(Phase::PreCombatMain);
    let nemesis = scenario
        .add_creature_to_graveyard(P0, "True-Name Nemesis", 3, 1)
        .from_oracle_text(TRUE_NAME_NEMESIS)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Foreign Choice Instrument", false, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    (scenario.build(), spell, nemesis)
}

/// The foreign prompt: True-Name Nemesis's "As this creature enters, choose
/// a player", bound to the entering Nemesis.
fn expect_nemesis_prompt(runner: &GameRunner, nemesis: ObjectId) -> Prompt {
    let p = prompt(runner).expect("reach-guard: the as-enters choice raised a prompt");
    assert!(matches!(p.choice_type, ChoiceType::Player { .. }));
    assert_eq!(
        p.source
            .as_ref()
            .map(|source| source.prompt.identity.reference.object_id),
        Some(nemesis),
        "reach-guard: the prompt is bound to the entering Nemesis"
    );
    p
}

fn assert_nemesis_chose(runner: &GameRunner, nemesis: ObjectId, player: PlayerId) {
    let obj = &runner.state().objects[&nemesis];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        obj.chosen_attributes
            .contains(&ChosenAttribute::Player(player)),
        "reach-guard: Nemesis persisted its own chosen player; got {:?}",
        obj.chosen_attributes
    );
}

fn chain_effects(oracle: &str) -> Vec<Effect> {
    let def = sorcery_abilities(oracle)
        .into_iter()
        .next()
        .expect("one spell ability");
    let mut effects = vec![(*def.effect).clone()];
    let mut cursor = def.sub_ability.as_deref();
    while let Some(node) = cursor {
        effects.push((*node.effect).clone());
        cursor = node.sub_ability.as_deref();
    }
    effects
}

fn loses_life_for_chosen(effect: &Effect, index: u8) -> bool {
    matches!(effect, Effect::LoseLife {
        target: Some(TargetFilter::Typed(typed)), ..
    } if typed.controller == Some(ControllerRef::ChosenPlayer { index }))
}

/// V2.6a (C2.6; A2.4 hostile): an entering permanent's "As ~ enters, choose
/// a player" answered while the spell resolves is that permanent's linked
/// choice (CR 614.12a, CR 607.2d) — it does not become the spell's chosen
/// player, so "that player" names the spell's own pick.
#[test]
fn foreign_as_enters_choice_does_not_shift_the_spells_chosen_player() {
    let effects = chain_effects(FOREIGN_FIRST);
    assert!(
        matches!(effects[0], Effect::ChangeZone { .. })
            && matches!(
                effects[1],
                Effect::Choose {
                    choice_type: ChoiceType::Opponent {
                        distinctness: PlayerChoiceDistinctness::Independent,
                        ..
                    },
                    ..
                }
            )
            && loses_life_for_chosen(&effects[2], 0),
        "parse reach-guard: {effects:?}"
    );
    let (mut runner, spell, nemesis) = nemesis_board(FOREIGN_FIRST);
    let before = lives(&runner);
    runner.cast(spell).target_objects(&[nemesis]).resolve();

    expect_nemesis_prompt(&runner, nemesis);
    answer(&mut runner, P2);
    expect_opponent_prompt(&runner, 2);
    answer(&mut runner, P3);
    assert!(prompt(&runner).is_none(), "two prompts observed");
    assert_nemesis_chose(&runner, nemesis, P2);

    assert_eq!(
        lost(&before, &runner, P3),
        2,
        "CR 608.2c: \"that player\" is the spell's own pick, P3"
    );
    assert_eq!(
        lost(&before, &runner, P2),
        0,
        "CR 614.12a + CR 607.2d: Nemesis's choice is not the spell's chosen player"
    );
}

/// V2.6a (candidate variant): the foreign answer is not one of the spell's
/// prior choices, so "a different opponent" still offers it.
#[test]
fn foreign_as_enters_choice_is_not_a_prior_choice_of_the_spell() {
    let oracle = FOREIGN_FIRST.replace("Choose an opponent.", "Choose a different opponent.");
    let (mut runner, spell, nemesis) = nemesis_board(&oracle);
    runner.cast(spell).target_objects(&[nemesis]).resolve();
    expect_nemesis_prompt(&runner, nemesis);
    answer(&mut runner, P2);
    let own = expect_opponent_prompt(&runner, 2);
    assert!(matches!(
        own.choice_type,
        ChoiceType::Opponent {
            distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices,
            ..
        }
    ));
    assert_eq!(
        sorted(own.options),
        option_set(&[P1, P2, P3]),
        "CR 614.12a + CR 607.2d: Nemesis's P2 is not excluded"
    );
}

const OWN_FOREIGN_OWN: &str = "Choose an opponent. That player loses 1 life. Return target \
     creature card from your graveyard to the battlefield. Choose a different opponent. That \
     player loses 2 life.";

/// V2.6b (C2.6): own choice, then a foreign choice inside a nested
/// replacement chain, then a distinct own choice. The reference set survives
/// the nested chain (P1 excluded), the foreign answer never enters it (P3
/// offered), and it does not shift `ChosenPlayer{1}`.
#[test]
fn reference_set_survives_a_nested_replacement_choice_and_excludes_only_own_picks() {
    let effects = chain_effects(OWN_FOREIGN_OWN);
    assert!(
        matches!(
            effects[0],
            Effect::Choose {
                choice_type: ChoiceType::Opponent {
                    distinctness: PlayerChoiceDistinctness::Independent,
                    ..
                },
                ..
            }
        ) && loses_life_for_chosen(&effects[1], 0)
            && matches!(effects[2], Effect::ChangeZone { .. })
            && matches!(
                effects[3],
                Effect::Choose {
                    choice_type: ChoiceType::Opponent {
                        distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices,
                        ..
                    },
                    ..
                }
            )
            && loses_life_for_chosen(&effects[4], 1),
        "parse reach-guard: {effects:?}"
    );
    let (mut runner, spell, nemesis) = nemesis_board(OWN_FOREIGN_OWN);
    let before = lives(&runner);
    runner.cast(spell).target_objects(&[nemesis]).resolve();

    let first = expect_opponent_prompt(&runner, 1);
    assert_eq!(sorted(first.options), option_set(&[P1, P2, P3]));
    answer(&mut runner, P1);
    expect_nemesis_prompt(&runner, nemesis);
    answer(&mut runner, P3);
    let second = expect_opponent_prompt(&runner, 3);
    assert_eq!(
        sorted(second.options),
        option_set(&[P2, P3]),
        "CR 608.2c + CR 608.2d: P1 (the spell's own pick) is excluded; P3 (Nemesis's) is not"
    );
    answer(&mut runner, P2);
    assert!(prompt(&runner).is_none(), "three prompts observed");
    assert_nemesis_chose(&runner, nemesis, P3);

    assert_eq!(lost(&before, &runner, P1), 1);
    assert_eq!(
        lost(&before, &runner, P2),
        2,
        "CR 608.2c: the second \"that player\" is the spell's second pick, P2"
    );
    assert_eq!(lost(&before, &runner, P3), 0);
}
