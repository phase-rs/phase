//! "For each 1 life your opponents have lost this turn" / "... you gained this
//! turn" reads the per-player life history of the current turn (CR 119.3) —
//! the opponents' sum, or your own gain — not the triggering life-change event,
//! which a cast or a phase trigger has none of. Before, the cost reductions
//! (Bloodsoaked Insight, Licia, Sanguine Tribune, Rakdos, Lord of Riots)
//! counted every permanent on the battlefield, and the mana lines (Neheb, the
//! Eternal, Megatron, Tyrant) were unparsed.
//!
//! The cases create any life history by casting real setup spells through
//! `GameAction`, then read what the game actually did: the lands a spell tapped
//! to pay its total cost (CR 601.2f), the mana a trigger added, a +1/+1
//! counter, or whether the convert was offered.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

/// Casts `spell` and answers its prompts (target: P1 while legal, else the
/// first legal target) until the stack is empty again.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                let choice = legal
                    .iter()
                    .find(|t| **t == TargetRef::Player(P1))
                    .or_else(|| legal.first())
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        }
    }
    panic!("the spell never resolved back to an empty stack");
}

const BLOODSOAKED_INSIGHT_WITH_REDUCTION: &str = "This spell costs {1} less to cast for each 1 life your opponents have lost this turn.\n\
Target opponent exiles the top three cards of their library. Until the end of your next turn, you may play those cards. If you cast a spell this way, mana of any type can be spent to cast it.";

fn tapped_life_cost_lands(runner: &GameRunner, lands: &[ObjectId]) -> usize {
    lands
        .iter()
        .filter(|land| runner.state().objects[land].tapped)
        .count()
}

// Life-history records are created only by resolving spells through GameAction
// (the scenario cast driver), never by assigning their recorded values.
fn bloodsoaked_life_cost_case(
    targets: &[engine::types::PlayerId],
    next_turn: bool,
    expected_payment: usize,
) {
    bloodsoaked_life_cost_case_with(
        BLOODSOAKED_INSIGHT_WITH_REDUCTION,
        targets,
        next_turn,
        false,
        expected_payment,
    );
}

/// `first_target_dies`: the first target starts at 2 life, so the setup Shock
/// makes that opponent lose the game (CR 104.3b) before Insight is cast.
fn bloodsoaked_life_cost_case_with(
    oracle: &str,
    targets: &[engine::types::PlayerId],
    next_turn: bool,
    first_target_dies: bool,
    expected_payment: usize,
) {
    let p2 = engine::types::PlayerId(2);

    let player_count = if targets.contains(&p2) { 3 } else { 2 };
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, p2].into_iter().take(usize::from(player_count)) {
        for n in 0..8 {
            scenario.add_card_to_library_top(player, &format!("Filler {n}"));
        }
    }
    if first_target_dies {
        scenario.with_life(targets[0], 2);
    }
    let lands: Vec<_> = (0..7)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let shocks: Vec<_> = targets
        .iter()
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Shock",
                    true,
                    "Shock deals 2 damage to any target.",
                )
                // Setup spell's payment is irrelevant; isolate Insight's cost.
                .with_mana_cost(ManaCost::default())
                .id()
        })
        .collect();
    let insight = scenario
        .add_spell_to_hand_from_oracle(P0, "Bloodsoaked Insight", false, oracle)
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::BlackRed, ManaCostShard::BlackRed],
        })
        .id();
    let mut runner = scenario.build();
    for (&shock, &target) in shocks.iter().zip(targets) {
        let before = runner.life(target);
        runner.cast(shock).target_player(target).resolve();
        assert_eq!(runner.life(target), before - 2);
        assert_eq!(runner.state().objects[&shock].zone, Zone::Graveyard);
    }
    if first_target_dies {
        assert!(
            runner.state().players[usize::from(targets[0].0)].is_eliminated,
            "the first opponent must actually have left the game"
        );
    }
    for player in &runner.state().players {
        assert_eq!(
            player.life_lost_this_turn,
            2 * targets
                .iter()
                .filter(|&&target| target == player.id)
                .count() as u32,
            "the setup spell must create the per-player life-loss record"
        );
    }
    if next_turn {
        // No battlefield creatures: phase advancement cannot stop at combat
        // choices. Both libraries contain enough cards for the actual draws.
        runner.advance_to_phase(Phase::Upkeep);
        assert_eq!(runner.state().active_player, P1);
        assert!(runner
            .state()
            .players
            .iter()
            .all(|p| p.life_lost_this_turn == 0));
        runner.advance_to_phase(Phase::PreCombatMain);
        runner.advance_to_phase(Phase::Upkeep);
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, P0);
        assert_eq!(runner.state().phase, Phase::PreCombatMain);
        assert!(runner
            .state()
            .players
            .iter()
            .all(|p| p.life_lost_this_turn == 0));
        assert_eq!(
            runner.life(P1),
            18,
            "life total stays lower after history resets"
        );
    }
    assert_eq!(tapped_life_cost_lands(&runner, &lands), 0);
    cast_and_resolve(&mut runner, insight);
    assert_eq!(runner.state().objects[&insight].zone, Zone::Graveyard);
    assert_eq!(
        tapped_life_cost_lands(&runner, &lands),
        expected_payment,
        "Insight must pay its printed cost minus this turn's opponent life loss"
    );
}

#[test]
fn bloodsoaked_costs_seven_without_life_loss_despite_seven_lands() {
    bloodsoaked_life_cost_case(&[], false, 7);
}

#[test]
fn bloodsoaked_costs_five_after_opponent_loses_two_life() {
    bloodsoaked_life_cost_case(&[P1], false, 5);
}

#[test]
fn bloodsoaked_does_not_reduce_for_its_controllers_life_loss() {
    bloodsoaked_life_cost_case(&[P0], false, 7);
}

#[test]
fn bloodsoaked_life_loss_reduction_expires_at_turn_boundary() {
    bloodsoaked_life_cost_case(&[P1], true, 7);
}

/// Rulings (Neheb, the Eternal; Rakdos, Lord of Riots): an opponent's loss of
/// life still counts after that opponent lost the game — the tally records what
/// happened, and an effect can find actions taken by a player who has left
/// the game (CR 800.4i).
#[test]
fn bloodsoaked_counts_an_opponent_who_lost_the_game() {
    bloodsoaked_life_cost_case_with(
        BLOODSOAKED_INSIGHT_WITH_REDUCTION,
        &[P1, engine::types::PlayerId(2)],
        false,
        true,
        3,
    );
}

/// The spelled-out "for each one life … this turn" reads the same life history
/// as "for each 1 life … this turn".
#[test]
fn bloodsoaked_word_form_one_life_reads_this_turns_life_loss() {
    let oracle = BLOODSOAKED_INSIGHT_WITH_REDUCTION.replacen("each 1 life", "each one life", 1);
    assert_ne!(oracle, BLOODSOAKED_INSIGHT_WITH_REDUCTION);
    bloodsoaked_life_cost_case_with(&oracle, &[P1], false, false, 5);
}

#[test]
fn bloodsoaked_sums_every_opponents_life_loss() {
    bloodsoaked_life_cost_case(&[P1, engine::types::PlayerId(2)], false, 3);
}

#[test]
fn licia_reduces_its_printed_cost_for_life_actually_gained_this_turn() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mut lands: Vec<_> = (0..6)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    lands.push(scenario.add_basic_land(P0, ManaColor::Red));
    lands.push(scenario.add_basic_land(P0, ManaColor::White));
    // Pure gain: no opponent loses life, so a "you gained" line misread as
    // "your opponents have lost" would reduce nothing.
    let gain = scenario
        .add_spell_to_hand_from_oracle(P0, "Life Gain (setup)", false, "You gain 4 life.")
        // Setup spell's payment is irrelevant; isolate Licia's cost.
        .with_mana_cost(ManaCost::default())
        .id();
    let licia = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Licia, Sanguine Tribune",
            4,
            4,
            "This spell costs {1} less to cast for each 1 life you gained this turn.\n\
First strike, lifelink\n\
Pay 5 life: Put three +1/+1 counters on Licia. Activate only during your turn and only once each turn.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::Red, ManaCostShard::White, ManaCostShard::Black],
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(gain).resolve();
    assert_eq!(runner.state().objects[&gain].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0), 24);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.state().players[0].life_gained_this_turn, 4);
    assert_eq!(runner.state().players[1].life_gained_this_turn, 0);
    assert_eq!(tapped_life_cost_lands(&runner, &lands), 0);
    cast_and_resolve(&mut runner, licia);
    assert_eq!(runner.state().objects[&licia].zone, Zone::Battlefield);
    assert_eq!(tapped_life_cost_lands(&runner, &lands), 4);
}

/// CR 106.3 + CR 119.3: Neheb, the Eternal's postcombat trigger adds one {R}
/// per point of life the opponents lost this turn — the same quantity the cost
/// reductions read, here as a mana count (the line was unparsed before).
#[test]
fn neheb_adds_red_for_each_life_the_opponents_lost_this_turn() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Neheb, the Eternal",
        4,
        6,
        "Afflict 3 (Whenever this creature becomes blocked, defending player loses 3 life.)\n\
At the beginning of each of your postcombat main phases, add {R} for each 1 life your opponents have lost this turn.",
    );
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        // Setup spell's payment is irrelevant; isolate Neheb's mana.
        .with_mana_cost(ManaCost::default())
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_player(P1).resolve();
    assert_eq!(runner.state().players[1].life_lost_this_turn, 2);

    let mut triggered = false;
    for _ in 0..60 {
        let state = runner.state();
        if state.phase == Phase::PostCombatMain && !state.stack.is_empty() {
            triggered = true;
        }
        if triggered
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    assert!(triggered, "Neheb's postcombat trigger must fire");
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    let pool = &runner.state().players[0].mana_pool;
    assert_eq!(pool.count_color(engine::types::mana::ManaType::Red), 2);
    assert_eq!(pool.total(), 2);
}

/// CR 601.2f: Rakdos, Lord of Riots reduces OTHER creature spells through the
/// battlefield cost-modifier path (not the spell's own), by the same count.
#[test]
fn rakdos_reduces_creature_spells_by_the_opponents_life_loss() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Rakdos, Lord of Riots",
        6,
        6,
        "You can't cast Rakdos unless an opponent lost life this turn.\n\
Flying, trample\n\
Creature spells you cast cost {1} less to cast for each 1 life your opponents have lost this turn.",
    );
    let lands: Vec<_> = (0..4)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        // Setup spell's payment is irrelevant; isolate the creature's cost.
        .with_mana_cost(ManaCost::default())
        .id();
    let giant = scenario
        .add_creature_to_hand_from_oracle(P0, "Hill Giant", 3, 3, "")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_player(P1).resolve();
    assert_eq!(runner.state().players[1].life_lost_this_turn, 2);
    cast_and_resolve(&mut runner, giant);
    assert_eq!(runner.state().objects[&giant].zone, Zone::Battlefield);
    assert_eq!(
        tapped_life_cost_lands(&runner, &lands),
        2,
        "{{3}}{{R}} minus the 2 life the opponent lost"
    );
}

const MEGATRON_TYRANT: &str = "More Than Meets the Eye {1}{R}{W}{B} (You may cast this card converted for {1}{R}{W}{B}.)\n\
Your opponents can't cast spells during combat.\n\
At the beginning of each of your postcombat main phases, you may convert Megatron. If you do, add {C} for each 1 life your opponents have lost this turn.";

fn megatron_back_face() -> engine::game::game_object::BackFaceData {
    engine::game::game_object::BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Megatron, Destructive Force".to_string(),
        power: Some(4),
        toughness: Some(5),
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: engine::types::card_type::CardType {
            supertypes: vec![engine::types::card_type::Supertype::Legendary],
            core_types: vec![engine::types::card_type::CoreType::Artifact],
            subtypes: vec![],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: Some(engine::types::card::LayoutKind::Transform),
        parse_warnings: vec![],
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MegatronChoice {
    Convert,
    Decline,
    /// Megatron is destroyed while its trigger waits; the convert is then
    /// impossible (CR 400.7), so "If you do" must not add mana.
    DestroyedFirst,
    /// A single-faced Megatron (e.g. a nontoken copy such as Spark Double's):
    /// converting it does nothing (CR 701.28c), so the convert is not offered.
    NotDoubleFaced,
    /// Megatron is blinked while its trigger waits: it is back on the
    /// battlefield, but as a new object (CR 400.7), so the trigger can no
    /// longer convert it and "If you do" must not add mana.
    Blinked,
    /// Another effect converts Megatron while its trigger waits: the trigger
    /// can no longer convert it (CR 701.28e), so "If you do" adds nothing.
    ConvertedFirst,
}

/// Returns (colorless mana added, whether Megatron converted, whether the
/// "you may convert" prompt was offered).
fn megatron_case(choice: MegatronChoice) -> (usize, bool, bool) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let megatron = scenario
        .add_creature_from_oracle(P0, "Megatron, Tyrant", 7, 5, MEGATRON_TYRANT)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(ManaCost::default())
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::default())
        .id();
    let convert = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Convert (setup)",
            true,
            "Transform target creature you control.",
        )
        .with_mana_cost(ManaCost::default())
        .id();
    let blink = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Cloudshift",
            true,
            "Exile target creature you control, then return that card to the battlefield under your control.",
        )
        .with_mana_cost(ManaCost::default())
        .id();
    let mut runner = scenario.build();
    if choice != MegatronChoice::NotDoubleFaced {
        runner
            .state_mut()
            .objects
            .get_mut(&megatron)
            .unwrap()
            .back_face = Some(megatron_back_face());
    }
    runner.cast(shock).target_player(P1).resolve();

    let mut offered = false;
    let mut responded = false;
    let mut triggered = false;
    for _ in 0..80 {
        let state = runner.state();
        if state.phase == Phase::PostCombatMain && !state.stack.is_empty() {
            triggered = true;
        }
        if triggered
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::Priority { .. }
                if triggered
                    && matches!(
                        choice,
                        MegatronChoice::DestroyedFirst
                            | MegatronChoice::Blinked
                            | MegatronChoice::ConvertedFirst
                    )
                    && !responded =>
            {
                responded = true;
                let response = match choice {
                    MegatronChoice::Blinked => blink,
                    MegatronChoice::ConvertedFirst => convert,
                    _ => murder,
                };
                GameAction::CastSpell {
                    object_id: response,
                    card_id: state.objects[&response].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                }
            }
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Object(megatron)],
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                GameAction::DecideOptionalEffect {
                    accept: choice != MegatronChoice::Decline,
                }
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    assert!(triggered, "Megatron's postcombat trigger must fire");
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    if choice == MegatronChoice::DestroyedFirst {
        assert_eq!(runner.state().objects[&megatron].zone, Zone::Graveyard);
    }
    if choice == MegatronChoice::Blinked {
        assert_eq!(runner.state().objects[&blink].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().objects[&megatron].zone,
            Zone::Battlefield,
            "the blinked Megatron is back on the battlefield"
        );
    }
    let pool = &runner.state().players[0].mana_pool;
    let colorless = pool.count_color(engine::types::mana::ManaType::Colorless);
    assert_eq!(pool.total(), colorless, "only colorless mana");
    let converted = runner.state().objects[&megatron].transformed;
    (colorless, converted, offered)
}

#[test]
fn megatron_converts_and_adds_colorless_for_each_life_the_opponents_lost() {
    assert_eq!(megatron_case(MegatronChoice::Convert), (2, true, true));
}

#[test]
fn megatron_adds_nothing_when_the_convert_is_declined() {
    assert_eq!(megatron_case(MegatronChoice::Decline), (0, false, true));
}

/// CR 608.2d + CR 400.7: once Megatron has left the battlefield the convert
/// is impossible, so the "you may" is not offered and "If you do" adds nothing.
#[test]
fn megatron_adds_nothing_when_it_left_before_the_trigger_resolved() {
    let (colorless, _, offered) = megatron_case(MegatronChoice::DestroyedFirst);
    assert!(!offered, "an impossible convert must not be offered");
    assert_eq!(colorless, 0);
}

/// CR 608.2d + CR 701.28e: once another effect converted Megatron, the trigger
/// can no longer convert it, so the "you may" is not offered and "If you do"
/// adds nothing.
#[test]
fn megatron_adds_nothing_when_it_already_converted_before_the_trigger_resolved() {
    assert_eq!(
        megatron_case(MegatronChoice::ConvertedFirst),
        (0, true, false)
    );
}

/// CR 608.2d + CR 400.7: a Megatron blinked in response is a new object on the
/// battlefield; the trigger can no longer convert it, so the "you may" is not
/// offered and "If you do" adds nothing.
#[test]
fn megatron_adds_nothing_when_it_was_blinked_before_the_trigger_resolved() {
    assert_eq!(megatron_case(MegatronChoice::Blinked), (0, false, false));
}

/// CR 608.2d + CR 701.28c: a single-faced Megatron cannot convert, so the
/// "you may" is not offered and "If you do" adds nothing.
#[test]
fn megatron_adds_nothing_when_it_is_not_double_faced() {
    assert_eq!(
        megatron_case(MegatronChoice::NotDoubleFaced),
        (0, false, false)
    );
}

const OPTIONAL_SELF_TRANSFORM_WITH_ALTERNATIVE: &str = "At the beginning of each of your postcombat main phases, you may transform Moonlit Test Wolf. If you don't, you gain 3 life.";

/// The parser writes "If you don't, …" as a `Not(EffectOutcome)`-gated
/// sub-ability. The decline authority also honors an explicit `else_ability`,
/// which the regular resolver runs only when the ability's own condition is
/// false; move the printed alternative there.
fn move_alternative_into_else_ability(trigger: &mut engine::types::ability::TriggerDefinition) {
    let transform = trigger.execute.as_mut().expect("trigger body");
    assert!(
        transform.optional,
        "the transform is the optional instruction"
    );
    let mut alternative = transform.sub_ability.take().expect("If you don't");
    assert!(matches!(
        alternative.condition,
        Some(engine::types::ability::AbilityCondition::Not { .. })
    ));
    alternative.condition = None;
    transform.else_ability = Some(alternative);
}

/// Returns (life gained, whether it transformed, whether the "you may
/// transform" prompt was offered).
fn optional_self_transform_case(
    double_faced: bool,
    accept: bool,
    alternative_as_else: bool,
) -> (i32, bool, bool) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let wolf = scenario
        .add_creature_from_oracle(
            P0,
            "Moonlit Test Wolf",
            2,
            2,
            OPTIONAL_SELF_TRANSFORM_WITH_ALTERNATIVE,
        )
        .id();
    let mut runner = scenario.build();
    let wolf_object = runner.state_mut().objects.get_mut(&wolf).unwrap();
    if double_faced {
        wolf_object.back_face = Some(megatron_back_face());
    }
    if alternative_as_else {
        move_alternative_into_else_ability(&mut wolf_object.trigger_definitions[0].definition);
        move_alternative_into_else_ability(
            &mut std::sync::Arc::make_mut(&mut wolf_object.base_trigger_definitions)[0],
        );
    }
    let before = runner.life(P0);

    let mut offered = false;
    let mut triggered = false;
    for _ in 0..80 {
        let state = runner.state();
        if state.phase == Phase::PostCombatMain && !state.stack.is_empty() {
            triggered = true;
        }
        if triggered
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                GameAction::DecideOptionalEffect { accept }
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    assert!(triggered, "the postcombat trigger must fire");
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    let transformed = runner.state().objects[&wolf].transformed;
    (runner.life(P0) - before, transformed, offered)
}

#[test]
fn optional_self_transform_accepted_skips_the_alternative() {
    for alternative_as_else in [false, true] {
        assert_eq!(
            optional_self_transform_case(true, true, alternative_as_else),
            (0, true, true)
        );
    }
}

#[test]
fn optional_self_transform_declined_runs_the_alternative() {
    for alternative_as_else in [false, true] {
        assert_eq!(
            optional_self_transform_case(true, false, alternative_as_else),
            (3, false, true)
        );
    }
}

/// CR 608.2d: an impossible self-transform is not offered, and it resolves
/// like a decline, so "If you don't" still happens. The parsed gated form also
/// passed before the decline routing; the `else_ability` form needs it.
#[test]
fn impossible_optional_self_transform_runs_the_alternative() {
    for alternative_as_else in [false, true] {
        assert_eq!(
            optional_self_transform_case(false, true, alternative_as_else),
            (3, false, false),
            "alternative_as_else = {alternative_as_else}"
        );
    }
}

/// Belbe, Corrupted Observer ruling: "If an opponent lost life and subsequently
/// lost the game, Belbe's triggered ability still counts that player …" — the
/// player-count twin of the life tallies (CR 800.4i).
#[test]
fn belbe_counts_an_opponent_who_lost_life_and_then_the_game() {
    let p2 = engine::types::PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 2);
    for player in [P0, P1, p2] {
        for n in 0..8 {
            scenario.add_card_to_library_top(player, &format!("Filler {n}"));
        }
    }
    scenario.add_creature_from_oracle(
        P0,
        "Belbe, Corrupted Observer",
        2,
        2,
        "At the beginning of each postcombat main phase, the active player adds {C}{C} for each of your opponents who lost life this turn.",
    );
    let shocks: Vec<_> = [P1, p2]
        .iter()
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Shock",
                    true,
                    "Shock deals 2 damage to any target.",
                )
                .with_mana_cost(ManaCost::default())
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.cast(shocks[0]).target_player(P1).resolve();
    runner.cast(shocks[1]).target_player(p2).resolve();
    assert!(
        runner.state().players[1].is_eliminated,
        "P1 must actually have left the game"
    );

    let mut triggered = false;
    for _ in 0..60 {
        let state = runner.state();
        if state.phase == Phase::PostCombatMain && !state.stack.is_empty() {
            triggered = true;
        }
        if triggered
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    assert!(triggered, "Belbe's postcombat trigger must fire");
    let pool = &runner.state().players[0].mana_pool;
    assert_eq!(
        pool.count_color(engine::types::mana::ManaType::Colorless),
        4,
        "{{C}}{{C}} for each of the two opponents who lost life, departed or not"
    );
}

/// CR 608.2d + CR 400.7: after a round trip (convert, bounce to hand, recast)
/// Megatron is a new object that can convert again, so the next postcombat
/// trigger must offer the convert again. Its stored back face then carries no
/// layout tag, which a layout-based double-faced check would misread as
/// single-faced; the feasibility check asks `transform::can_transform`.
#[test]
fn megatron_offers_the_convert_again_after_a_round_trip() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for n in 0..8 {
            scenario.add_card_to_library_top(player, &format!("Filler {n}"));
        }
    }
    let megatron = scenario
        .add_creature_from_oracle(P0, "Megatron, Tyrant", 7, 5, MEGATRON_TYRANT)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(ManaCost::default())
        .id();
    // The converted face is a noncreature artifact, so bounce a permanent.
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Bounce (setup)",
            true,
            "Return target permanent to its owner's hand.",
        )
        .with_mana_cost(ManaCost::default())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&megatron)
        .unwrap()
        .back_face = Some(megatron_back_face());
    runner.cast(shock).target_player(P1).resolve();

    // Turn 1: accept the convert; then bounce the converted Megatron and recast it.
    let mut prompts = 0;
    let mut bounced = false;
    for _ in 0..200 {
        let state = runner.state();
        if prompts == 2 {
            break;
        }
        let action = match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                prompts += 1;
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::Priority { .. }
                if prompts == 1
                    && !bounced
                    && state.phase == Phase::PostCombatMain
                    && state.stack.is_empty() =>
            {
                assert!(
                    state.objects[&megatron].transformed,
                    "first convert happened"
                );
                bounced = true;
                runner.cast(unsummon).target_object(megatron).resolve();
                assert_eq!(runner.state().objects[&megatron].zone, Zone::Hand);
                runner.cast(megatron).resolve();
                assert_eq!(runner.state().objects[&megatron].zone, Zone::Battlefield);
                assert!(!runner.state().objects[&megatron].transformed);
                continue;
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    assert!(bounced, "Megatron must have been bounced and recast");
    assert_eq!(
        prompts, 2,
        "the recast Megatron's next postcombat trigger offers the convert again"
    );
    assert_eq!(runner.state().active_player, P0);
    assert!(runner.state().turn_number > 1);
}

/// CR 800.4i, `AllPlayers` aggregate: Knight of the Ebon Legion's end-step
/// "if a player lost 4 or more life this turn" still sees an opponent who lost
/// 4 life and then lost the game.
#[test]
fn knight_of_the_ebon_legion_counts_a_player_who_lost_the_game() {
    let p2 = engine::types::PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 4);
    for player in [P0, P1, p2] {
        for n in 0..8 {
            scenario.add_card_to_library_top(player, &format!("Filler {n}"));
        }
    }
    let knight = scenario
        .add_creature_from_oracle(
            P0,
            "Knight of the Ebon Legion",
            1,
            2,
            "{2}{B}: This creature gets +3/+3 and gains deathtouch until end of turn.\n\
At the beginning of your end step, if a player lost 4 or more life this turn, put a +1/+1 counter on this creature. (Damage causes loss of life.)",
        )
        .id();
    let shocks: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Shock",
                    true,
                    "Shock deals 2 damage to any target.",
                )
                .with_mana_cost(ManaCost::default())
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.cast(shocks[0]).target_player(P1).resolve();
    runner.cast(shocks[1]).target_player(P1).resolve();
    assert!(
        runner.state().players[1].is_eliminated,
        "P1 lost 4 life and the game"
    );
    assert_eq!(runner.state().players[1].life_lost_this_turn, 4);
    assert_eq!(runner.state().players[2].life_lost_this_turn, 0);

    for _ in 0..80 {
        let state = runner.state();
        if state.phase == Phase::Cleanup || state.active_player != P0 {
            break;
        }
        let action = match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    let counters = runner.state().objects[&knight]
        .counters
        .get(&engine::types::counter::CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 1,
        "the departed opponent's 4 life lost this turn counts"
    );
}

/// Two-Headed Giant seating (`FormatConfig::two_headed_giant()`, 4 players):
/// P0+P1 are one team, P2+P3 the other.
const TEAMMATE: engine::types::PlayerId = engine::types::PlayerId(1);
const OPPOSING: engine::types::PlayerId = engine::types::PlayerId(2);

/// Life changes happen to each player individually (CR 810.9), and a player's
/// opponents are the players not on their team (CR 102.3). `Change` spells hit
/// `who` through real casts before P0's postcombat trigger adds `{C}` per the
/// printed `trigger` line; returns the `{C}` added.
fn team_game_life_history_case(trigger: &str, change: &str, who: engine::types::PlayerId) -> usize {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Life Ledger", 2, 2, trigger);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Life Changer", true, change)
        .with_mana_cost(ManaCost::default())
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_player(who).resolve();
    // Reach guard: the life change is recorded on exactly `who`.
    for player in &runner.state().players {
        let changed = player.life_lost_this_turn > 0 || player.life_gained_this_turn > 0;
        assert_eq!(changed, player.id == who, "{:?}", player.id);
    }

    let mut triggered = false;
    for _ in 0..60 {
        let state = runner.state();
        if state.phase == Phase::PostCombatMain && !state.stack.is_empty() {
            triggered = true;
        }
        if triggered
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    assert!(triggered, "the postcombat trigger must fire");
    runner.state().players[0]
        .mana_pool
        .count_color(engine::types::mana::ManaType::Colorless)
}

const SHOCK: &str = "Shock deals 2 damage to any target.";
const GAIN_THREE: &str = "Target player gains 3 life.";

/// CR 102.3 + CR 810.9: in Two-Headed Giant a teammate's life change is not an
/// opponent's — neither for "each 1 life your opponents have lost/gained this
/// turn" (`PlayerScope::Opponent`) nor for "each of your opponents who
/// lost/gained life this turn" (`OpponentLostLife` / `OpponentGainedLife`). The
/// same change on a player of the opposing team counts.
#[test]
fn team_game_life_history_counts_opponents_not_the_teammate() {
    let prefix = "At the beginning of each of your postcombat main phases, add {C} for each ";
    for (tail, change, opposing) in [
        ("1 life your opponents have lost this turn.", SHOCK, 2),
        (
            "1 life your opponents have gained this turn.",
            GAIN_THREE,
            3,
        ),
        ("of your opponents who lost life this turn.", SHOCK, 1),
        (
            "of your opponents who gained life this turn.",
            GAIN_THREE,
            1,
        ),
    ] {
        let trigger = format!("{prefix}{tail}");
        assert_eq!(
            team_game_life_history_case(&trigger, change, TEAMMATE),
            0,
            "teammate: {tail}"
        );
        assert_eq!(
            team_game_life_history_case(&trigger, change, OPPOSING),
            opposing,
            "opposing team: {tail}"
        );
    }
}

/// CR 702.137a + CR 102.3: Spectacle's "if an opponent lost life this turn"
/// reads the same life history, so in Two-Headed Giant a teammate's loss does
/// not open it. P0 has one Mountain; Skewer the Critics costs {2}{R}, so it can
/// only be cast for its spectacle cost {R}.
fn team_game_spectacle_castable_after_shock(who: engine::types::PlayerId) -> bool {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .with_mana_cost(ManaCost::default())
        .id();
    let skewer = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Skewer the Critics",
            false,
            "Spectacle {R}\nSkewer the Critics deals 3 damage to any target.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_player(who).resolve();
    assert!(runner.state().players[usize::from(who.0)].life_lost_this_turn > 0);
    let card_id = runner.state().objects[&skewer].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: skewer,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .and_then(|_| {
            runner.act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(OPPOSING)],
            })
        });
    let on_stack = runner.state().objects[&skewer].zone == Zone::Stack;
    assert_eq!(
        cast.is_ok(),
        on_stack,
        "the cast either lands or is refused"
    );
    on_stack
}

#[test]
fn team_game_spectacle_opens_for_an_opposing_player_not_the_teammate() {
    assert!(!team_game_spectacle_castable_after_shock(TEAMMATE));
    assert!(team_game_spectacle_castable_after_shock(OPPOSING));
}
