//! "Whenever you face a dilemma" (TRK). The reminder text reads: "You face a
//! dilemma as you choose one or more modes for a spell or ability." No CR rule
//! names it (needs manual verification); the mode choices it reads are CR
//! 700.2a (spells and activated abilities, CR 601.2b / CR 602.2b), CR 700.2b
//! (triggered abilities, CR 603.3c) and CR 700.2e (a player other than the
//! controller chooses).
//!
//! One dilemma is faced per mode-choice announcement, not per mode. The trigger
//! event happens as the modes are announced (CR 603.2); the per-turn history is
//! written once the announcement reaches the stack (CR 601.2i + CR 733.1: an
//! undone announcement records nothing). Copies (CR 707.10, CR 700.2g) and
//! modes drawn at random face no dilemma (engine policy, needs manual
//! verification).
//!
//! Every card below is built from its verbatim Oracle text.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::PlayerActionKind;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SEVEN_OF_NINE: &str = "Whenever you face a dilemma, draw a card. (You face a dilemma as you choose one or more modes for a spell or ability.)";
const OBSIDIAN_ORDER_OPERATIVE: &str = "Whenever you face a dilemma, each opponent loses 1 life and you gain 1 life. (You face a dilemma as you choose one or more modes for a spell or ability.)";
const JETT_RENO: &str = "Whenever Jett Reno becomes tapped, surveil 1. If her power is 4 or greater, instead draw a card.\nWhenever you face a dilemma, put a +1/+1 counter on Jett Reno. (You face a dilemma as you choose one or more modes for a spell or ability.)";
const ELIM_GARAK: &str = "Menace\nWhenever you face a dilemma, each opponent loses 2 life and you gain 2 life. (You face a dilemma as you choose one or more modes for a spell or ability.)";
const MELKOTIAN_ADJUDICATOR: &str = "Flying, vigilance\nWhenever you face a dilemma, put a +1/+1 counter on this creature. (You face a dilemma as you choose one or more modes for a spell or ability.)";
const GABRIEL_LORCA: &str = "Menace\nWhenever you face a dilemma, you may pay {B}. If you do, return this card from your graveyard to your hand. (You face a dilemma as you choose one or more modes for a spell or ability.)";
const TELL_ME: &str = "When this enchantment enters, target opponent may have you draw a card. If they don't, they lose 4 life and you surveil 1.\nWhenever you face a dilemma, you may return this enchantment to its owner's hand. (You face a dilemma as you choose one or more modes for a spell or ability.)";

const ABZAN_CHARM: &str = "Choose one —\n• Exile target creature with power 3 or greater.\n• You draw two cards and you lose 2 life.\n• Distribute two +1/+1 counters among one or two target creatures.";
const ATARKAS_COMMAND: &str = "Choose two —\n• Your opponents can't gain life this turn.\n• Atarka's Command deals 3 damage to each opponent.\n• You may put a land card from your hand onto the battlefield.\n• Creatures you control get +1/+1 and gain reach until end of turn.";
const CANKERBLOOM: &str = "{1}, Sacrifice this creature: Choose one —\n• Destroy target artifact.\n• Destroy target enchantment.\n• Proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)";
const CHARMING_PRINCE: &str = "When this creature enters, choose one —\n• Scry 2.\n• You gain 3 life.\n• Exile another target creature you own. Return it to the battlefield under your control at the beginning of the next end step.";
const FATAL_LORE: &str = "An opponent chooses one —\n• You draw three cards.\n• You destroy up to two target creatures that player controls. They can't be regenerated. That player draws up to three cards.";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const CULT_OF_SKARO: &str = "Whenever Cult of Skaro attacks, choose one at random —\n• Thay — Put a +1/+1 counter on each artifact creature you control.\n• Caan — Draw two cards.\n• Sec — Create a 3/3 black Dalek artifact creature token with menace.\n• Jast — Each opponent loses 4 life.";
const CALL_DAMAGE_CONTROL: &str = "Choose up to two. Return those cards from your graveyard to your hand.\n• Target artifact card.\n• Target creature card.\n• Target enchantment card.\n• Target land card.";
const CHAOS_WAND: &str = "{4}, {T}: Target opponent exiles cards from the top of their library until they exile an instant or sorcery card. You may cast that card without paying its mana cost. Then put the exiled cards that weren't cast this way on the bottom of that library in a random order.";
const LOBELIA: &str = "When Lobelia enters, look at the top card of each opponent's library and exile those cards face down.\n{T}, Sacrifice an artifact: Choose one —\n• Until end of turn, you may play a card exiled with Lobelia without paying its mana cost.\n• Each opponent loses 2 life and you gain 2 life.";

/// `player`'s faced dilemmas in the per-turn history.
fn dilemmas(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(actor, kind)| *actor == player && *kind == PlayerActionKind::FaceDilemma)
        .count()
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// P0's precombat main phase, five library cards per seat.
fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for index in 0..5 {
            scenario.add_spell_to_library_top(
                player,
                &format!("Library {}-{index}", player.0),
                false,
            );
        }
    }
    scenario
}

fn add_seven(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, "Seven of Nine", 2, 4, SEVEN_OF_NINE)
        .as_artifact_creature()
        .id()
}

fn add_abzan_charm(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Abzan Charm", true, ABZAN_CHARM)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// Prompts answered by `drive`, consumed in order; anything unplanned panics.
#[derive(Default)]
struct Plan {
    modes: Vec<Vec<usize>>,
    targets: Vec<TargetRef>,
    optional: Vec<bool>,
}

/// The actor of each mode and optional prompt `drive` answered.
#[derive(Debug, Default)]
struct Record {
    mode_choosers: Vec<PlayerId>,
    optional_actors: Vec<PlayerId>,
    /// `(player, choices)` of each `PayCost` prompt, answered with every choice.
    cost_choices: Vec<(PlayerId, Vec<ObjectId>)>,
}

/// Answer every prompt from `plan` until the stack is empty at priority.
fn drive(runner: &mut GameRunner, plan: Plan) -> Record {
    let Plan {
        modes,
        targets,
        optional,
    } = plan;
    let mut modes = modes.into_iter();
    let mut targets = targets.into_iter();
    let mut optional = optional.into_iter();
    let mut record = Record::default();
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ModeChoice { player, .. }
            | WaitingFor::AbilityModeChoice { player, .. } => {
                record.mode_choosers.push(player);
                let indices = modes.next().expect("unplanned mode prompt");
                runner
                    .act(GameAction::SelectModes { indices })
                    .expect("choose the planned modes");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                let target = targets.next().expect("unplanned target prompt");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target),
                    })
                    .expect("choose the planned target");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                record.optional_actors.push(player);
                let accept = optional.next().expect("unplanned optional prompt");
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer the optional prompt");
            }
            WaitingFor::ScryChoice { cards, .. } | WaitingFor::SurveilChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("keep the cards on top");
            }
            WaitingFor::PayCost {
                player, choices, ..
            } => {
                record.cost_choices.push((player, choices.clone()));
                runner
                    .act(GameAction::SelectCards { cards: choices })
                    .expect("pay the cost with its only choice");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return record;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while the stack resolves");
            }
            other => panic!("unexpected prompt {other:?}; record {record:?}"),
        }
    }
    panic!("the stack did not resolve: {record:?}");
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the spell");
}

/// R1 — CR 601.2b + CR 700.2a: casting a modal spell faces one dilemma; Seven
/// of Nine draws once and the history records one dilemma.
#[test]
fn spell_mode_choice_faces_a_dilemma() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    let charm = add_abzan_charm(&mut scenario);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    cast(&mut runner, charm);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P0]);
    // The charm left; it drew two; Seven drew one.
    assert_eq!(hand_len(&runner, P0), hand - 1 + 2 + 1);
    assert_eq!(runner.life(P0), life - 2, "REACH: the charm resolved");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R2 — CR 700.2a + CR 700.2d: two modes chosen are one dilemma.
#[test]
fn two_modes_are_one_dilemma() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    let melkotian = scenario
        .add_creature_from_oracle(P0, "Melkotian Adjudicator", 2, 1, MELKOTIAN_ADJUDICATOR)
        .id();
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Atarka's Command", true, ATARKAS_COMMAND)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let p1_life = runner.life(P1);
    cast(&mut runner, command);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1, 3]],
            ..Plan::default()
        },
    );
    assert_eq!(runner.life(P1), p1_life - 3, "REACH: the command resolved");
    assert_eq!(
        hand_len(&runner, P0),
        hand - 1 + 1,
        "Seven drew exactly one"
    );
    assert_eq!(plus_counters(&runner, melkotian), 1);
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R4a — CR 602.2b + CR 700.2a: an activated ability's mode choice faces a
/// dilemma; the history is written when the ability reaches the stack.
#[test]
fn activated_mode_choice_faces_a_dilemma() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    let cankerbloom = scenario
        .add_creature_from_oracle(P0, "Cankerbloom", 3, 2, CANKERBLOOM)
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cankerbloom,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Cankerbloom");
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![2]],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P0]);
    assert_eq!(
        runner.state().objects[&cankerbloom].zone,
        Zone::Graveyard,
        "REACH: the sacrifice cost was paid"
    );
    assert_eq!(hand_len(&runner, P0), hand + 1, "Seven drew one");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R4c — CR 603.2 + CR 602.2b: the dilemma is faced at the mode choice, before
/// the cost sacrifices Seven of Nine (P0's only artifact, so the sacrifice
/// prompt offers only Seven), and Seven's trigger still fires: Seven was on
/// the battlefield when the event happened.
#[test]
fn trigger_source_sacrificed_as_the_cost_still_triggers() {
    let mut scenario = board();
    let seven = add_seven(&mut scenario, P0);
    let lobelia = scenario
        .add_creature_from_oracle(P0, "Lobelia, Defender of Bag End", 2, 2, LOBELIA)
        .id();
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let lives = (runner.life(P0), runner.life(P1));
    runner
        .act(GameAction::ActivateAbility {
            source_id: lobelia,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Lobelia");
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(record.cost_choices, vec![(P0, vec![seven])]);
    assert_eq!(runner.state().objects[&seven].zone, Zone::Graveyard);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (lives.0 + 2, lives.1 - 2),
        "REACH: the mode resolved"
    );
    assert_eq!(hand_len(&runner, P0), hand + 1, "Seven's draw resolved");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R5 — CR 603.3c + CR 700.2b: a modal triggered ability's mode choice faces a
/// dilemma.
#[test]
fn triggered_mode_choice_faces_a_dilemma() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    let prince = scenario
        .add_creature_to_hand_from_oracle(P0, "Charming Prince", 2, 2, CHARMING_PRINCE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    cast(&mut runner, prince);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P0]);
    assert_eq!(runner.life(P0), life + 3, "REACH: the mode resolved");
    assert_eq!(hand_len(&runner, P0), hand - 1 + 1, "Seven drew one");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R7 + R15 — "whenever YOU face a dilemma": P0's choice fires P0's Elim
/// Garak and Obsidian Order Operative, never P1's copies.
#[test]
fn only_the_choosers_dilemma_abilities_trigger() {
    let mut scenario = board();
    scenario.add_creature_from_oracle(P0, "Elim Garak, Simple Tailor", 3, 3, ELIM_GARAK);
    scenario.add_creature_from_oracle(
        P0,
        "Obsidian Order Operative",
        2,
        5,
        OBSIDIAN_ORDER_OPERATIVE,
    );
    scenario.add_creature_from_oracle(P1, "Elim Garak, Simple Tailor", 3, 3, ELIM_GARAK);
    let charm = add_abzan_charm(&mut scenario);
    let mut runner = scenario.build();
    let (p0, p1) = (runner.life(P0), runner.life(P1));
    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    // Charm: P0 −2. Garak: P1 −2, P0 +2. Operative: P1 −1, P0 +1. P1's Garak
    // does not fire, so P0 loses nothing more.
    assert_eq!(runner.life(P0), p0 - 2 + 2 + 1);
    assert_eq!(runner.life(P1), p1 - 2 - 1);
    assert_eq!(dilemmas(&runner, P0), 1);
    assert_eq!(dilemmas(&runner, P1), 0);
}

/// R8 — CR 700.2e: "An opponent chooses one" — the opponent who chooses faces
/// the dilemma, not the caster.
#[test]
fn the_mode_chooser_faces_the_dilemma_not_the_caster() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    scenario.add_creature_from_oracle(
        P1,
        "Obsidian Order Operative",
        2,
        5,
        OBSIDIAN_ORDER_OPERATIVE,
    );
    let lore = scenario
        .add_spell_to_hand_from_oracle(P0, "Fatal Lore", false, FATAL_LORE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let (p0, p1) = (runner.life(P0), runner.life(P1));
    cast(&mut runner, lore);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![0]],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P1]);
    assert_eq!(hand_len(&runner, P0), hand - 1 + 3, "only the mode drew");
    assert_eq!((runner.life(P0), runner.life(P1)), (p0 - 1, p1 + 1));
    assert_eq!(dilemmas(&runner, P0), 0);
    assert_eq!(dilemmas(&runner, P1), 1);
}

/// R9 — CR 707.10 + CR 700.2g: a copy copies the modes and faces no dilemma.
#[test]
fn a_copy_faces_no_dilemma() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Atarka's Command", true, ATARKAS_COMMAND)
        .with_mana_cost(ManaCost::zero())
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let p1 = runner.life(P1);
    cast(&mut runner, command);
    runner
        .act(GameAction::SelectModes {
            indices: vec![1, 3],
        })
        .expect("choose the command's modes");
    cast(&mut runner, twincast);
    drive(
        &mut runner,
        Plan {
            targets: vec![TargetRef::Object(command)],
            optional: vec![false],
            ..Plan::default()
        },
    );
    assert_eq!(runner.life(P1), p1 - 6, "REACH: the copy resolved");
    assert_eq!(hand_len(&runner, P0), hand - 2 + 1, "Seven drew once");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R10 — CR 733.1: a cast cancelled after the mode choice faces no dilemma
/// that is recorded, and its trigger never reaches the stack.
#[test]
fn a_cancelled_cast_records_no_dilemma() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    // Two legal exile targets, so the target prompt is raised.
    scenario.add_creature(P1, "P1 Giant", 4, 4);
    scenario.add_creature(P1, "P1 Second Giant", 4, 4);
    let charm = add_abzan_charm(&mut scenario);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    cast(&mut runner, charm);
    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("choose the exile mode");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "REACH: the exile mode asks for its target: {:?}",
        runner.state().waiting_for
    );
    runner.act(GameAction::CancelCast).expect("cancel the cast");
    drive(&mut runner, Plan::default());
    assert_eq!(runner.state().objects[&charm].zone, Zone::Hand);
    assert_eq!(hand_len(&runner, P0), hand, "Seven never drew");
    assert_eq!(dilemmas(&runner, P0), 0);

    // Positive: recast with the draw mode faces the dilemma.
    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R12 — CR 113.6m: Gabriel Lorca's ability works from the graveyard; paying
/// {B} returns it to its owner's hand.
#[test]
fn lorca_returns_from_the_graveyard_when_paid() {
    for accept in [true, false] {
        let mut scenario = board();
        let lorca = scenario
            .add_creature_to_graveyard(P0, "Gabriel Lorca, Mirror Menace", 5, 4)
            .from_oracle_text(GABRIEL_LORCA)
            .id();
        scenario.add_basic_land(P0, ManaColor::Black);
        let charm = add_abzan_charm(&mut scenario);
        let mut runner = scenario.build();
        cast(&mut runner, charm);
        let record = drive(
            &mut runner,
            Plan {
                modes: vec![vec![1]],
                optional: vec![accept],
                ..Plan::default()
            },
        );
        assert_eq!(record.optional_actors, vec![P0], "accept {accept}");
        let zone = runner
            .state()
            .objects
            .values()
            .find(|object| object.name == "Gabriel Lorca, Mirror Menace")
            .map(|object| object.zone);
        let expected = if accept { Zone::Hand } else { Zone::Graveyard };
        assert_eq!(zone, Some(expected), "accept {accept} ({lorca:?})");
    }
}

/// R13 — the per-turn history clears at the start of the next turn (the
/// card-level reading is Captain Benjamin Sisko's, phase 4).
#[test]
fn the_dilemma_history_resets_each_turn() {
    let mut scenario = board();
    let charm = add_abzan_charm(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(dilemmas(&runner, P0), 1, "REACH");
    runner.advance_to_end_step();
    drive(&mut runner, Plan::default());
    runner.pass_both_players();
    for _ in 0..16 {
        if runner.state().active_player == P1 {
            break;
        }
        runner.pass_both_players();
    }
    assert_eq!(runner.state().active_player, P1, "REACH: P1's turn began");
    assert_eq!(dilemmas(&runner, P0), 0);
}

/// R14 + R16 — Jett Reno and Melkotian Adjudicator each get one +1/+1 counter
/// for one dilemma.
#[test]
fn counter_creatures_grow_once_per_dilemma() {
    let mut scenario = board();
    let jett = scenario
        .add_creature_from_oracle(P0, "Jett Reno, Problem Solver", 1, 2, JETT_RENO)
        .id();
    let melkotian = scenario
        .add_creature_from_oracle(P0, "Melkotian Adjudicator", 2, 1, MELKOTIAN_ADJUDICATOR)
        .id();
    let charm = add_abzan_charm(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(plus_counters(&runner, jett), 1);
    assert_eq!(plus_counters(&runner, melkotian), 1);
}

/// R17 — modes drawn at random are not chosen by a player: no dilemma (engine
/// policy, needs manual verification).
#[test]
fn random_modes_face_no_dilemma() {
    let mut scenario = board();
    let cult = scenario
        .add_creature_from_oracle(P0, "Cult of Skaro", 4, 4, CULT_OF_SKARO)
        .id();
    let seven = add_seven(&mut scenario, P0);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let p1 = runner.life(P1);
    let tokens = runner.state().battlefield.len();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(cult, engine::game::combat::AttackTarget::Player(P1))])
        .expect("Cult of Skaro attacks");
    drive(&mut runner, Plan::default());
    // Thay counters the artifact creature (Seven); Caan draws two; Sec makes
    // a Dalek; Jast drains each opponent.
    let resolved_a_mode = plus_counters(&runner, seven) > 0
        || hand_len(&runner, P0) == hand + 2
        || runner.state().battlefield.len() > tokens
        || runner.life(P1) == p1 - 4;
    assert!(resolved_a_mode, "REACH: one random mode resolved");
    assert_eq!(dilemmas(&runner, P0), 0);
    assert!(
        hand_len(&runner, P0) == hand || hand_len(&runner, P0) == hand + 2,
        "Seven never drew"
    );
}

/// R21 — Tell Me What You See: "Whenever you face a dilemma, you may return
/// this enchantment to its owner's hand."
#[test]
fn tell_me_what_you_see_may_return_to_hand() {
    for accept in [true, false] {
        let mut scenario = board();
        let tell_me = scenario
            .add_enchantment_from_oracle(P0, "Tell Me What You See", TELL_ME)
            .id();
        let charm = add_abzan_charm(&mut scenario);
        let mut runner = scenario.build();
        cast(&mut runner, charm);
        let record = drive(
            &mut runner,
            Plan {
                modes: vec![vec![1]],
                optional: vec![accept],
                ..Plan::default()
            },
        );
        assert_eq!(record.optional_actors, vec![P0]);
        let expected = if accept {
            Zone::Hand
        } else {
            Zone::Battlefield
        };
        let zone = runner
            .state()
            .objects
            .values()
            .find(|object| object.name == "Tell Me What You See")
            .map(|object| object.zone);
        assert_eq!(zone, Some(expected), "accept {accept} ({tell_me:?})");
    }
}

/// R5b — "as you choose one or more modes": choosing zero modes for "Choose up
/// to two" faces no dilemma. Positive: the same turn's Abzan Charm does.
#[test]
fn zero_modes_face_no_dilemma() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    let call = scenario
        .add_spell_to_hand_from_oracle(P0, "Call Damage Control", false, CALL_DAMAGE_CONTROL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let charm = add_abzan_charm(&mut scenario);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    cast(&mut runner, call);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![]],
            ..Plan::default()
        },
    );
    assert_eq!(
        record.mode_choosers,
        vec![P0],
        "REACH: the mode prompt was raised"
    );
    assert_eq!(hand_len(&runner, P0), hand - 1, "Seven did not draw");
    assert_eq!(dilemmas(&runner, P0), 0);

    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R18 — a modal spell cast during another ability's resolution (Chaos Wand's
/// free cast) faces one dilemma, recorded once. Measured: this cast's events do
/// not pass through the resolution's action scan, so the scan exclusion
/// (`PlayerActionKind::turn_history_recorded_by_chain_scan`) is pinned by its
/// own unit test in `phase-ast`; this row pins the end-to-end count.
#[test]
fn a_free_cast_during_resolution_records_one_dilemma() {
    let mut scenario = board();
    add_seven(&mut scenario, P0);
    let wand = scenario
        .add_artifact_from_oracle(P0, "Chaos Wand", CHAOS_WAND)
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    scenario
        .add_spell_to_library_top(P1, "Abzan Charm", true)
        .from_oracle_text(ABZAN_CHARM);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: wand,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Chaos Wand");
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            optional: vec![true],
            ..Plan::default()
        },
    );
    assert_eq!(
        record.mode_choosers,
        vec![P0],
        "REACH: the free cast chose modes"
    );
    assert_eq!(runner.life(P0), life - 2, "REACH: the charm resolved");
    assert_eq!(
        hand_len(&runner, P0),
        hand + 2 + 1,
        "the charm drew two, Seven one"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}
