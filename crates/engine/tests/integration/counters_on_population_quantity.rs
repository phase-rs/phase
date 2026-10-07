//! Counter-count quantities summed over a population of objects (CR 122.1).
//!
//! "for each +1/+1 counter on creatures you control", "the number of counters
//! on permanents you control", "for each lore counter among Sagas you control":
//! the count of counters of the named kind (every kind when untyped) on every
//! object the population names (`QuantityRef::CountersOnObjects`). Each test
//! drives the real pipeline on a card staged from its verbatim Oracle text
//! (MTGJSON `AtomicCards.json`) — except the self-spell cost static, which no
//! printed card phrases this way — one per production route that reads the count:
//! an activated cost rider, a self-spell cost static, a per-recipient
//! continuous P/T, an enters-with replacement, resolution effects, a where-X,
//! and a target-filter quantity.

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::derived::derive_display_state;
use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DEEPWOOD_DENIZEN: &str = "Vigilance (Attacking doesn't cause this creature to tap.)\n{5}{G}, {T}: Draw a card. This ability costs {1} less to activate for each +1/+1 counter on creatures you control.";
const IMMACULATE_MAGISTRATE: &str =
    "{T}: Put a +1/+1 counter on target creature for each Elf you control.";
const GLEAM_OF_AUTHORITY: &str = "Enchant creature\nEnchanted creature gets +1/+1 for each +1/+1 counter on other creatures you control.\nEnchanted creature has vigilance and \"{W}, {T}: Bolster 1.\" (To bolster 1, choose a creature with the least toughness among creatures you control and put a +1/+1 counter on it.)";
const BATTLEGROWTH: &str = "Put a +1/+1 counter on target creature.";
const BIOESSENCE_HYDRA: &str = "Trample\nThis creature enters with a +1/+1 counter on it for each loyalty counter on planeswalkers you control.\nWhenever one or more loyalty counters are put on planeswalkers you control, put that many +1/+1 counters on this creature.";
const ASCENDANT_ACOLYTE: &str = "This creature enters with a +1/+1 counter on it for each +1/+1 counter among other creatures you control.\nAt the beginning of your upkeep, double the number of +1/+1 counters on this creature.";
const CHONG_AND_LILY: &str = "Whenever one or more Bards you control attack, choose one —\n• Put a lore counter on each of any number of target Sagas you control.\n• Creatures you control get +1/+0 until end of turn for each lore counter among Sagas you control.";
const SPARAS_BODYGUARD: &str = "When Spara's Bodyguard enters the battlefield, you may choose a creature card in your hand. If you do, it perpetually gains \"This creature enters the battlefield with an additional shield counter on it.\" Otherwise, put a shield counter on Spara's Bodyguard. (If it would be dealt damage or destroyed, remove a shield counter from it instead.)\nAt the beginning of each combat, Spara's Bodyguard gets +1/+1 until end of turn for each shield counter among other creatures you control.";
const MOIRA_BROWN: &str = "When Moira Brown enters, create a colorless Book Equipment artifact token named Wasteland Survival Guide with \"Equipped creature gets +1/+1 for each quest counter among permanents you control\" and equip {1}.\nWhenever you attack, put a quest counter on target nonland permanent you control.";
const HYDRA_TRAINER: &str = "You may exert this creature as it attacks. When you do, target creature gets +X/+X until end of turn, where X is the number of counters on permanents you control. (An exerted creature won't untap during your next untap step.)\n{2}{G}: Adapt 2. (If this creature has no +1/+1 counters on it, put two +1/+1 counters on it.)";
const DIMENSION_X_PIZZASAUR: &str = "When this creature enters, put two +1/+1 counters on target creature. When you do, destroy up to one target creature with mana value less than or equal to the number of counters among permanents you control.\n{2}, {T}, Sacrifice this creature: You gain 3 life and each opponent loses 3 life.";
/// Not a printed card: the self-spell cost static over the same census, so the
/// spell-cost route is exercised with the population grammar.
const POPULATION_DISCOUNT_SPELL: &str =
    "This spell costs {1} less to cast for each +1/+1 counter on creatures you control.";

fn units(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|ty| ManaUnit::new(*ty, ObjectId(0), false, vec![]))
        .collect()
}

fn green_plus_colorless(colorless: usize) -> Vec<ManaType> {
    std::iter::once(ManaType::Green)
        .chain(std::iter::repeat_n(ManaType::Colorless, colorless))
        .collect()
}

fn pool(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].mana_pool.total()
}

fn set_pool(runner: &mut GameRunner, player: PlayerId, types: &[ManaType]) {
    let state = runner.state_mut();
    state.players[player.0 as usize].mana_pool.clear();
    for unit in units(types) {
        let _ = state.add_mana_to_pool(player, unit);
    }
}

fn counters(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(kind)
        .copied()
        .unwrap_or(0)
}

fn add_counters(runner: &mut GameRunner, id: ObjectId, kind: CounterType, n: u32) {
    *runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .counters
        .entry(kind)
        .or_insert(0) += n;
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = &runner.state().objects[&id];
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn seed_library(scenario: &mut GameScenario, player: PlayerId) {
    for name in ["Library A", "Library B", "Library C"] {
        scenario.add_card_to_library_top(player, name);
    }
}

/// Pass priority and answer every prompt on the way until the stack is empty,
/// declaring `targets` (in order) at each target prompt.
fn drive_to_empty_stack(runner: &mut GameRunner, targets: &[ObjectId]) {
    let mut targets = targets.iter();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let target = targets.next().expect("a declared target for this prompt");
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(*target)],
                    })
                    .expect("declared target accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay from pool");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

// ── Deepwood Denizen: activated self cost rider (CR 601.2f + CR 602.2b) ──────

struct DeepwoodBoard {
    runner: GameRunner,
    deepwood: ObjectId,
}

/// Deepwood Denizen with `own` +1/+1 counters, plus P0 / P1 creatures carrying
/// the given counters. Libraries are seeded so the draw resolves.
fn deepwood_board(own: u32, p0_others: &[(CounterType, u32)], p1: &[u32]) -> DeepwoodBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let deepwood = scenario
        .add_creature(P0, "Deepwood Denizen", 3, 2)
        .with_subtypes(vec!["Elf", "Warrior"])
        .from_oracle_text_with_keywords(&["Vigilance"], DEEPWOOD_DENIZEN)
        .id();
    scenario.with_counter(deepwood, CounterType::Plus1Plus1, own);
    for (i, (kind, n)) in p0_others.iter().enumerate() {
        let id = scenario.add_creature(P0, &format!("Ally {i}"), 5, 5).id();
        scenario.with_counter(id, kind.clone(), *n);
    }
    for (i, n) in p1.iter().enumerate() {
        let id = scenario.add_creature(P1, &format!("Foe {i}"), 2, 2).id();
        scenario.with_counter(id, CounterType::Plus1Plus1, *n);
    }
    seed_library(&mut scenario, P0);
    seed_library(&mut scenario, P1);
    let runner = scenario.build();
    assert!(
        runner.state().objects[&deepwood].abilities[0]
            .cost_reduction
            .is_some(),
        "reach guard: the cost rider is parsed onto the draw ability"
    );
    DeepwoodBoard { runner, deepwood }
}

fn deepwood_offered(board: &DeepwoodBoard) -> bool {
    let by_gate = can_activate_ability_now(board.runner.state(), P0, board.deepwood, 0);
    let by_actions = legal_actions(board.runner.state()).iter().any(|action| {
        matches!(
            action,
            GameAction::ActivateAbility { source_id, ability_index: 0 } if *source_id == board.deepwood
        )
    });
    assert_eq!(by_gate, by_actions, "the gate and the legal actions agree");
    by_gate
}

/// CR 601.2f + CR 602.2b + CR 122.1: one +1/+1 counter on Deepwood and two
/// on another creature you control reduce {5}{G} to {2}{G}; three mana pays it,
/// the ability resolves and draws.
#[test]
fn deepwood_denizen_counts_counters_on_every_creature_you_control() {
    let mut board = deepwood_board(1, &[(CounterType::Plus1Plus1, 2)], &[]);
    set_pool(&mut board.runner, P0, &green_plus_colorless(2));
    assert!(deepwood_offered(&board));
    let hand_before = hand_len(&board.runner, P0);

    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.deepwood,
            ability_index: 0,
        })
        .expect("{2}{G} is affordable from [G, C, C]");
    drive_to_empty_stack(&mut board.runner, &[]);

    assert_eq!(pool(&board.runner, P0), 0, "exactly {{2}}{{G}} was paid");
    assert!(board.runner.state().objects[&board.deepwood].tapped);
    assert_eq!(hand_len(&board.runner, P0), hand_before + 1, "drew a card");
}

/// The same board with two mana: {2}{G} is not affordable, so the ability
/// is not offered; one more mana makes it so (reach guard).
#[test]
fn deepwood_denizen_is_not_offered_below_the_reduced_cost() {
    let mut board = deepwood_board(1, &[(CounterType::Plus1Plus1, 2)], &[]);
    set_pool(&mut board.runner, P0, &green_plus_colorless(1));
    assert!(!deepwood_offered(&board));
    set_pool(&mut board.runner, P0, &green_plus_colorless(2));
    assert!(
        deepwood_offered(&board),
        "reach guard: {{2}}{{G}} with three mana"
    );
}

/// CR 109.5: "you control" — an opponent's creature's counters do not
/// reduce the cost.
#[test]
fn deepwood_denizen_ignores_counters_on_opponents_creatures() {
    let mut board = deepwood_board(0, &[], &[5]);
    set_pool(&mut board.runner, P0, &green_plus_colorless(2));
    assert!(
        !deepwood_offered(&board),
        "the opponent's five counters must not reduce {{5}}{{G}}"
    );
    set_pool(&mut board.runner, P0, &green_plus_colorless(5));
    assert!(
        deepwood_offered(&board),
        "reach guard: the full cost is payable"
    );
}

/// CR 122.1: only +1/+1 counters count. -1/-1 counters on a separate
/// permanent (so CR 704.5q does not annihilate them) are a different kind.
#[test]
fn deepwood_denizen_ignores_other_counter_kinds() {
    let mut board = deepwood_board(1, &[(CounterType::Minus1Minus1, 3)], &[]);
    set_pool(&mut board.runner, P0, &green_plus_colorless(1));
    assert!(
        !deepwood_offered(&board),
        "-1/-1 counters must not reduce the cost"
    );
    set_pool(&mut board.runner, P0, &green_plus_colorless(4));
    assert!(
        deepwood_offered(&board),
        "reach guard: {{4}}{{G}} is payable"
    );
}

/// CR 118.7a: a generic reduction cannot reduce the colored component —
/// seven counters still leave {G}.
#[test]
fn deepwood_denizen_reduction_floors_at_the_colored_pip() {
    let mut board = deepwood_board(3, &[(CounterType::Plus1Plus1, 4)], &[]);
    set_pool(&mut board.runner, P0, &[ManaType::Colorless; 6]);
    assert!(
        !deepwood_offered(&board),
        "colorless mana alone cannot pay the {{G}}"
    );
    set_pool(&mut board.runner, P0, &[ManaType::Green]);
    assert!(
        deepwood_offered(&board),
        "{{G}} alone pays the floored cost"
    );
}

/// CR 601.2f: the count is read live when the cost is determined, so
/// counters added by Immaculate Magistrate make the ability affordable.
#[test]
fn deepwood_denizen_cost_tracks_counters_added_before_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let deepwood = scenario
        .add_creature(P0, "Deepwood Denizen", 3, 2)
        .with_subtypes(vec!["Elf", "Warrior"])
        .from_oracle_text_with_keywords(&["Vigilance"], DEEPWOOD_DENIZEN)
        .id();
    let magistrate = scenario
        .add_creature_from_oracle(P0, "Immaculate Magistrate", 2, 2, IMMACULATE_MAGISTRATE)
        .with_subtypes(vec!["Elf", "Shaman"])
        .id();
    seed_library(&mut scenario, P0);
    let mut runner = scenario.build();
    set_pool(&mut runner, P0, &green_plus_colorless(3));
    assert!(!can_activate_ability_now(runner.state(), P0, deepwood, 0));

    runner
        .act(GameAction::ActivateAbility {
            source_id: magistrate,
            ability_index: 0,
        })
        .expect("Magistrate activates");
    drive_to_empty_stack(&mut runner, &[deepwood]);
    assert_eq!(
        counters(&runner, deepwood, &CounterType::Plus1Plus1),
        2,
        "reach guard: two Elves put two counters on Deepwood"
    );
    assert!(
        can_activate_ability_now(runner.state(), P0, deepwood, 0),
        "{{3}}{{G}} is now payable from [G, C, C, C]"
    );
}

// ── Gleam of Authority: per-recipient continuous P/T (CR 611.3a + CR 613.4c) ──

/// "other creatures you control" is relative to the enchanted creature:
/// the host's own counter is excluded, an opponent's counters are excluded,
/// and the bonus follows counters added later (CR 611.3a).
#[test]
fn gleam_of_authority_counts_counters_on_creatures_other_than_the_enchanted_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    scenario.with_counter(host, CounterType::Plus1Plus1, 1);
    let other = scenario.add_creature(P0, "Other", 2, 2).id();
    scenario.with_counter(other, CounterType::Plus1Plus1, 2);
    let foe = scenario.add_creature(P1, "Foe", 2, 2).id();
    scenario.with_counter(foe, CounterType::Plus1Plus1, 4);
    let gleam = scenario
        .add_creature(P0, "Gleam of Authority", 0, 0)
        .from_oracle_text_with_keywords(&["Bolster", "Enchant"], GLEAM_OF_AUTHORITY)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .id();
    let battlegrowth = scenario
        .add_spell_to_hand_from_oracle(P0, "Battlegrowth", true, BATTLEGROWTH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), gleam, host);
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());

    // 2/2 + its own counter = 3/3, + two counters on Other = 5/5. Counting the
    // host's own counter would give 6/6; the opponent's, 9/9.
    assert_eq!(power_toughness(&runner, host), (5, 5));

    runner.cast(battlegrowth).target_object(other).resolve();
    assert_eq!(counters(&runner, other, &CounterType::Plus1Plus1), 3);
    assert_eq!(power_toughness(&runner, host), (6, 6));
}

// ── Enters-with replacements (CR 614.1c + CR 614.12) ─────────────────────────

/// Bioessence Hydra enters with a +1/+1 counter per loyalty counter on
/// planeswalkers you control — not an opponent's.
#[test]
fn bioessence_hydra_enters_with_counters_from_your_planeswalkers_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_planeswalker_from_oracle(P0, "Your Walker", "Jace", 4, "");
    scenario.add_planeswalker_from_oracle(P1, "Their Walker", "Liliana", 3, "");
    let hydra = scenario
        .add_creature_to_hand_from_oracle(P0, "Bioessence Hydra", 4, 4, BIOESSENCE_HYDRA)
        .with_subtypes(vec!["Hydra", "Mutant"])
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        units(&[
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(hydra).resolve();
    outcome.assert_zone(&[hydra], Zone::Battlefield);
    assert_eq!(counters(&runner, hydra, &CounterType::Plus1Plus1), 4);
}

/// Ascendant Acolyte counts +1/+1 counters among OTHER creatures you
/// control.
#[test]
fn ascendant_acolyte_enters_with_counters_among_your_other_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Ally A", 2, 2).id();
    scenario.with_counter(a, CounterType::Plus1Plus1, 2);
    let b = scenario.add_creature(P0, "Ally B", 2, 2).id();
    scenario.with_counter(b, CounterType::Plus1Plus1, 1);
    let foe = scenario.add_creature(P1, "Foe", 2, 2).id();
    scenario.with_counter(foe, CounterType::Plus1Plus1, 5);
    let acolyte = scenario
        .add_creature_to_hand_from_oracle(P0, "Ascendant Acolyte", 1, 1, ASCENDANT_ACOLYTE)
        .with_subtypes(vec!["Human", "Monk"])
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(P0, units(&green_plus_colorless(4)));
    let mut runner = scenario.build();
    let outcome = runner.cast(acolyte).resolve();
    outcome.assert_zone(&[acolyte], Zone::Battlefield);
    assert_eq!(counters(&runner, acolyte, &CounterType::Plus1Plus1), 3);
}

// ── Resolution effects (CR 608.2h) ───────────────────────────────────────────

/// Chong and Lily, Nomads, mode two: +1/+0 for each lore counter among
/// Sagas you control — the opponent's Saga and a non-Saga enchantment are
/// excluded, and the amount is locked in at resolution (CR 608.2h).
#[test]
fn chong_and_lily_mode_two_counts_lore_among_your_sagas() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let chong = scenario
        .add_creature_from_oracle(P0, "Chong and Lily, Nomads", 3, 3, CHONG_AND_LILY)
        .with_subtypes(vec!["Human", "Bard", "Ally"])
        .as_legendary()
        .id();
    let saga_a = scenario
        .add_enchantment_from_oracle(P0, "Saga A", "")
        .with_subtypes(vec!["Saga"])
        .id();
    let saga_b = scenario
        .add_enchantment_from_oracle(P0, "Saga B", "")
        .with_subtypes(vec!["Saga"])
        .id();
    let plain = scenario
        .add_enchantment_from_oracle(P0, "Plain Enchantment", "")
        .id();
    let saga_opp = scenario
        .add_enchantment_from_oracle(P1, "Opponent Saga", "")
        .with_subtypes(vec!["Saga"])
        .id();
    scenario.with_counter(saga_a, CounterType::Lore, 2);
    scenario.with_counter(saga_b, CounterType::Lore, 1);
    scenario.with_counter(plain, CounterType::Lore, 4);
    scenario.with_counter(saga_opp, CounterType::Lore, 5);
    let mut runner = scenario.build();

    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(chong, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Chong attacks");
    let mut chose = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes { indices: vec![1] })
                    .expect("mode two");
                chose = true;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(chose, "reach guard: the attack trigger offered its modes");
    assert_eq!(power_toughness(&runner, chong), (6, 3), "3 + three lore");

    add_counters(&mut runner, saga_a, CounterType::Lore, 1);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        power_toughness(&runner, chong),
        (6, 3),
        "CR 608.2h: the bonus was determined once, at resolution"
    );
}

/// Spara's Bodyguard's combat trigger counts shield counters among OTHER
/// creatures you control: not its own, not an opponent's.
#[test]
fn sparas_bodyguard_counts_shield_counters_among_your_other_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spara = scenario
        .add_creature_from_oracle(P0, "Spara's Bodyguard", 3, 3, SPARAS_BODYGUARD)
        .with_subtypes(vec!["Rhino", "Warrior"])
        .id();
    scenario.with_counter(spara, CounterType::Shield, 1);
    let ally = scenario.add_creature(P0, "Ally", 2, 2).id();
    scenario.with_counter(ally, CounterType::Shield, 2);
    let foe = scenario.add_creature(P1, "Foe", 2, 2).id();
    scenario.with_counter(foe, CounterType::Shield, 3);
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::BeginCombat);
    drive_to_empty_stack(&mut runner, &[]);
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert_eq!(power_toughness(&runner, spara), (5, 5));
}

/// Moira Brown's Wasteland Survival Guide: "+1/+1 for each quest counter
/// among permanents you control" — the token's own, Moira's, not an opponent's.
#[test]
fn wasteland_survival_guide_counts_quest_counters_among_your_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bearer = scenario.add_creature(P0, "Bearer", 2, 2).id();
    let foe_relic = scenario.add_artifact_from_oracle(P1, "Foe Relic", "").id();
    scenario.with_counter(foe_relic, parse_counter_type("quest"), 4);
    let moira = scenario
        .add_creature_to_hand_from_oracle(P0, "Moira Brown, Guide Author", 2, 3, MOIRA_BROWN)
        .with_subtypes(vec!["Human", "Citizen"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red, ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        units(&[ManaType::Red, ManaType::White, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    runner.cast(moira).resolve();
    drive_to_empty_stack(&mut runner, &[]);
    let guide = *runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Wasteland Survival Guide")
        .expect("reach guard: the Book token was created");

    add_counters(&mut runner, guide, parse_counter_type("quest"), 1);
    add_counters(&mut runner, moira, parse_counter_type("quest"), 1);
    attach_to(runner.state_mut(), guide, bearer);
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());
    assert_eq!(power_toughness(&runner, bearer), (4, 4));
}

/// Hydra Trainer: X is the number of counters (every kind) on permanents
/// you control.
#[test]
fn hydra_trainer_x_counts_every_kind_of_counter_on_your_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trainer = scenario
        .add_creature(P0, "Hydra Trainer", 1, 1)
        .with_subtypes(vec!["Human", "Warrior"])
        .from_oracle_text_with_keywords(&["Adapt", "Exert"], HYDRA_TRAINER)
        .id();
    let target = scenario.add_creature(P0, "Target", 2, 2).id();
    scenario.with_counter(target, CounterType::Plus1Plus1, 1);
    let relic = scenario.add_artifact_from_oracle(P0, "Relic", "").id();
    scenario.with_counter(relic, parse_counter_type("charge"), 2);
    let foe = scenario.add_artifact_from_oracle(P1, "Foe Relic", "").id();
    scenario.with_counter(foe, parse_counter_type("charge"), 5);
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(trainer, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Hydra Trainer attacks");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");
    runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("exert");
    drive_to_empty_stack(&mut runner, &[target]);

    // 2/2 + one counter = 3/3, + X = 3 (one +1/+1, two charge) = 6/6.
    assert_eq!(power_toughness(&runner, target), (6, 6));
}

/// Dimension X Pizzasaur: the reflexive destroy's mana-value cap is the
/// number of counters among permanents you control, read after the two
/// counters land.
#[test]
fn dimension_x_pizzasaur_caps_the_destroy_at_counters_among_your_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    scenario.with_counter(host, CounterType::Plus1Plus1, 1);
    let three = scenario
        .add_creature(P1, "Mana Value Three", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![],
        })
        .id();
    let four = scenario
        .add_creature(P1, "Mana Value Four", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![],
        })
        .id();
    let pizzasaur = scenario
        .add_creature_to_hand_from_oracle(P0, "Dimension X Pizzasaur", 2, 1, DIMENSION_X_PIZZASAUR)
        .with_subtypes(vec!["Food", "Alien", "Mutant"])
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        units(&[
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    runner.cast(pizzasaur).commit();

    let mut offered = None;
    let mut first_prompt = true;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } if first_prompt => {
                first_prompt = false;
                assert!(target_slots
                    .iter()
                    .any(|slot| slot.legal_targets.contains(&TargetRef::Object(host))));
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(host)],
                    })
                    .expect("counters on Host");
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                offered = Some(
                    target_slots
                        .iter()
                        .flat_map(|slot| slot.legal_targets.clone())
                        .collect::<Vec<_>>(),
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(three)],
                    })
                    .expect("destroy the mana value three creature");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(counters(&runner, host, &CounterType::Plus1Plus1), 3);
    let offered = offered.expect("reach guard: the reflexive destroy asked for a target");
    assert!(offered.contains(&TargetRef::Object(three)));
    assert!(
        !offered.contains(&TargetRef::Object(four)),
        "mana value 4 exceeds the three counters"
    );
    assert_eq!(runner.state().objects[&three].zone, Zone::Graveyard);
}

// ── Self-spell cost static (CR 601.2f) ───────────────────────────────────────

/// A self-spell cost reduction over the same census: three counters on
/// creatures you control reduce {5}{G} to {2}{G}; an opponent's do not count.
#[test]
fn self_spell_cost_reduction_counts_counters_on_your_creatures() {
    let build = |mana: Vec<ManaType>| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ally = scenario.add_creature(P0, "Ally", 2, 2).id();
        scenario.with_counter(ally, CounterType::Plus1Plus1, 3);
        let foe = scenario.add_creature(P1, "Foe", 2, 2).id();
        scenario.with_counter(foe, CounterType::Plus1Plus1, 4);
        let spell = scenario
            .add_creature_to_hand_from_oracle(P0, "Census Beast", 4, 4, POPULATION_DISCOUNT_SPELL)
            .with_mana_cost(ManaCost::Cost {
                generic: 5,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        scenario.with_mana_pool(P0, units(&mana));
        (scenario.build(), spell)
    };

    let (mut runner, spell) = build(green_plus_colorless(2));
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Battlefield);

    let (mut runner, spell) = build(green_plus_colorless(1));
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "{{2}}{{G}} is not payable from two mana; the opponent's counters do not count"
    );
}
